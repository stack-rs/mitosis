use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use async_compression::tokio::write::GzipEncoder;
use futures::StreamExt;
use nix::sys::signal::{self, Signal};
use nix::unistd::Pid;
use redis::aio::{MultiplexedConnection, PubSub};
use redis::AsyncCommands;
use reqwest::header::CONTENT_LENGTH;
use reqwest::{Client, StatusCode};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_tar::{Builder, Header};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use url::Url;
use uuid::Uuid;

use crate::config::TracingGuard;
use crate::entity::content::ArtifactContentType;
use crate::entity::state::TaskExecState;
use crate::error::RequestError;
use crate::schema::*;
use crate::service::s3::download_file;
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

pub struct TaskExecutor {
    pub(crate) task_client: Client,
    pub(crate) task_credential: String,
    pub(crate) task_url: Url,
    pub(crate) task_cancel_token: CancellationToken,
    pub(crate) coordinator_force_exit: Arc<AtomicBool>,
    pub(crate) polling_interval: std::time::Duration,
    pub(crate) task_cache_path: PathBuf,
    pub(crate) task_redis_conn: Option<MultiplexedConnection>,
    pub(crate) task_redis_pubsub: Option<PubSub>,
}

impl TaskExecutor {
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

    async fn announce_task_state(&mut self, uuid: &Uuid, state: i32) {
        self.set_task_state(uuid, state).await;
        self.publish_state(uuid, state).await;
    }

    async fn announce_task_state_ex(&mut self, uuid: &Uuid, state: i32, ex: u64) {
        self.set_task_state_ex(uuid, state, ex).await;
        self.publish_state(uuid, state).await;
    }

