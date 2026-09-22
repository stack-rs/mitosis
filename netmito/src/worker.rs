use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use futures::StreamExt;
use redis::aio::MultiplexedConnection;
use redis::AsyncCommands;
use reqwest::{Client, StatusCode};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::layer::SubscriberExt;
use url::Url;
use uuid::Uuid;

use crate::config::TracingGuard;
use crate::entity::content::ArtifactContentType;
use crate::entity::state::TaskExecState;
use crate::error::RequestError;
use crate::executor::{ExecClient, Executor, UploadTarget};
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

/// The worker's half of the execution seam: one task, reported to
/// `POST /workers/tasks` with the worker's own credential, its exec state
/// announced over redis.
struct WorkerTaskClient {
    http_client: Client,
    credential: String,
    coordinator_addr: Url,
    cancel_token: CancellationToken,
    coordinator_force_exit: Arc<AtomicBool>,
    polling_interval: std::time::Duration,
    task_id: i64,
    task_uuid: Uuid,
    upstream_task_uuid: Option<Uuid>,
    /// A clone of the worker's single multiplexed connection, not a new one.
    redis_conn: Option<MultiplexedConnection>,
    /// Kept whole so `watch` can open its own pubsub connection for the wait and
    /// drop it afterwards.
    redis_client: Option<redis::Client>,
}

impl WorkerTaskClient {
    fn api_url(&self, path: &str) -> Url {
        let mut url = self.coordinator_addr.clone();
        url.set_path(path);
        url
    }

