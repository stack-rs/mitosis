//! Transport-agnostic task-execution core, shared by the worker and the agent.
//!
//! Both `worker.rs` and `agent.rs` provide a [`CoordinatorClient`] impl and run
//! this same [`execute_task`] / [`TaskExecutor`] machinery — only the coordinator
//! I/O (which endpoints, which credential, redis-or-poll) differs between them.

use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use async_compression::tokio::write::GzipEncoder;
use nix::sys::signal::{self, Signal};
use nix::unistd::Pid;
use reqwest::header::CONTENT_LENGTH;
use reqwest::{Client, StatusCode};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_tar::{Builder, Header};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::entity::content::ArtifactContentType;
use crate::entity::state::TaskExecState;
use crate::error::{Error, ErrorMsg, RequestError};
use crate::schema::*;
use crate::service::s3::download_file;

#[async_trait::async_trait]
pub trait CoordinatorClient: Send {
    /// POST a task report; returns the presigned upload URL for `Upload`, else None.
    async fn report(&mut self, id: i64, op: ReportTaskOp) -> crate::error::Result<Option<String>>;
    /// Authed GET for a task input artifact's presigned download info.
    fn artifact_download_req(
        &self,
        uuid: Uuid,
        content_type: ArtifactContentType,
    ) -> reqwest::RequestBuilder;
    /// Authed GET for a task input attachment's presigned download info.
    fn attachment_download_req(&self, task_uuid: &Uuid, key: &str) -> reqwest::RequestBuilder;
    /// Wait until `uuid` reaches `target` (worker: redis-or-poll; agent: poll).
    async fn watch(&mut self, uuid: &Uuid, target: TaskExecState);
    /// Whether a watch should be attempted at all (worker: has redis; agent: yes).
    fn can_watch(&self) -> bool;
    /// Clean up a watch subscription on abort (worker: redis unsubscribe; agent: no-op).
    async fn unsubscribe(&mut self, uuid: &Uuid);
    /// Announce a task exec-state (worker: redis set+publish; agent: no-op).
    async fn announce_state(&mut self, uuid: &Uuid, state: i32, ex: Option<u64>);
}

pub struct TaskExecutor {
    pub task_cancel_token: CancellationToken,
    pub coordinator_force_exit: Arc<AtomicBool>,
    pub polling_interval: std::time::Duration,
    pub task_cache_path: PathBuf,
    /// Plain HTTP client for presigned S3 transfers (resource download / artifact upload).
    pub http_client: Client,
    pub client: Box<dyn CoordinatorClient>,
}

impl TaskExecutor {
    async fn report(&mut self, id: i64, op: ReportTaskOp) -> crate::error::Result<Option<String>> {
        self.client.report(id, op).await
    }

    async fn announce_task_state(&mut self, uuid: &Uuid, state: i32) {
        self.client.announce_state(uuid, state, None).await
    }

    async fn announce_task_state_ex(&mut self, uuid: &Uuid, state: i32, ex: u64) {
        self.client.announce_state(uuid, state, Some(ex)).await
    }

    async fn watch_task(&mut self, uuid: &Uuid, target: TaskExecState) {
        self.client.watch(uuid, target).await
    }

    async fn unsubscribe_task_exec_state(&mut self, uuid: &Uuid) {
        self.client.unsubscribe(uuid).await
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

enum ResourceError {
    /// 404: the resource does not exist.
    NotFound,
    /// 403 status: the resource is forbidden.
    ForbiddenStatus,
    /// The presigned S3 download itself failed.
    DownloadFailed,
    /// Exceeded the per-download (120s) or overall (30-min) budget.
    Timeout,
    /// Cancelled via the shutdown token.
    Cancelled,
    /// Connection error or unexpected status — propagate to the caller.
    Other(crate::error::Error),
}

/// The shared "abandon this task" report pair: `Cancel` then `Commit` with the
/// given result message. The caller handles the state announcements (which vary
/// per case) around it.
async fn report_cancel_commit(
    task_executor: &mut TaskExecutor,
    task_id: i64,
    msg: TaskResultMessage,
) -> crate::error::Result<()> {
    report_task(
        task_executor,
        ReportTaskReq {
            id: task_id,
            op: ReportTaskOp::Cancel,
        },
    )
    .await?;
    report_task(
        task_executor,
        ReportTaskReq {
            id: task_id,
            op: ReportTaskOp::Commit(TaskResultSpec {
                exit_status: 0,
                msg: Some(msg),
            }),
        },
    )
    .await?;
    Ok(())
}

/// Fetch one resource to its local path: pure I/O + resilience (connection
/// retry, per-download 120s + overall `timeout_until`, cancellation). It never
/// touches the task lifecycle — every failure is returned as a typed
/// [`ResourceError`] for `execute_task` to translate into a task outcome.
async fn download_resource(
    task_executor: &mut TaskExecutor,
    resource: RemoteResourceDownload,
    task_uuid: &Uuid,
    timeout_until: Instant,
) -> std::result::Result<(), ResourceError> {
    let resp = loop {
        // The request (URL + credential) is the only worker/agent-specific part;
        // the retry / timeout / status orchestration below is shared.
        let req = match &resource.remote_file {
            RemoteResource::Artifact { uuid, content_type } => task_executor
                .client
                .artifact_download_req(*uuid, *content_type),
            RemoteResource::Attachment { key } => task_executor
                .client
                .attachment_download_req(task_uuid, key.as_str()),
        };
        match req.send().await {
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
                        _ = task_executor.task_cancel_token.cancelled() => return Err(ResourceError::Cancelled),
                        _ = tokio::time::sleep(task_executor.polling_interval) => {},
                        _ = tokio::time::sleep_until(timeout_until) => {
                            tracing::debug!("Fetching resource timeout, commit this task as cancelled");
                            return Err(ResourceError::Timeout);
                        }
                    }
                    continue;
                } else {
                    return Err(ResourceError::Other(RequestError::from(e).into()));
                }
            }
        }
    };
    if resp.status().is_success() {
        let download_resp = resp
            .json::<RemoteResourceDownloadResp>()
            .await
            .map_err(|e| ResourceError::Other(RequestError::from(e).into()))?;
        let local_path = task_executor
            .task_cache_path
            .join("resource")
            .join(resource.local_path);
        tokio::select! {
            biased;
            res = download_file(&task_executor.http_client, &download_resp, local_path, false) => {
                if let Err(e) = res {
                    tracing::error!("Failed to download resource: {}", e);
                    return Err(ResourceError::DownloadFailed);
                }
                Ok(())
            }
            _ = task_executor.task_cancel_token.cancelled() => Err(ResourceError::Cancelled),
            _ = tokio::time::sleep(std::time::Duration::from_secs(120)) => {
                tracing::debug!("Fetching resource timeout, commit this task as cancelled");
                Err(ResourceError::Timeout)
            }
            _ = tokio::time::sleep_until(timeout_until) => {
                tracing::debug!("Fetching resource timeout, commit this task as cancelled");
                Err(ResourceError::Timeout)
            }
        }
    } else if resp.status() == StatusCode::NOT_FOUND {
        tracing::debug!("Resource not found, commit this task as cancelled");
        Err(ResourceError::NotFound)
    } else if resp.status() == StatusCode::FORBIDDEN {
        tracing::debug!("Resource is forbidden to be fetched, commit this task as cancelled");
        Err(ResourceError::ForbiddenStatus)
    } else {
        let resp: ErrorMsg = resp
            .json()
            .await
            .unwrap_or_else(|e| ErrorMsg { msg: e.to_string() });
        Err(ResourceError::Other(Error::Custom(format!(
            "Fetch resource info failed with error: {}",
            resp.msg
        ))))
    }
}