    async fn watch_task(&mut self, uuid: &Uuid, state: TaskExecState) {
        tracing::debug!("Watch task: {} -> {:?}", uuid, state);
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
                                    if cur_state.is_reach(&state) {
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
                            if cur_state.is_reach(&state) {
                                break;
                            }
                        }
                        self.task_url
                            .set_path(format!("workers/tasks/{uuid}").as_str());
                        let resp = self
                            .task_client
                            .get(self.task_url.as_str())
                            .bearer_auth(&self.task_credential)
                            .send()
                            .await;
                        match resp {
                            Ok(resp) => {
                                if resp.status().is_success() {
                                    if let Ok(task) = resp.json::<TaskQueryResp>().await {
                                        if task.info.state.is_reach(&state, task.info.result) {
                                            break;
                                        }
                                    }
                                } else {
                                    let resp: ErrorMsg = resp
                                        .json()
                                        .await
                                        .unwrap_or_else(|e| ErrorMsg { msg: e.to_string() });
                                    tracing::error!("Get Task failed with error: {}", resp.msg);
                                }
                            }
                            Err(e) => {
                                tracing::error!("Get task failed with error: {}", e);
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
                    if cur_state.is_reach(&state) {
                        break;
                    }
                }
                self.task_url
                    .set_path(format!("workers/tasks/{uuid}").as_str());
                let resp = self
                    .task_client
                    .get(self.task_url.as_str())
                    .bearer_auth(&self.task_credential)
                    .send()
                    .await;
                match resp {
                    Ok(resp) => {
                        if resp.status().is_success() {
                            if let Ok(task) = resp.json::<TaskQueryResp>().await {
                                if task.info.state.is_reach(&state, task.info.result) {
                                    break;
                                }
                            }
                        } else {
                            let resp: ErrorMsg = resp
                                .json()
                                .await
                                .unwrap_or_else(|e| ErrorMsg { msg: e.to_string() });
                            tracing::error!("Get Task failed with error: {}", resp.msg);
                        }
                    }
                    Err(e) => {
                        tracing::error!("Get task failed with error: {}", e);
                    }
                }
            }
        }
    }

    pub async fn subscribe_task_exec_state(&mut self, uuid: &Uuid) {
        if let Some(pubsub) = self.task_redis_pubsub.as_mut() {
            let _ = pubsub.subscribe(format!("task:{uuid}")).await;
        }
    }

    pub async fn unsubscribe_task_exec_state(&mut self, uuid: &Uuid) {
        if let Some(pubsub) = self.task_redis_pubsub.as_mut() {
            let _ = pubsub.unsubscribe(format!("task:{uuid}")).await;
        }
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
        TaskExecutor {
            task_client: self.http_client.clone(),
            task_credential: self.credential.clone(),
            task_url: self.config.coordinator_addr.clone(),
            task_cancel_token: self.cancel_token.clone(),
            coordinator_force_exit: self.coordinator_force_exit.clone(),
            polling_interval: self.config.polling_interval,
            task_cache_path: self.cache_path.clone(),
            task_redis_conn,
            task_redis_pubsub,
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
                task_executor.task_url.set_path("workers/tasks");
                let resp = task_executor
                    .task_client
                    .get(task_executor.task_url.as_str())
                    .bearer_auth(&task_executor.task_credential)
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

enum ProcessOutput {
    WithLog {
        stdout: Vec<u8>,
        stderr: Vec<u8>,
        exit_status: ExitStatus,
    },
    WithoutLog {
        exit_status: ExitStatus,
    },
}

impl ProcessOutput {
    fn get_exit_status(&self) -> ExitStatus {
        match self {
            ProcessOutput::WithLog { exit_status, .. } => *exit_status,
            ProcessOutput::WithoutLog { exit_status } => *exit_status,
        }
    }
}

enum TaskResult {
    Finish(ProcessOutput),
    Timeout(ProcessOutput),
}

impl TaskResult {
    fn state(&self) -> (bool, ExitStatus) {
        match self {
            TaskResult::Finish(output) => (true, output.get_exit_status()),
            TaskResult::Timeout(output) => (false, output.get_exit_status()),
        }
    }

    fn get_output(self) -> ProcessOutput {
        match self {
            TaskResult::Finish(output) => output,
            TaskResult::Timeout(output) => output,
        }
    }
}

async fn execute_task(
    task: WorkerTaskResp,
    task_executor: &mut TaskExecutor,
) -> crate::error::Result<()> {
    task_executor
        .announce_task_state_ex(&task.uuid, TaskExecState::FetchResource as i32, 360)
        .await;
    // Allow downloading resources for at most 30 minutes
    let timeout_until = tokio::time::Instant::now() + std::time::Duration::from_secs(1800);
    for resource in task.spec.resources {
        match resource.remote_file {
            RemoteResource::Artifact { uuid, content_type } => {
                let content_serde_val = serde_json::to_value(content_type)?;
                let content_serde_str = content_serde_val.as_str().unwrap_or("result");
                task_executor.task_url.set_path(&format!(
                    "workers/tasks/{uuid}/artifacts/{content_serde_str}"
                ));
            }
            RemoteResource::Attachment { key } => {
                let uuid = task.uuid;
                task_executor
                    .task_url
                    .set_path(&format!("workers/tasks/{uuid}/attachments/{key}"));
            }
        };
        let resp = loop {
            match task_executor
                .task_client
                .get(task_executor.task_url.as_str())
                .bearer_auth(&task_executor.task_credential)
                .send()
                .await
            {
                Ok(resp) => break resp,
                Err(e) => {
                    if e.is_connect() && e.is_request() {
                        tracing::error!(
                            "Fetch resource info failed with connection error: {}. Retry after {:?}",
                            e,
                            task_executor.polling_interval
                        );
                        tokio::select! {
                            biased;
                            _ = task_executor.task_cancel_token.cancelled() => return Ok(()),
                            _ = tokio::time::sleep(task_executor.polling_interval) => {},
                            _ = tokio::time::sleep_until(timeout_until) => {
                                tracing::debug!("Fetching resource timeout, commit this task as canceled");
                                let req = ReportTaskReq {
                                    id: task.id,
                                    op: ReportTaskOp::Cancel,
                                };
                                report_task(task_executor, req).await?;
                                let req = ReportTaskReq {
                                    id: task.id,
                                    op: ReportTaskOp::Commit(TaskResultSpec {
                                        exit_status: 0,
                                        msg: Some(TaskResultMessage::FetchResourceTimeout),
                                    }),
                                };
                                report_task(task_executor, req).await?;
                                task_executor
                                    .announce_task_state_ex(
                                        &task.uuid,
                                        TaskExecState::FetchResourceTimeout as i32,
                                        60,
                                    )
                                    .await;
                                task_executor
                                    .announce_task_state_ex(
                                        &task.uuid,
                                        TaskExecState::TaskCommitted as i32,
                                        60,
                                    )
                                    .await;
                                return Ok(());
                            }
                        }
                        continue;
                    } else {
                        task_executor
                            .announce_task_state_ex(
                                &task.uuid,
                                TaskExecState::FetchResourceError as i32,
                                60,
                            )
                            .await;
                        return Err(RequestError::from(e).into());
                    }
                }
            }
        };
        if resp.status().is_success() {
            let download_resp = resp
                .json::<RemoteResourceDownloadResp>()
                .await
                .map_err(RequestError::from)?;
            let local_path = task_executor
                .task_cache_path
                .join("resource")
                .join(resource.local_path);
            tokio::select! {
                biased;
                res = download_file(&task_executor.task_client, &download_resp, local_path, false) => {
                    if let Err(e) = res {
                        tracing::error!("Failed to download resource: {}", e);
                        let req = ReportTaskReq {
                            id: task.id,
                            op: ReportTaskOp::Cancel,
                        };
                        report_task(task_executor, req).await?;
                        let req = ReportTaskReq {
                            id: task.id,
                            op: ReportTaskOp::Commit(TaskResultSpec {
                                exit_status: 0,
                                msg: Some(TaskResultMessage::ResourceForbidden),
                            }),
                        };
                        report_task(task_executor, req).await?;
                        task_executor
                            .announce_task_state(
                                &task.uuid,
                                TaskExecState::FetchResourceForbidden as i32,
                            )
                            .await;
                        task_executor
                            .announce_task_state_ex(
                                &task.uuid,
                                TaskExecState::TaskCommitted as i32,
                                60,
                            )
                            .await;
                        return Ok(());
                    }
                }
                _ = task_executor.task_cancel_token.cancelled() => return Ok(()),
                _ = tokio::time::sleep(std::time::Duration::from_secs(120)) => {
                    tracing::debug!("Fetching resource timeout, commit this task as canceled");
                    let req = ReportTaskReq {
                        id: task.id,
                        op: ReportTaskOp::Cancel,
                    };
                    report_task(task_executor, req).await?;
                    let req = ReportTaskReq {
                        id: task.id,
                        op: ReportTaskOp::Commit(TaskResultSpec {
                            exit_status: 0,
                            msg: Some(TaskResultMessage::FetchResourceTimeout),
                        }),
                    };
                    report_task(task_executor, req).await?;
                    task_executor
                        .announce_task_state_ex(
                            &task.uuid,
                            TaskExecState::FetchResourceTimeout as i32,
                            60,
                        )
                        .await;
                    task_executor
                        .announce_task_state_ex(&task.uuid, TaskExecState::TaskCommitted as i32, 60)
                        .await;
                    return Ok(());
                }
                _ = tokio::time::sleep_until(timeout_until) => {
                    tracing::debug!("Fetching resource timeout, commit this task as canceled");
                    let req = ReportTaskReq {
                        id: task.id,
                        op: ReportTaskOp::Cancel,
                    };
                    report_task(task_executor, req).await?;
                    let req = ReportTaskReq {
                        id: task.id,
                        op: ReportTaskOp::Commit(TaskResultSpec {
                            exit_status: 0,
                            msg: Some(TaskResultMessage::FetchResourceTimeout),
                        }),
                    };
                    report_task(task_executor, req).await?;
                    task_executor
                        .announce_task_state_ex(
                            &task.uuid,
                            TaskExecState::FetchResourceTimeout as i32,
                            60,
                        )
                        .await;
                    task_executor
                        .announce_task_state_ex(&task.uuid, TaskExecState::TaskCommitted as i32, 60)
                        .await;
                    return Ok(());
                }
            }
        } else if resp.status() == StatusCode::NOT_FOUND {
            tracing::debug!("Resource not found, commit this task as canceled");
            let req = ReportTaskReq {
                id: task.id,
                op: ReportTaskOp::Cancel,
            };
            report_task(task_executor, req).await?;
            let req = ReportTaskReq {
                id: task.id,
                op: ReportTaskOp::Commit(TaskResultSpec {
                    exit_status: 0,
                    msg: Some(TaskResultMessage::ResourceNotFound),
                }),
            };
            report_task(task_executor, req).await?;
            task_executor
                .announce_task_state_ex(&task.uuid, TaskExecState::FetchResourceNotFound as i32, 60)
                .await;
            task_executor
                .announce_task_state_ex(&task.uuid, TaskExecState::TaskCommitted as i32, 60)
                .await;
            return Ok(());
        } else if resp.status() == StatusCode::FORBIDDEN {
            tracing::debug!("Resource is forbidden to be fetched, commit this task as canceled");
            let req = ReportTaskReq {
                id: task.id,
                op: ReportTaskOp::Cancel,
            };
            report_task(task_executor, req).await?;
            let req = ReportTaskReq {
                id: task.id,
                op: ReportTaskOp::Commit(TaskResultSpec {
                    exit_status: 0,
                    msg: Some(TaskResultMessage::ResourceForbidden),
                }),
            };
            report_task(task_executor, req).await?;
            task_executor
                .announce_task_state_ex(
                    &task.uuid,
                    TaskExecState::FetchResourceForbidden as i32,
                    60,
                )
                .await;
            task_executor
                .announce_task_state_ex(&task.uuid, TaskExecState::TaskCommitted as i32, 60)
                .await;
            return Ok(());
        } else {
            let resp: ErrorMsg = resp
                .json()
                .await
                .unwrap_or_else(|e| ErrorMsg { msg: e.to_string() });
            task_executor
                .announce_task_state_ex(&task.uuid, TaskExecState::FetchResourceError as i32, 60)
                .await;
            return Err(Error::Custom(format!(
                "Fetch resource info failed with error: {}",
                resp.msg
            )));
        }
    }

    if let Some((watched_task_uuid, watched_task_state)) = task.spec.watch {
        // Watch other tasks to specified state to trigger this task
        if task_executor.task_redis_conn.is_some() && task_executor.task_redis_pubsub.is_some() {
            task_executor
                .announce_task_state(&task.uuid, TaskExecState::Watch as i32)
                .await;
            let tmp_cancel_token = task_executor.task_cancel_token.clone();
            tokio::select! {
                biased;
                _ = tmp_cancel_token.cancelled() => {
                    tracing::info!("Task watching interrupted by shutdown signal");
                    task_executor.unsubscribe_task_exec_state(&watched_task_uuid).await;
                    task_executor
                        .announce_task_state_ex(&task.uuid, TaskExecState::WorkerExited as i32, 60)
                        .await;
                    return Ok(());
                },
                _ = task_executor.watch_task(&watched_task_uuid, watched_task_state) => {},
                _ = tokio::time::sleep_until(timeout_until) => {
                    tracing::debug!("Watching timeout, commit this task as canceled");
                    task_executor.unsubscribe_task_exec_state(&watched_task_uuid).await;
                    let req = ReportTaskReq {
                        id: task.id,
                        op: ReportTaskOp::Cancel,
                    };
                    report_task(task_executor, req).await?;
                    let req = ReportTaskReq {
                        id: task.id,
                        op: ReportTaskOp::Commit(TaskResultSpec {
                            exit_status: 0,
                            msg: Some(TaskResultMessage::WatchTimeout),
                        }),
                    };
                    report_task(task_executor, req).await?;
                    task_executor
                        .announce_task_state_ex(
                            &task.uuid,
                            TaskExecState::WatchTimeout as i32,
                            60,
                        )
                        .await;
                    task_executor
                        .announce_task_state_ex(&task.uuid, TaskExecState::TaskCommitted as i32, 60)
                        .await;
                    return Ok(());
                }
            }
        }
    }

    task_executor
        .announce_task_state_ex(
            &task.uuid,
            TaskExecState::ExecPending as i32,
            task.timeout.as_secs() + 60,
        )
        .await;
    let timeout_until = tokio::time::Instant::now() + task.timeout;

    // Setup new task file path and clean up any stale file
    let new_task_path = task_executor.task_cache_path.join("new_task.json");
    let _ = tokio::fs::remove_file(&new_task_path).await; // Ignore errors if file doesn't exist

    let mut command = Command::new("/usr/bin/env");
    command
        .args(task.spec.args)
        .envs(task.spec.envs)
        .env(
            "MITO_RESULT_DIR",
            task_executor.task_cache_path.join("result"),
        )
        .env("MITO_EXEC_DIR", task_executor.task_cache_path.join("exec"))
        .env(
            "MITO_RESOURCE_DIR",
            task_executor.task_cache_path.join("resource"),
        )
        .env("MITO_TASK_UUID", task.uuid.to_string())
        .env("MITO_NEW_TASK", &new_task_path)
        .stdin(std::process::Stdio::null());
    if let Some(uuid) = task.upstream_task_uuid {
        command.env("MITO_UPSTREAM_TASK_UUID", uuid.to_string());
    }
    if task.spec.terminal_output {
        command
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
    } else {
        command
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
    }

    let mut child = command.spawn().inspect_err(|e| {
        tracing::error!("Failed to spawn task: {}", e);
    })?;
    task_executor
        .announce_task_state(&task.uuid, TaskExecState::ExecSpawned as i32)
        .await;
    let process_output = async {
        if task.spec.terminal_output {
            let process_output = async {
                let mut stdout_buf = Vec::new();
                let mut stdout = child.stdout.take().unwrap();
                let mut stderr_buf = Vec::new();
                let mut stderr = child.stderr.take().unwrap();
                tokio::try_join!(
                    stdout.read_to_end(&mut stdout_buf),
                    stderr.read_to_end(&mut stderr_buf),
                    child.wait()
                )
                .map(|(_, _, exit_status)| ProcessOutput::WithLog {
                    stdout: stdout_buf,
                    stderr: stderr_buf,
                    exit_status,
                })
            };
            process_output.await
        } else {
            child
                .wait()
                .await
                .map(|exit_status| ProcessOutput::WithoutLog { exit_status })
        }
    };

    let output = tokio::select! {
        biased;
        _ = task_executor.task_cancel_token.cancelled() => {
            tracing::info!("Task execution interrupted by shutdown signal");
            child.kill().await.inspect_err(|e| {
                tracing::error!("Failed to kill task: {}", e);
            })?;
            task_executor
                .announce_task_state_ex(&task.uuid, TaskExecState::WorkerExited as i32, 60)
                .await;
            return Ok(());
        },
        output = process_output => {
            task_executor
                .announce_task_state_ex(&task.uuid, TaskExecState::ExecFinished as i32, 660)
                .await;
            output.map(TaskResult::Finish)
        },
        _ = tokio::time::sleep_until(timeout_until) => {
            tracing::debug!("Task execution timeout");
            task_executor
                .announce_task_state_ex(&task.uuid, TaskExecState::ExecTimeout as i32, 60)
                .await;
            if let Some(id) = child.id() {
                // TODO: we may change this when once the `linux_pidfd` is stabilized in standard library
                // Tracking issue for std lib: [rust-lang/rust #82971](https://github.com/rust-lang/rust/issues/82971)
                // Tracking issue for tokio: [tokio-rs/tokio #6281](https://github.com/tokio-rs/tokio/issues/6281)
                let _ = signal::kill(Pid::from_raw(id as i32), Signal::SIGTERM).inspect_err(|e| {
                    tracing::error!("Failed to send SIGTERM to task: {}", e);
                });
            }
            tokio::select! {
                biased;
                _ = child.wait() => {},
                _ = task_executor.task_cancel_token.cancelled() => {
                    child.kill().await.inspect_err(|e| {
                        tracing::error!("Failed to kill task: {}", e);
                    })?;
                    return Ok(());
                },
                _ = tokio::time::sleep(std::time::Duration::from_secs(10)) => {
                    child.kill().await.inspect_err(|e| {
                        tracing::error!("Failed to kill task: {}", e);
                    })?;
                },
            }
            if task.spec.terminal_output {
                let output = child.wait_with_output().await?;
                Ok(TaskResult::Timeout(ProcessOutput::WithLog {
                    stdout: output.stdout,
                    stderr: output.stderr,
                    exit_status: output.status,
                }))
            } else {
                let exit_status = child.wait().await?;
                Ok(TaskResult::Timeout(ProcessOutput::WithoutLog {
                    exit_status,
                }))
            }
        },
    }?;
    tracing::debug!("Task execution finished");
    task_executor
        .announce_task_state_ex(&task.uuid, TaskExecState::UploadResult as i32, 660)
        .await;
    process_task_result(task.id, task.uuid, task_executor, output).await?;
    Ok(())
}

async fn process_task_result(
    id: i64,
    uuid: Uuid,
    task_executor: &mut TaskExecutor,
    output: TaskResult,
) -> crate::error::Result<()> {
    let (is_finished, exit_status) = output.state();
    let req = ReportTaskReq {
        id,
        op: if is_finished {
            task_executor
                .announce_task_state_ex(&uuid, TaskExecState::UploadFinishedResult as i32, 660)
                .await;
            ReportTaskOp::Finish
        } else {
            task_executor
                .announce_task_state_ex(&uuid, TaskExecState::UploadCancelledResult as i32, 660)
                .await;
            ReportTaskOp::Cancel
        },
    };
    report_task(task_executor, req).await?;
    // Compress possible output and upload
    let (tx, mut rx) = mpsc::channel::<(ArtifactContentType, u64)>(3);
    // Spawn a task to archive the output
    let timeout_cancel_token = CancellationToken::new();
    let archive_timeout_cancel_token = timeout_cancel_token.clone();
    let archive_cancel_token = task_executor.task_cancel_token.clone();
    let archive_cache_path = task_executor.task_cache_path.clone();
    let archive_hd = tokio::spawn(async move {
        let result_dir = archive_cache_path.join("result");
        if !result_dir
            .read_dir()
            .map(|mut dir| dir.next().is_none())
            .unwrap_or(true)
        {
            let tar_file =
                tokio::fs::File::create(archive_cache_path.join("result.tar.gz")).await?;
            let encoder = GzipEncoder::new(tar_file);
            let mut ar = Builder::new(encoder);
            let compress_task = async {
                ar.append_dir_all("result", result_dir).await?;
                std::io::Result::Ok(())
            };
            tokio::select! {
                biased;
                _ = archive_cancel_token.cancelled() => {
                    let mut encoder = ar.into_inner().await?;
                    encoder.shutdown().await?;
                    tokio::fs::remove_file(archive_cache_path.join("result.tar.gz")).await?;
                    tracing::info!("Task output generation interrupted by shutdown signal");
                    return Ok(());
                }
                _ = archive_timeout_cancel_token.cancelled() => {
                    let mut encoder = ar.into_inner().await?;
                    encoder.shutdown().await?;
                    tokio::fs::remove_file(archive_cache_path.join("result.tar.gz")).await?;
                    tracing::warn!("Task output generation timeout");
                    return Ok(());
                }
                res = compress_task => {
                    match res {
                        Ok(_) => {
                            ar.finish().await?;
                            let mut encoder = ar.into_inner().await?;
                            encoder.shutdown().await?;
                            let file = encoder.into_inner();
                            let size = file.metadata().await?.len();
                            if let Err(e) = tx.send((ArtifactContentType::Result, size)).await {
                                tracing::error!("Failed to send result size: {}", e);
                                archive_cancel_token.cancel();
                                return Ok(());
                            }
                        }
                        Err(e) => {
                            tracing::error!("Failed to compress result: {}", e);
                            let mut encoder = ar.into_inner().await?;
                            encoder.shutdown().await?;
                            archive_cancel_token.cancel();
                            return Err(e);
                        }
                    }

                }
            }
        }
        let exec_log_dir = archive_cache_path.join("exec");
        if !exec_log_dir
            .read_dir()
            .map(|mut dir| dir.next().is_none())
            .unwrap_or(true)
        {
            let file_name = ArtifactContentType::ExecLog.to_string();
            let tar_file = tokio::fs::File::create(archive_cache_path.join(&file_name)).await?;
            let encoder = GzipEncoder::new(tar_file);
            let mut ar = Builder::new(encoder);
            let compress_task = async {
                ar.append_dir_all("exec-log", exec_log_dir).await?;
                std::io::Result::Ok(())
            };
            tokio::select! {
                biased;
                _ = archive_cancel_token.cancelled() => {
                    let mut encoder = ar.into_inner().await?;
                    encoder.shutdown().await?;
                    tokio::fs::remove_file(archive_cache_path.join(&file_name)).await?;
                    tracing::info!("Task output generation interrupted by shutdown signal");
                    return Ok(());
                }
                _ = archive_timeout_cancel_token.cancelled() => {
                    let mut encoder = ar.into_inner().await?;
                    encoder.shutdown().await?;
                    tokio::fs::remove_file(archive_cache_path.join(&file_name)).await?;
                    tracing::warn!("Task output generation timeout");
                    return Ok(());
                }
                res = compress_task => {
                    match res {
                        Ok(_) => {
                            ar.finish().await?;
                            let mut encoder = ar.into_inner().await?;
                            encoder.shutdown().await?;
                            let file = encoder.into_inner();
                            let size = file.metadata().await?.len();
                            if let Err(e) = tx.send((ArtifactContentType::ExecLog, size)).await {
                                tracing::error!("Failed to compress exec log: {}", e);
                                archive_cancel_token.cancel();
                                return Ok(());
                            }
                        }
                        Err(e) => {
                            tracing::error!("Failed to compress exec log: {}", e);
                            let mut encoder = ar.into_inner().await?;
                            encoder.shutdown().await?;
                            archive_cancel_token.cancel();
                            return Err(e);
                        }
                    }

                }
            }
        }
        if let ProcessOutput::WithLog { stdout, stderr, .. } = output.get_output() {
            let tar_file =
                tokio::fs::File::create(archive_cache_path.join("std-log.tar.gz")).await?;
            let encoder = GzipEncoder::new(tar_file);
            let mut ar = Builder::new(encoder);
            let compress_task = async {
                let mut header = Header::new_gnu();
                header.set_cksum();
                header.set_mode(436);
                header.set_size(stdout.len() as u64);
                ar.append_data(&mut header, "std-log/stdout.log", &*stdout)
                    .await?;
                header.set_size(stderr.len() as u64);
                ar.append_data(&mut header, "std-log/stderr.log", &*stderr)
                    .await?;
                std::io::Result::Ok(())
            };
            tokio::select! {
                biased;
                _ = archive_cancel_token.cancelled() => {
                    let mut encoder = ar.into_inner().await?;
                    encoder.shutdown().await?;
                    tokio::fs::remove_file(archive_cache_path.join("std-log.tar.gz")).await?;
                    tracing::info!("Task output generation interrupted by shutdown signal");
                    return Ok(());
                }
                _ = archive_timeout_cancel_token.cancelled() => {
                    let mut encoder = ar.into_inner().await?;
                    encoder.shutdown().await?;
                    tokio::fs::remove_file(archive_cache_path.join("std-log.tar.gz")).await?;
                    tracing::warn!("Task output generation timeout");
                    return Ok(());
                }
                res = compress_task => {
                    match res {
                        Ok(_) => {
                            ar.finish().await?;
                            let mut encoder = ar.into_inner().await?;
                            encoder.shutdown().await?;
                            let file = encoder.into_inner();
                            let size = file.metadata().await?.len();
                            if let Err(e) = tx.send((ArtifactContentType::StdLog, size)).await {
                                tracing::error!("Failed to compress std log: {}", e);
                                archive_cancel_token.cancel();
                                return Ok(());
                            }
                        }
                        Err(e) => {
                            tracing::error!("Failed to compress std log: {}", e);
                            let mut encoder = ar.into_inner().await?;
                            encoder.shutdown().await?;
                            archive_cancel_token.cancel();
                            return Err(e);
                        }
                    }

                }
            }
        }
        Ok(())
    });
    let upload_artifact_fut = async {
        while let Some((content_type, content_length)) = rx.recv().await {
            let req = ReportTaskReq {
                id,
                op: ReportTaskOp::Upload {
                    content_type,
                    content_length,
                },
            };
            task_executor.task_url.set_path("workers/tasks");
            let resp = loop {
                let resp = task_executor
                    .task_client
                    .post(task_executor.task_url.as_str())
                    .json(&req)
                    .bearer_auth(&task_executor.task_credential)
                    .send()
                    .await;
                match resp {
                    Ok(resp) => {
                        if resp.status().is_success() {
                            let resp = resp
                                .json::<ReportTaskResp>()
                                .await
                                .map_err(RequestError::from)?;
                            break resp;
                        } else if resp.status() == StatusCode::UNAUTHORIZED {
                            tracing::info!("Request upload url failed with coordinator force exit");
                            task_executor
                                .coordinator_force_exit
                                .store(true, std::sync::atomic::Ordering::Release);
                            task_executor.task_cancel_token.cancel();
                            return Ok(());
                        } else if resp.status() == StatusCode::NOT_FOUND {
                            tracing::debug!("Task not found, ignore and go on for next cycle");
                            return Ok(());
                        } else if resp.status() == StatusCode::FORBIDDEN {
                            let resp: ErrorMsg = resp
                                .json()
                                .await
                                .unwrap_or_else(|e| ErrorMsg { msg: e.to_string() });
                            tracing::info!(
                                "Request upload url failed with permission denied: {}",
                                resp.msg
                            );
                            return Ok(());
                        } else {
                            let resp: ErrorMsg = resp
                                .json()
                                .await
                                .unwrap_or_else(|e| ErrorMsg { msg: e.to_string() });
                            task_executor.task_cancel_token.cancel();
                            return Err(Error::Custom(format!(
                                "Request upload url failed with error: {}",
                                resp.msg
                            )));
                        }
                    }
                    Err(e) => {
                        if e.is_connect() && e.is_request() {
                            tracing::error!(
                                    "Request upload url failed with connection error: {}. Retry after {:?}",
                                    e,
                                    task_executor.polling_interval
                                );
                            tokio::select! {
                                biased;
                                _ = task_executor.task_cancel_token.cancelled() => return Ok(()),
                                _ = tokio::time::sleep(task_executor.polling_interval) => {},
                            }
                            continue;
                        } else {
                            task_executor.task_cancel_token.cancel();
                            return Err(RequestError::from(e).into());
                        }
                    }
                }
            };
            if let Some(url) = resp.url {
                loop {
                    let file = tokio::fs::File::open(
                        task_executor.task_cache_path.join(content_type.to_string()),
                    )
                    .await?;
                    let upload_file = task_executor
                        .task_client
                        .put(url.as_str())
                        .header(CONTENT_LENGTH, content_length)
                        .body(file)
                        .send();
                    let resp = tokio::select! {
                        biased;
                        _ = task_executor.task_cancel_token.cancelled() => {
                            tracing::info!("Upload failed with shutdown signal");
                            return Ok(());
                        }
                        _ = timeout_cancel_token.cancelled() => {
                            tracing::warn!("Upload failed with timeout");
                            return Ok(());
                        }
                        resp = upload_file => resp
                    };
                    match resp {
                        Ok(resp) => {
                            if resp.status().is_success() {
                                break;
                            } else {
                                let status = resp.status();
                                return Err(Error::Custom(format!(
                                    "Upload failed with status code: {status}"
                                )));
                            }
                        }
                        Err(e) => {
                            if e.is_connect() && e.is_request() {
                                tracing::error!(
                                    "Upload failed with connection error: {}. Retry after {:?}",
                                    e,
                                    task_executor.polling_interval
                                );
                                tokio::select! {
                                    biased;
                                    _ = task_executor.task_cancel_token.cancelled() => return Ok(()),
                                    _ = timeout_cancel_token.cancelled() => {
                                        tracing::warn!("Upload failed with timeout");
                                        return Ok(());
                                    }
                                    _ = tokio::time::sleep(task_executor.polling_interval) => {},
                                }
                                continue;
                            } else {
                                return Err(RequestError::from(e).into());
                            }
                        }
                    }
                }
            }
        }
        crate::error::Result::Ok(())
    };
    let timeout_until = tokio::time::Instant::now() + std::time::Duration::from_secs(600);
    tokio::select! {
        biased;
        _ = tokio::time::sleep_until(timeout_until) => {
            tracing::warn!("Upload result timeout");
            timeout_cancel_token.cancel();
            // Commit the task result
            let req = ReportTaskReq {
                id,
                op: ReportTaskOp::Commit(TaskResultSpec {
                    exit_status: exit_status.into_raw(),
                    msg: Some(TaskResultMessage::UploadResultTimeout),
                }),
            };
            report_task(task_executor, req).await?;
            task_executor
                .announce_task_state_ex(&uuid, TaskExecState::UploadResultTimeout as i32, 60)
                .await;
            task_executor
                .announce_task_state_ex(&uuid, TaskExecState::TaskCommitted as i32, 60)
                .await;
            archive_hd.await??;
        }
        res = upload_artifact_fut => {
            res?;
            archive_hd.await??;
            if task_executor.task_cancel_token.is_cancelled() {
                tracing::info!("Task execution interrupted by shutdown signal");
                task_executor
                    .announce_task_state_ex(&uuid, TaskExecState::WorkerExited as i32, 60)
                    .await;
                return Ok(());
            }
            task_executor
                .announce_task_state_ex(&uuid, TaskExecState::UploadResultFinished as i32, 60)
                .await;

            // Check for new task file and submit if present
            let new_task_scceed = submit_new_task_if_present(id, task_executor).await;
            let msg = if is_finished {
                if new_task_scceed {
                    None
                } else {
                    Some(TaskResultMessage::SubmitNewTaskFailed)
                }
            } else {
                Some(TaskResultMessage::ExecTimeout)
            };
            // Commit the task result
            let req = ReportTaskReq {
                id,
                op: ReportTaskOp::Commit(TaskResultSpec {
                    exit_status: exit_status.into_raw(),
                    msg
                }),
            };
            report_task(task_executor, req).await?;
            task_executor
                .announce_task_state_ex(&uuid, TaskExecState::TaskCommitted as i32, 60)
                .await;

        }
    }

    // clean the directory after all the artifacts uploaded and the task committed
    tokio::fs::remove_dir_all(&task_executor.task_cache_path).await?;
    tokio::fs::create_dir_all(&task_executor.task_cache_path).await?;
    tokio::fs::create_dir_all(&task_executor.task_cache_path.join("result")).await?;
    tokio::fs::create_dir_all(&task_executor.task_cache_path.join("exec")).await?;
    Ok(())
}

async fn report_task(
    task_executor: &mut TaskExecutor,
    req: ReportTaskReq,
) -> crate::error::Result<()> {
    task_executor.task_url.set_path("workers/tasks");
    loop {
        let resp = task_executor
            .task_client
            .post(task_executor.task_url.as_str())
            .json(&req)
            .bearer_auth(&task_executor.task_credential)
            .send()
            .await;
        match resp {
            Ok(resp) => {
                if resp.status().is_success() {
                    break;
                } else if resp.status() == StatusCode::UNAUTHORIZED {
                    tracing::info!("Report task failed with coordinator force exit");
                    task_executor
                        .coordinator_force_exit
                        .store(true, std::sync::atomic::Ordering::Release);
                    task_executor.task_cancel_token.cancel();
                    return Ok(());
                } else if resp.status() == StatusCode::NOT_FOUND {
                    tracing::debug!("Task not found, ignore and go on for next cycle");
                    return Ok(());
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
                        task_executor.polling_interval
                    );
                    tokio::select! {
                        biased;
                        _ = task_executor.task_cancel_token.cancelled() => break,
                        _ = tokio::time::sleep(task_executor.polling_interval) => {},
                    }
                    continue;
                } else {
                    return Err(RequestError::from(e).into());
                }
            }
        }
    }
    Ok(())
}

async fn submit_new_task_if_present(task_id: i64, task_executor: &mut TaskExecutor) -> bool {
    let new_task_path = task_executor.task_cache_path.join("new_task.json");
    // Check if the new task file exists
    if !new_task_path.exists() {
        return true; // No new task to submit
    }

    // Read and parse the file
    let new_task_content = match tokio::fs::read_to_string(&new_task_path).await {
        Ok(content) if !content.trim().is_empty() => content,
        Ok(_) => {
            tracing::debug!("New task file exists but is empty, ignoring");
            let _ = tokio::fs::remove_file(&new_task_path).await;
            return true;
        }
        Err(e) => {
            tracing::warn!("Failed to read new task file: {}", e);
            let _ = tokio::fs::remove_file(&new_task_path).await;
            return false;
        }
    };

    // Parse JSON to SubmitTaskReq
    let submit_req: crate::schema::SubmitTaskReq = match serde_json::from_str(&new_task_content) {
        Ok(req) => req,
        Err(e) => {
            tracing::warn!("Failed to parse new task JSON: {}", e);
            let _ = tokio::fs::remove_file(&new_task_path).await;
            return false;
        }
    };

    tracing::debug!(
        "Submitting new task from completed task {} to group '{}'",
        task_id,
        submit_req.group_name
    );
    let req = ReportTaskReq {
        id: task_id,
        op: ReportTaskOp::Submit(Box::new(submit_req)),
    };
    if let Err(e) = report_task(task_executor, req).await {
        tracing::warn!("Failed to submit new task: {}", e);
        return false;
    }

    // Always clean up the file after processing (success or failure)
    let _ = tokio::fs::remove_file(&new_task_path).await;
    true
}