    /// POST one report, retrying connection failures. Returns the presigned
    /// upload URL for an `Upload` op, and `None` when the coordinator answered
    /// something that means "stop bothering with this task".
    async fn report(&mut self, op: ReportTaskOp) -> crate::error::Result<Option<ReportTaskResp>> {
        // Only an upload can legitimately be refused: the group's storage quota
        // is enforced there. Any other 403 is a real error.
        let upload = matches!(op, ReportTaskOp::Upload { .. });
        let req = ReportTaskReq {
            id: self.task_id,
            op,
        };
        let url = self.api_url("workers/tasks");
        loop {
            let resp = self
                .http_client
                .post(url.as_str())
                .json(&req)
                .bearer_auth(&self.credential)
                .send()
                .await;
            match resp {
                Ok(resp) => {
                    if resp.status().is_success() {
                        return resp
                            .json::<ReportTaskResp>()
                            .await
                            .map(Some)
                            .map_err(|e| RequestError::from(e).into());
                    } else if resp.status() == StatusCode::UNAUTHORIZED {
                        tracing::info!("Report task failed with coordinator force exit");
                        self.coordinator_force_exit
                            .store(true, std::sync::atomic::Ordering::Release);
                        self.cancel_token.cancel();
                        return Ok(None);
                    } else if resp.status() == StatusCode::NOT_FOUND {
                        tracing::debug!("Task not found, ignore and go on for next cycle");
                        return Ok(None);
                    } else if upload && resp.status() == StatusCode::FORBIDDEN {
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
                            _ = self.cancel_token.cancelled() => return Ok(None),
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

    async fn cached_state(&mut self) -> Option<TaskExecState> {
        let conn = self.redis_conn.as_mut()?;
        tracing::trace!("Get task state: {}", self.task_uuid);
        let state: Result<i32, _> = conn.get(format!("task:{}", self.task_uuid)).await;
        state.ok().map(TaskExecState::from)
    }

    /// Ask the coordinator directly whether the watched task has got there. The
    /// redis mirror can be missing or stale, so the poll is the source of truth.
    async fn poll_watched_task(&self, uuid: &Uuid, target: TaskExecState) -> bool {
        let resp = self
            .http_client
            .get(self.api_url(&format!("workers/tasks/{uuid}")).as_str())
            .bearer_auth(&self.credential)
            .send()
            .await;
        match resp {
            Ok(resp) => {
                if resp.status().is_success() {
                    match resp.json::<TaskQueryResp>().await {
                        Ok(task) => task.info.state.is_reach(&target, task.info.result),
                        Err(_) => false,
                    }
                } else {
                    let resp: ErrorMsg = resp
                        .json()
                        .await
                        .unwrap_or_else(|e| ErrorMsg { msg: e.to_string() });
                    tracing::error!("Get Task failed with error: {}", resp.msg);
                    false
                }
            }
            Err(e) => {
                tracing::error!("Get task failed with error: {}", e);
                false
            }
        }
    }
}

#[async_trait::async_trait]
impl ExecClient for WorkerTaskClient {
    fn describe(&self) -> String {
        format!("task {}", self.task_uuid)
    }

    fn exec_env(&self) -> Vec<(&'static str, String)> {
        let mut env = vec![("MITO_TASK_UUID", self.task_uuid.to_string())];
        if let Some(uuid) = self.upstream_task_uuid {
            env.push(("MITO_UPSTREAM_TASK_UUID", uuid.to_string()));
        }
        env
    }

    fn supports_child_tasks(&self) -> bool {
        true
    }

    async fn report_finish(
        &mut self,
        finished: bool,
        _result: &TaskResultSpec,
    ) -> crate::error::Result<()> {
        let op = if finished {
            ReportTaskOp::Finish
        } else {
            ReportTaskOp::Cancel
        };
        self.report(op).await.map(|_| ())
    }

    async fn request_upload(
        &mut self,
        content_type: ArtifactContentType,
        content_length: u64,
    ) -> crate::error::Result<UploadTarget> {
        match self
            .report(ReportTaskOp::Upload {
                content_type,
                content_length,
            })
            .await?
        {
            Some(resp) => Ok(match resp.url {
                Some(url) => UploadTarget::Url(url),
                None => UploadTarget::Skip,
            }),
            None => Ok(UploadTarget::Stop),
        }
    }

    async fn report_commit(&mut self, result: TaskResultSpec) -> crate::error::Result<()> {
        self.report(ReportTaskOp::Commit(result)).await.map(|_| ())
    }

    async fn submit_child_task(&mut self, req: SubmitTaskReq) -> crate::error::Result<()> {
        self.report(ReportTaskOp::Submit(Box::new(req)))
            .await
            .map(|_| ())
    }

    fn artifact_download_req(
        &self,
        uuid: Uuid,
        content_type: ArtifactContentType,
    ) -> reqwest::RequestBuilder {
        let content_type = serde_json::to_value(content_type)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_else(|| "result".to_string());
        self.http_client
            .get(
                self.api_url(&format!("workers/tasks/{uuid}/artifacts/{content_type}"))
                    .as_str(),
            )
            .bearer_auth(&self.credential)
    }

    fn attachment_download_req(&self, key: &str) -> reqwest::RequestBuilder {
        let uuid = self.task_uuid;
        self.http_client
            .get(
                self.api_url(&format!("workers/tasks/{uuid}/attachments/{key}"))
                    .as_str(),
            )
            .bearer_auth(&self.credential)
    }

    async fn announce_state(&mut self, state: TaskExecState, ex: Option<u64>) {
        let uuid = self.task_uuid;
        let state = state as i32;
        let Some(conn) = self.redis_conn.as_mut() else {
            return;
        };
        tracing::trace!("Set task state: {} -> {}", uuid, state);
        match ex {
            Some(ex) => {
                let _: Result<String, _> = conn.set_ex(format!("task:{uuid}"), state, ex).await;
            }
            None => {
                let _: Result<String, _> = conn.set(format!("task:{uuid}"), state).await;
            }
        }
        tracing::trace!("Publish task state: {} -> {}", uuid, state);
        let _: Result<i32, _> = conn.publish(format!("task:{uuid}"), state).await;
    }

    fn can_watch(&self) -> bool {
        self.redis_conn.is_some() && self.redis_client.is_some()
    }

    /// Subscribe to the watched task's channel and, in parallel, re-poll every
    /// 30s in case the notification was missed or predates the subscription.
    async fn watch(&mut self, uuid: &Uuid, target: TaskExecState) {
        tracing::debug!("Watch task: {} -> {:?}", uuid, target);
        let channel_name = format!("task:{uuid}");
        // Opened for this wait only; dropping it on the way out is what used to
        // need an explicit unsubscribe.
        let mut pubsub = match self.redis_client.as_ref() {
            Some(client) => client
                .get_async_pubsub()
                .await
                .inspect_err(|e| tracing::warn!("Cannot open a redis pubsub connection: {}", e))
                .ok(),
            None => None,
        };
        if let Some(pubsub) = pubsub.as_mut() {
            let _ = pubsub.subscribe(&channel_name).await;
        }
        let mut stream = pubsub.as_mut().map(|pubsub| pubsub.on_message());

        let mut wait_until = Instant::now();
        loop {
            let published = async {
                match stream.as_mut() {
                    Some(stream) => stream.next().await,
                    // No pubsub: fall through to the poll arm forever.
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                biased;
                msg = published => {
                    if let Some(msg) = msg {
                        if msg.get_channel_name() == channel_name {
                            if let Ok(state) = msg.get_payload::<i32>() {
                                if TaskExecState::from(state).is_reach(&target) {
                                    break;
                                }
                            }
                        }
                    }
                },
                _ = tokio::time::sleep_until(wait_until) => {
                    wait_until = Instant::now() + std::time::Duration::from_secs(30);
                    if let Some(state) = self.cached_state().await {
                        if state.is_reach(&target) {
                            break;
                        }
                    }
                    if self.poll_watched_task(uuid, target).await {
                        break;
                    }
                },
            }
        }
    }
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
            crate::executor::warn_unreachable_ancestors(&cache_path).await;
            cache_path.push("mitosis");
            crate::executor::ensure_traversable_dir(&cache_path).await?;
            let log_dir = cache_path.join("worker");
            cache_path.push(resp.worker_id.to_string());
            // Same tree `reset_workspace` rebuilds between tasks, and made the
            // same way: the first task must not be the one that runs against a
            // umask-restricted workspace.
            crate::executor::reset_workspace(&cache_path).await?;
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
        let mut fetcher = TaskFetcher {
            http_client: self.http_client.clone(),
            credential: self.credential.clone(),
            fetch_url: {
                let mut url = self.config.coordinator_addr.clone();
                url.set_path("workers/tasks");
                url
            },
            cancel_token: self.cancel_token.clone(),
            coordinator_force_exit: self.coordinator_force_exit.clone(),
            polling_interval: self.config.polling_interval,
        };
        let redis_conn = match self.redis_client {
            Some(ref client) => client
                .get_multiplexed_tokio_connection()
                .await
                .inspect_err(|e| tracing::warn!("{}", e))
                .ok(),
            None => None,
        };
        let coordinator_addr = self.config.coordinator_addr.clone();
        let redis_client = self.redis_client.clone();
        let cache_path = self.cache_path.clone();
        let http_client = self.http_client.clone();
        let credential = self.credential.clone();
        let polling_interval = self.config.polling_interval;
        let cancel_token = self.cancel_token.clone();
        let coordinator_force_exit = self.coordinator_force_exit.clone();
        let task_hd = tokio::spawn(async move {
            loop {
                let task = match fetcher.next_task().await {
                    FetchOutcome::Task(task) => *task,
                    FetchOutcome::Idle => continue,
                    FetchOutcome::Stop => break,
                };
                let mut executor = Executor {
                    cancel_token: cancel_token.clone(),
                    polling_interval,
                    cache_path: cache_path.clone(),
                    http_client: http_client.clone(),
                    client: Box::new(WorkerTaskClient {
                        http_client: http_client.clone(),
                        credential: credential.clone(),
                        coordinator_addr: coordinator_addr.clone(),
                        cancel_token: cancel_token.clone(),
                        coordinator_force_exit: coordinator_force_exit.clone(),
                        polling_interval,
                        task_id: task.id,
                        task_uuid: task.uuid,
                        upstream_task_uuid: task.upstream_task_uuid,
                        redis_conn: redis_conn.clone(),
                        redis_client: redis_client.clone(),
                    }),
                };
                if let Err(e) = executor
                    .execute(task.spec, task.exec_options.as_ref())
                    .await
                {
                    tracing::error!("Task execution failed: {}", e);
                    cancel_token.cancel();
                    break;
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

/// What one poll of `GET /workers/tasks` came back with.
enum FetchOutcome {
    Task(Box<WorkerTaskResp>),
    /// Nothing to run right now; the caller should poll again.
    Idle,
    /// The worker is done — shut down or told to exit.
    Stop,
}

/// The worker's own task source. Kept out of the execution seam on purpose:
/// pulling work is the one coordinator interaction a worker does *not* share
/// with the agent, which is handed its tasks a suite at a time.
struct TaskFetcher {
    http_client: Client,
    credential: String,
    fetch_url: Url,
    cancel_token: CancellationToken,
    coordinator_force_exit: Arc<AtomicBool>,
    polling_interval: std::time::Duration,
}

impl TaskFetcher {
    async fn next_task(&mut self) -> FetchOutcome {
        if self.cancel_token.is_cancelled() {
            return FetchOutcome::Stop;
        }
        let resp = self
            .http_client
            .get(self.fetch_url.as_str())
            .bearer_auth(&self.credential)
            .send()
            .await;
        match resp {
            Ok(resp) => {
                if resp.status().is_success() {
                    match resp.json::<Option<WorkerTaskResp>>().await {
                        Ok(Some(task)) => FetchOutcome::Task(Box::new(task)),
                        Ok(None) => {
                            tracing::debug!(
                                "No task fetched. Next fetch after {:?}",
                                self.polling_interval
                            );
                            self.backoff().await
                        }
                        Err(e) => {
                            tracing::error!("Failed to parse task specification: {}", e);
                            self.cancel_token.cancel();
                            FetchOutcome::Stop
                        }
                    }
                } else if resp.status() == StatusCode::UNAUTHORIZED {
                    tracing::info!("Task fetch failed with coordinator force exit");
                    self.coordinator_force_exit
                        .store(true, std::sync::atomic::Ordering::Release);
                    self.cancel_token.cancel();
                    FetchOutcome::Stop
                } else {
                    let resp: ErrorMsg = resp
                        .json()
                        .await
                        .unwrap_or_else(|e| ErrorMsg { msg: e.to_string() });
                    tracing::error!("Task fetch failed with error: {}", resp.msg);
                    self.cancel_token.cancel();
                    FetchOutcome::Stop
                }
            }
            Err(e) => {
                if e.is_connect() && e.is_request() {
                    tracing::error!(
                        "Fetching task failed with connection error: {}. Retry after {:?}",
                        e,
                        self.polling_interval
                    );
                    self.backoff().await
                } else {
                    tracing::error!("Fetching task failed with error: {}", e);
                    self.cancel_token.cancel();
                    FetchOutcome::Stop
                }
            }
        }
    }

    async fn backoff(&self) -> FetchOutcome {
        tokio::select! {
            biased;
            _ = self.cancel_token.cancelled() => FetchOutcome::Stop,
            _ = tokio::time::sleep(self.polling_interval) => FetchOutcome::Idle,
        }
    }
}
