use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use reqwest::{Client, StatusCode};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::layer::SubscriberExt;

use crate::config::TracingGuard;
use crate::error::RequestError;
use crate::executor::{Executor, RESOURCE_FETCH_BUDGET, SINGLE_DOWNLOAD_BUDGET, UPLOAD_BUDGET};
use crate::schema::*;
use crate::service::auth::get_and_prompt_username;
use crate::{
    config::{WorkerConfig, WorkerConfigCli},
    error::{Error, ErrorMsg},
    schema::{RegisterWorkerReq, RegisterWorkerResp},
    service::auth::{cred::get_user_credential, credential_guard::CredentialGuard},
    signal::shutdown_signal,
};

pub struct MitoWorker {
    config: WorkerConfig,
    http_client: Client,
    credential: String,
    cancel_token: CancellationToken,
    coordinator_force_exit: Arc<AtomicBool>,
    cache_path: PathBuf,
    redis_client: Option<redis::Client>,
}

impl MitoWorker {
    pub async fn main(cli: WorkerConfigCli) {
        // `set_default` rather than `init`: this only has to cover config
        // loading and registration. The real subscriber needs the worker id the
        // coordinator hands back, so `setup` installs it and drops this one.
        let bootstrap = tracing::dispatcher::set_default(&tracing::Dispatch::new(
            tracing_subscriber::registry()
                .with(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| "netmito=info".into()),
                )
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_file(true)
                        .with_line_number(true),
                ),
        ));
        match WorkerConfig::new(&cli) {
            Ok(config) => match Self::setup(config, bootstrap).await {
                Ok((mut worker, _guards)) => {
                    if let Err(e) = worker.run().await {
                        tracing::error!("{}", e);
                    }
                    worker.cleanup().await;
                }
                Err(e) => {
                    tracing::error!("{}", e);
                }
            },
            Err(e) => {
                tracing::error!("{}", e);
            }
        }
    }

    /// `bootstrap` is `main`'s temporary subscriber, released as soon as the
    /// real one is installed below. Holding it across the awaits before that is
    /// sound only because the worker runs on a current-thread runtime — a
    /// `DefaultGuard` is thread-local.
    pub async fn setup(
        mut config: WorkerConfig,
        bootstrap: tracing::subscriber::DefaultGuard,
    ) -> crate::error::Result<(Self, TracingGuard)> {
        tracing::debug!("Worker is setting up");
        let http_client = Client::new();
        let mut credential_guard = CredentialGuard::new(
            config
                .credential_path
                .as_ref()
                .map(|credential_path| credential_path.relative()),
            &config.coordinator_addr,
        )
        .await;
        let username = match &config.user {
            Some(name) => name.to_string(),
            None => get_and_prompt_username(None, "Please input username")?,
        };
        let (_, credential) = get_user_credential(
            &mut credential_guard,
            &http_client,
            config.coordinator_addr.clone(),
            username,
            config.password.take(),
            false,
        )
        .await?;
        let mut url = config.coordinator_addr.clone();
        url.set_path("workers");
        let req = RegisterWorkerReq {
            tags: config.tags.clone(),
            labels: config.labels.clone(),
            groups: config.groups.clone(),
            lifetime: config.lifetime,
        };
        let resp = http_client
            .post(url.as_str())
            .json(&req)
            .bearer_auth(&credential)
            .send()
            .await
            .map_err(|e| {
                if e.is_request() && e.is_connect() {
                    url.set_path("");
                    RequestError::ConnectionError(url.to_string())
                } else {
                    e.into()
                }
            })?;
        if resp.status().is_success() {
            let resp: RegisterWorkerResp = resp.json().await.map_err(RequestError::from)?;
            let mut cache_path =
                dirs::cache_dir().ok_or(Error::Custom("Cache dir not found".to_string()))?;
            cache_path.push("mitosis");
            let log_dir = cache_path.join("worker");
            cache_path.push(resp.worker_id.to_string());
            tokio::fs::create_dir_all(&cache_path).await?;
            tokio::fs::create_dir_all(&cache_path.join("result")).await?;
            tokio::fs::create_dir_all(&cache_path.join("exec")).await?;
            tokio::fs::create_dir_all(&cache_path.join("resource")).await?;
            tokio::fs::create_dir_all(&log_dir).await?;
            let guards = config.setup_tracing_subscriber::<&uuid::Uuid, _>(&resp.worker_id)?;
            // The worker's own subscriber is live now; stop shadowing it.
            drop(bootstrap);
            let redis_client = if config.skip_redis {
                None
            } else {
                resp.redis_url.and_then(|url| {
                    redis::Client::open(url)
                        .inspect_err(|e| tracing::warn!("Worker cannot setup redis conn: {}", e))
                        .ok()
                })
            };
            tracing::info!("Worker registered with ID: {}", resp.worker_id);
            Ok((
                MitoWorker {
                    config,
                    http_client,
                    credential: resp.token,
                    cancel_token: CancellationToken::new(),
                    coordinator_force_exit: Arc::new(AtomicBool::new(false)),
                    cache_path,
                    redis_client,
                },
                guards,
            ))
        } else {
            let resp: crate::error::ErrorMsg = resp.json().await.map_err(RequestError::from)?;
            tracing::error!("{}", resp.msg);
            Err(Error::Custom(resp.msg))
        }
    }

    pub async fn get_task_executor(&self) -> Executor {
        let task_redis_conn = if let Some(ref client) = self.redis_client {
            client
                .get_multiplexed_tokio_connection()
                .await
                .inspect_err(|e| tracing::warn!("{}", e))
                .ok()
        } else {
            None
        };
        let task_redis_pubsub = if let Some(ref client) = self.redis_client {
            client
                .get_async_pubsub()
                .await
                .inspect_err(|e| tracing::warn!("{}", e))
                .ok()
        } else {
            None
        };
        Executor {
            http_client: self.http_client.clone(),
            task_credential: self.credential.clone(),
            http_url: self.config.coordinator_addr.clone(),
            task_cancel_token: self.cancel_token.clone(),
            coordinator_force_exit: self.coordinator_force_exit.clone(),
            polling_interval: self.config.polling_interval,
            task_cache_path: self.cache_path.clone(),
            task_redis_conn,
            task_redis_pubsub,
            resource_fetch_budget: RESOURCE_FETCH_BUDGET,
            single_download_budget: SINGLE_DOWNLOAD_BUDGET,
            upload_budget: UPLOAD_BUDGET,
        }
    }

    pub async fn run(&mut self) -> crate::error::Result<()> {
        tracing::info!("Worker is running");
        let mut heartbeat_url = self.config.coordinator_addr.clone();
        heartbeat_url.set_path("workers/heartbeat");
        let heartbeat_client = self.http_client.clone();
        let heartbeat_credential = self.credential.clone();
        let heartbeat_cancel_token = self.cancel_token.clone();
        let heartbeat_coordinator_force_exit = self.coordinator_force_exit.clone();
        let heartbeat_interval = self.config.heartbeat_interval;
        let heartbeat_hd = tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = heartbeat_cancel_token.cancelled() => {
                        break;
                    }
                    _ = tokio::time::sleep(heartbeat_interval) => {
                        let resp = heartbeat_client
                            .post(heartbeat_url.as_str())
                            .bearer_auth(&heartbeat_credential)
                            .send()
                            .await;
                        match resp {
                            Ok(resp) => {
                                if resp.status().is_success() {
                                    tracing::debug!("Heartbeat success");
                                } else if resp.status() == StatusCode::UNAUTHORIZED {
                                    tracing::info!("Heartbeat failed with coordinator force exit");
                                    heartbeat_coordinator_force_exit.store(true, std::sync::atomic::Ordering::Release);
                                    heartbeat_cancel_token.cancel();
                                    break;
                                } else {
                                    let resp: ErrorMsg = resp
                                        .json()
                                        .await
                                        .unwrap_or_else(|e| ErrorMsg { msg: e.to_string() });
                                    tracing::error!("Heartbeat failed with error: {}", resp.msg);
                                    heartbeat_cancel_token.cancel();
                                    break;
                                }
                            }
                            Err(e) => {
                                if e.is_connect() && e.is_request() {
                                    tracing::error!("Heartbeat failed with connection error: {}", e);
                                    continue;
                                } else {
                                    tracing::error!("Heartbeat failed with error: {}", e);
                                    heartbeat_cancel_token.cancel();
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        });
        let mut task_executor = self.get_task_executor().await;
        let task_hd = tokio::spawn(async move {
            loop {
                if task_executor.task_cancel_token.is_cancelled() {
                    break;
                }
                task_executor.http_url.set_path("workers/tasks");
                let resp = task_executor
                    .http_client
                    .get(task_executor.http_url.as_str())
                    .bearer_auth(&task_executor.task_credential)
                    .send()
                    .await;
                match resp {
                    Ok(resp) => {
                        if resp.status().is_success() {
                            match resp.json::<Option<WorkerTaskResp>>().await {
                                Ok(task) => match task {
                                    Some(task) => match task_executor.execute(task).await {
                                        Ok(_) => {}
                                        Err(e) => {
                                            tracing::error!("Task execution failed: {}", e);
                                            task_executor.task_cancel_token.cancel();
                                        }
                                    },
                                    None => {
                                        tracing::debug!(
                                            "No task fetched. Next fetch after {:?}",
                                            task_executor.polling_interval
                                        );
                                        tokio::select! {
                                            biased;
                                            _ = task_executor.task_cancel_token.cancelled() => break,
                                            _ = tokio::time::sleep(task_executor.polling_interval) => {},
                                        }
                                    }
                                },
                                Err(e) => {
                                    tracing::error!("Failed to parse task specification: {}", e);
                                    task_executor.task_cancel_token.cancel();
                                    break;
                                }
                            }
                        } else if resp.status() == StatusCode::UNAUTHORIZED {
                            tracing::info!("Task fetch failed with coordinator force exit");
                            task_executor
                                .coordinator_force_exit
                                .store(true, std::sync::atomic::Ordering::Release);
                            task_executor.task_cancel_token.cancel();
                            break;
                        } else {
                            let resp: ErrorMsg = resp
                                .json()
                                .await
                                .unwrap_or_else(|e| ErrorMsg { msg: e.to_string() });
                            tracing::error!("Task fetch failed with error: {}", resp.msg);
                            task_executor.task_cancel_token.cancel();
                            break;
                        }
                    }
                    Err(e) => {
                        if e.is_connect() && e.is_request() {
                            tracing::error!(
                                "Fetching task failed with connection error: {}. Retry after {:?}",
                                e,
                                task_executor.polling_interval
                            );
                            tokio::select! {
                                biased;
                                _ = task_executor.task_cancel_token.cancelled() => break,
                                _ = tokio::time::sleep(task_executor.polling_interval) => {},
                            }
                            continue;
                        } else {
                            tracing::error!("Fetching task failed with error: {}", e);
                            task_executor.task_cancel_token.cancel();
                            break;
                        }
                    }
                }
            }
        });
        tokio::select! {
            biased;
            _ = shutdown_signal(self.cancel_token.clone()) => {
                tracing::info!("Worker exits due to terminate signal received. Wait for resource cleanup");
                self.cancel_token.cancel();
                heartbeat_hd.await?;
                task_hd.await?;
            },
            _ = self.cancel_token.cancelled() => {
                if self.coordinator_force_exit.load(std::sync::atomic::Ordering::Acquire) {
                    tracing::info!("Worker exits due to coordinator force exit. Wait for resource cleanup");
                } else {
                    tracing::info!("Worker exits due to internal execution error. Wait for resource cleanup");
                }
                heartbeat_hd.await?;
                task_hd.await?;
            },
        }
        Ok(())
    }

    pub async fn cleanup(&self) {
        tracing::debug!("Worker is cleaning up.");
        let mut url = self.config.coordinator_addr.clone();
        url.set_path("workers");
        let _ = self
            .http_client
            .delete(url.as_str())
            .bearer_auth(&self.credential)
            .timeout(std::time::Duration::from_secs(3))
            .send()
            .await;
        let _ = tokio::fs::remove_dir_all(&self.cache_path).await;
    }
}