pub async fn execute_task(
    task: WorkerTaskResp,
    task_executor: &mut TaskExecutor,
) -> crate::error::Result<()> {
    task_executor
        .announce_task_state_ex(&task.uuid, TaskExecState::FetchResource as i32, 360)
        .await;
    // Allow downloading resources for at most 30 minutes
    let timeout_until = tokio::time::Instant::now() + std::time::Duration::from_secs(1800);
    for resource in task.spec.resources {
        match download_resource(task_executor, resource, &task.uuid, timeout_until).await {
            Ok(()) => {}
            // Shutdown while fetching — just stop; teardown reclaims the task.
            Err(ResourceError::Cancelled) => return Ok(()),
            // Connection error / unexpected status — surface as a hard error.
            Err(ResourceError::Other(e)) => {
                task_executor
                    .announce_task_state_ex(
                        &task.uuid,
                        TaskExecState::FetchResourceError as i32,
                        60,
                    )
                    .await;
                return Err(e);
            }
            Err(ResourceError::NotFound) => {
                report_cancel_commit(task_executor, task.id, TaskResultMessage::ResourceNotFound)
                    .await?;
                task_executor
                    .announce_task_state_ex(
                        &task.uuid,
                        TaskExecState::FetchResourceNotFound as i32,
                        60,
                    )
                    .await;
                task_executor
                    .announce_task_state_ex(&task.uuid, TaskExecState::TaskCommitted as i32, 60)
                    .await;
                return Ok(());
            }
            Err(ResourceError::ForbiddenStatus) => {
                report_cancel_commit(task_executor, task.id, TaskResultMessage::ResourceForbidden)
                    .await?;
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
            }
            Err(ResourceError::DownloadFailed) => {
                report_cancel_commit(task_executor, task.id, TaskResultMessage::ResourceForbidden)
                    .await?;
                // Preserve the original no-expiry announce for this case.
                task_executor
                    .announce_task_state(&task.uuid, TaskExecState::FetchResourceForbidden as i32)
                    .await;
                task_executor
                    .announce_task_state_ex(&task.uuid, TaskExecState::TaskCommitted as i32, 60)
                    .await;
                return Ok(());
            }
            Err(ResourceError::Timeout) => {
                report_cancel_commit(
                    task_executor,
                    task.id,
                    TaskResultMessage::FetchResourceTimeout,
                )
                .await?;
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
    }

    if let Some((watched_task_uuid, watched_task_state)) =
        task.exec_options.as_ref().and_then(|opts| opts.watch)
    {
        // Watch other tasks to specified state to trigger this task
        if task_executor.client.can_watch() {
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
                    tracing::debug!("Watching timeout, commit this task as cancelled");
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

    // Default timeout is 10 minutes if not specified
    let timeout = task
        .spec
        .timeout
        .unwrap_or(std::time::Duration::from_secs(600));
    task_executor
        .announce_task_state_ex(
            &task.uuid,
            TaskExecState::ExecPending as i32,
            timeout.as_secs() + 60,
        )
        .await;
    let timeout_until = tokio::time::Instant::now() + timeout;

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
            // Request a presigned upload URL via the unified report path.
            let resp_url = task_executor
                .report(
                    id,
                    ReportTaskOp::Upload {
                        content_type,
                        content_length,
                    },
                )
                .await?;
            if let Some(url) = resp_url {
                loop {
                    let file = tokio::fs::File::open(
                        task_executor.task_cache_path.join(content_type.to_string()),
                    )
                    .await?;
                    let upload_file = task_executor
                        .http_client
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
    // Non-upload reports ignore the (always-None) URL.
    task_executor.report(req.id, req.op).await.map(|_| ())
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
