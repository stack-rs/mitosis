use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use futures::StreamExt;
use redis::aio::{MultiplexedConnection, PubSub};
use redis::AsyncCommands;
use reqwest::{Client, StatusCode};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use url::Url;
use uuid::Uuid;

use crate::config::TracingGuard;
use crate::entity::content::ArtifactContentType;
use crate::entity::state::TaskExecState;
use crate::error::RequestError;
use crate::schema::*;
use crate::executor::{execute_task, CoordinatorClient, TaskExecutor};
use crate::{
    config::{WorkerConfig, WorkerConfigCli},
    error::{Error, ErrorMsg},
    schema::{RegisterWorkerReq, RegisterWorkerResp},
    service::auth::cred::get_user_credential,
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

/// Worker impl of [`CoordinatorClient`]: talks to the `workers/*` endpoints with
/// the worker credential, and optionally to redis for live state.
pub(crate) struct WorkerCoordinatorClient {
    pub(crate) task_client: Client,
    pub(crate) task_credential: String,
    pub(crate) task_url: Url,
    pub(crate) task_cancel_token: CancellationToken,
    pub(crate) coordinator_force_exit: Arc<AtomicBool>,
    pub(crate) polling_interval: std::time::Duration,
    pub(crate) task_redis_conn: Option<MultiplexedConnection>,
    pub(crate) task_redis_pubsub: Option<PubSub>,
}

impl WorkerCoordinatorClient {
    async fn set_task_state_ex(&mut self, uuid: &Uuid, state: i32, ex: u64) {
        if let Some(ref mut conn) = self.task_redis_conn {
            tracing::trace!("Set task state: {} -> {}", uuid, state);
            let _: Result<String, _> = conn.set_ex(format!("task:{uuid}"), state, ex).await;
        }
    }

    async fn set_task_state(&mut self, uuid: &Uuid, state: i32) {
        if let Some(ref mut conn) = self.task_redis_conn {
            tracing::trace!("Set task state: {} -> {}", uuid, state);
            let _: Result<String, _> = conn.set(format!("task:{uuid}"), state).await;
        }
    }

    async fn get_task_state(&mut self, uuid: &Uuid) -> Option<TaskExecState> {
        if let Some(ref mut conn) = self.task_redis_conn {
            tracing::trace!("Get task state: {}", uuid);
            let state: Result<i32, _> = conn.get(format!("task:{uuid}")).await;
            state.ok().map(TaskExecState::from)
        } else {
            None
        }
    }

    async fn publish_state(&mut self, uuid: &Uuid, state: i32) {
        if let Some(ref mut conn) = self.task_redis_conn {
            tracing::trace!("Publish task state: {} -> {}", uuid, state);
            let _: Result<i32, _> = conn.publish(format!("task:{uuid}"), state).await;
        }
    }

}

#[async_trait::async_trait]
impl CoordinatorClient for WorkerCoordinatorClient {
    /// POST a task report to the coordinator. Returns the presigned upload URL
    /// for an `Upload` op, `None` otherwise. Encapsulates the retry / cancel /
    /// auth-error handling shared by every report op — unifying the former
    /// `report_task` free function and the inline upload-presign path. The
    /// per-status nuances of those two paths are preserved (only `Upload`
    /// treats `403` as a skippable no-op and cancels on a hard error).
    async fn report(&mut self, id: i64, op: ReportTaskOp) -> crate::error::Result<Option<String>> {
        let is_upload = matches!(op, ReportTaskOp::Upload { .. });
        let req = ReportTaskReq { id, op };
        self.task_url.set_path("workers/tasks");
        loop {
            let resp = self
                .task_client
                .post(self.task_url.as_str())
                .json(&req)
                .bearer_auth(&self.task_credential)
                .send()
                .await;
            match resp {
                Ok(resp) => {
                    if resp.status().is_success() {
                        let resp = resp
                            .json::<ReportTaskResp>()
                            .await
                            .map_err(RequestError::from)?;
                        return Ok(resp.url);
                    } else if resp.status() == StatusCode::UNAUTHORIZED {
                        tracing::info!("Report task failed with coordinator force exit");
                        self.coordinator_force_exit
                            .store(true, std::sync::atomic::Ordering::Release);
                        self.task_cancel_token.cancel();
                        return Ok(None);
                    } else if resp.status() == StatusCode::NOT_FOUND {
                        tracing::debug!("Task not found, ignore and go on for next cycle");
                        return Ok(None);
                    } else if is_upload && resp.status() == StatusCode::FORBIDDEN {
                        let resp: ErrorMsg = resp
                            .json()
                            .await
                            .unwrap_or_else(|e| ErrorMsg { msg: e.to_string() });
                        tracing::info!(
                            "Request upload url failed with permission denied: {}",
                            resp.msg
                        );
                        return Ok(None);
                    } else {
                        let resp: ErrorMsg = resp
                            .json()
                            .await
                            .unwrap_or_else(|e| ErrorMsg { msg: e.to_string() });
                        // The upload-presign path cancels on a hard error; the
                        // plain report path does not. Preserve both.
                        if is_upload {
                            self.task_cancel_token.cancel();
                        }
                        return Err(Error::Custom(format!(
                            "Report task failed with error: {}",
                            resp.msg
                        )));
                    }
                }
                Err(e) => {
                    if e.is_connect() && e.is_request() {
                        tracing::error!(
                            "Report task failed with connection error: {}. Retry after {:?}",
                            e,
                            self.polling_interval
                        );
                        tokio::select! {
                            biased;
                            _ = self.task_cancel_token.cancelled() => return Ok(None),
                            _ = tokio::time::sleep(self.polling_interval) => {},
                        }
                        continue;
                    } else {
                        return Err(RequestError::from(e).into());
                    }
                }
            }
        }
    }

    fn artifact_download_req(
        &self,
        uuid: Uuid,
        content_type: ArtifactContentType,
    ) -> reqwest::RequestBuilder {
        let ct = serde_json::to_value(content_type)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_else(|| "result".to_owned());
        let mut url = self.task_url.clone();
        url.set_path(&format!("workers/tasks/{uuid}/artifacts/{ct}"));
        self.task_client.get(url).bearer_auth(&self.task_credential)
    }

    fn attachment_download_req(&self, task_uuid: &Uuid, key: &str) -> reqwest::RequestBuilder {
        let mut url = self.task_url.clone();
        url.set_path(&format!("workers/tasks/{task_uuid}/attachments/{key}"));
        self.task_client.get(url).bearer_auth(&self.task_credential)
    }

    async fn watch(&mut self, uuid: &Uuid, target: TaskExecState) {
        tracing::debug!("Watch task: {} -> {:?}", uuid, target);
        let mut wait_until = Instant::now();
        if let Some(pubsub) = self.task_redis_pubsub.as_mut() {
            let channel_name = format!("task:{uuid}");
            let _ = pubsub.subscribe(&channel_name).await;
            let mut stream = pubsub.on_message();
            loop {
                tokio::select! {
                    biased;
                    msg = stream.next() => {
                        if let Some(msg) = msg {
                            if msg.get_channel_name() == channel_name {
                                if let Ok(task_state) = msg.get_payload::<i32>() {
                                    let cur_state = TaskExecState::from(task_state);
                                    if cur_state.is_reach(&target) {
                                        break;
                                    }
                                }
                            }
                        }
                    },
                    _ = tokio::time::sleep_until(wait_until) => {
                        wait_until = Instant::now() + std::time::Duration::from_secs(30);
                        let cur_state = if let Some(ref mut conn) = self.task_redis_conn {
                            tracing::trace!("Get task state: {}", uuid);
                            let state: Result<i32, _> = conn.get(format!("task:{uuid}")).await;
                            state.ok().map(TaskExecState::from)
                        } else {
                            None
                        };
                        if let Some(cur_state) = cur_state {
                            if cur_state.is_reach(&target) {
                                break;
                            }
                        }
                        if let Some(task) = query_task(
                            &self.task_client,
                            &mut self.task_url,
                            &self.task_credential,
                            uuid,
                        )
                        .await
                        {
                            if task.info.state.is_reach(&target, task.info.result) {
                                break;
                            }
                        }
                    },
                }
            }
        } else {
            loop {
                tokio::time::sleep_until(wait_until).await;
                wait_until = Instant::now() + std::time::Duration::from_secs(30);
                if let Some(cur_state) = self.get_task_state(uuid).await {
                    if cur_state.is_reach(&target) {
                        break;
                    }
                }
                if let Some(task) = query_task(
                    &self.task_client,
                    &mut self.task_url,
                    &self.task_credential,
                    uuid,
                )
                .await
                {
                    if task.info.state.is_reach(&target, task.info.result) {
                        break;
                    }
                }
            }
        }
    }

    fn can_watch(&self) -> bool {
        self.task_redis_conn.is_some() && self.task_redis_pubsub.is_some()
    }

    async fn unsubscribe(&mut self, uuid: &Uuid) {
        if let Some(pubsub) = self.task_redis_pubsub.as_mut() {
            let _ = pubsub.unsubscribe(format!("task:{uuid}")).await;
        }
    }

    async fn announce_state(&mut self, uuid: &Uuid, state: i32, ex: Option<u64>) {
        match ex {
            Some(ex) => self.set_task_state_ex(uuid, state, ex).await,
            None => self.set_task_state(uuid, state).await,
        }
        self.publish_state(uuid, state).await;
    }
}

impl MitoWorker {
    pub async fn main(cli: WorkerConfigCli) {
        tracing_subscriber::registry()
            .with(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| "netmito=info".into()),
            )
            .with(tracing_subscriber::fmt::layer())
            .init();
        match WorkerConfig::new(&cli) {
            Ok(config) => match Self::setup(config).await {
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

    pub async fn setup(mut config: WorkerConfig) -> crate::error::Result<(Self, TracingGuard)> {
        tracing::debug!("Worker is setting up");
        let http_client = Client::new();
        let (_, credential) = get_user_credential(
            config.credential_path.as_ref(),
            &http_client,
            config.coordinator_addr.clone(),
            config.user.take(),
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

    pub async fn get_task_executor(&self) -> TaskExecutor {
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
        let client = WorkerCoordinatorClient {
            task_client: self.http_client.clone(),
            task_credential: self.credential.clone(),
            task_url: self.config.coordinator_addr.clone(),
            task_cancel_token: self.cancel_token.clone(),
            coordinator_force_exit: self.coordinator_force_exit.clone(),
            polling_interval: self.config.polling_interval,
            task_redis_conn,
            task_redis_pubsub,
        };
        TaskExecutor {
            task_cancel_token: self.cancel_token.clone(),
            coordinator_force_exit: self.coordinator_force_exit.clone(),
            polling_interval: self.config.polling_interval,
            task_cache_path: self.cache_path.clone(),
            http_client: self.http_client.clone(),
            client: Box::new(client),
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
        // The fetch loop is worker-specific (the agent batch-fetches from a suite
        // instead), so it keeps its own transport rather than going through the seam.
        let fetch_client = self.http_client.clone();
        let fetch_credential = self.credential.clone();
        let mut fetch_url = self.config.coordinator_addr.clone();
        let task_hd = tokio::spawn(async move {
            loop {
                if task_executor.task_cancel_token.is_cancelled() {
                    break;
                }
                fetch_url.set_path("workers/tasks");
                let resp = fetch_client
                    .get(fetch_url.as_str())
                    .bearer_auth(&fetch_credential)
                    .send()
                    .await;
                match resp {
                    Ok(resp) => {
                        if resp.status().is_success() {
                            match resp.json::<Option<WorkerTaskResp>>().await {
                                Ok(task) => match task {
                                    Some(task) => {
                                        match execute_task(task, &mut task_executor).await {
                                            Ok(_) => {}
                                            Err(e) => {
                                                tracing::error!("Task execution failed: {}", e);
                                                task_executor.task_cancel_token.cancel();
                                            }
                                        }
                                    }
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

async fn query_task(
    client: &Client,
    base_url: &mut Url,
    credential: &str,
    uuid: &Uuid,
) -> Option<TaskQueryResp> {
    base_url.set_path(format!("workers/tasks/{uuid}").as_str());
    match client
        .get(base_url.as_str())
        .bearer_auth(credential)
        .send()
        .await
    {
        Ok(resp) => {
            if resp.status().is_success() {
                resp.json::<TaskQueryResp>().await.ok()
            } else {
                let resp: ErrorMsg = resp
                    .json()
                    .await
                    .unwrap_or_else(|e| ErrorMsg { msg: e.to_string() });
                tracing::error!("Get Task failed with error: {}", resp.msg);
                None
            }
        }
        Err(e) => {
            tracing::error!("Get task failed with error: {}", e);
            None
        }
    }
}

