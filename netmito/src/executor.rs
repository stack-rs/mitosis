//! The execution core: one flow, three report targets.
//!
//! Everything a running process needs from the outside world — input downloads,
//! dependency waits, state announcements and outcome reports — sits behind
//! [`ExecClient`]. [`execute`] itself knows nothing about workers, agents, tasks
//! or hooks, so the same code drives all three of:
//!
//! | impl | lives in | reports to |
//! |---|---|---|
//! | `WorkerTaskClient` | `worker.rs` | `POST /workers/tasks` |
//! | `AgentTaskClient` | `agent.rs` | `POST /agents/tasks/report` |
//! | `AgentHookClient` | `agent.rs` | `POST /agents/job/hook` |

use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::ExitStatus;

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
use crate::schema::{
    ExecSpec, RemoteResource, RemoteResourceDownload, RemoteResourceDownloadResp, SubmitTaskReq,
    TaskExecOptions, TaskResultMessage, TaskResultSpec,
};
use crate::service::s3::download_file;

/// How long the whole input-fetch phase may take.
const RESOURCE_FETCH_BUDGET: std::time::Duration = std::time::Duration::from_secs(1800);
/// How long a single input download may take.
const SINGLE_DOWNLOAD_BUDGET: std::time::Duration = std::time::Duration::from_secs(120);
/// How long the archive-and-upload phase may take once the process has exited.
const UPLOAD_BUDGET: std::time::Duration = std::time::Duration::from_secs(600);

/// One unit of execution's coordinator-facing half.
///
/// An impl bundles who is talking to the coordinator (which credential, which
/// endpoints) with what is being reported (which task, or which suite hook). It
/// is built per unit and owns that unit's identity, which is what lets a hook —
/// identified only by `{job, hook_type}` — use the same core as a task.
#[async_trait::async_trait]
pub trait ExecClient: Send {
    /// How this unit is named in logs.
    fn describe(&self) -> String;

    /// Identity exported into the process's environment, on top of the standard
    /// `MITO_*` directory variables the core always sets.
    fn exec_env(&self) -> Vec<(&'static str, String)>;

    /// Whether the process may hand back a child task through `MITO_NEW_TASK`.
    /// False for hooks: the hook report endpoint has no submit operation.
    fn supports_child_tasks(&self) -> bool {
        false
    }

    /// The process is done and its artifacts are about to be uploaded.
    /// `finished` distinguishes a clean exit from a cancellation or timeout.
    /// `result` is the outcome so far — the final message is not known yet, so
    /// impls that can only report once should expect [`report_commit`] to refine
    /// this.
    ///
    /// [`report_commit`]: ExecClient::report_commit
    async fn report_finish(
        &mut self,
        finished: bool,
        result: &TaskResultSpec,
    ) -> crate::error::Result<()>;

    /// Presign an upload for one produced artifact.
    async fn request_upload(
        &mut self,
        content_type: ArtifactContentType,
        content_length: u64,
    ) -> crate::error::Result<UploadTarget>;

    /// The unit's final result. Ends it.
    async fn report_commit(&mut self, result: TaskResultSpec) -> crate::error::Result<()>;

    /// Submit a task the process asked to spawn. Only ever called when
    /// [`supports_child_tasks`] is true.
    ///
    /// [`supports_child_tasks`]: ExecClient::supports_child_tasks
    async fn submit_child_task(&mut self, req: SubmitTaskReq) -> crate::error::Result<()>;

    /// Authed request for an input artifact's presigned download info.
    fn artifact_download_req(
        &self,
        uuid: Uuid,
        content_type: ArtifactContentType,
    ) -> reqwest::RequestBuilder;

    /// Authed request for an input attachment's presigned download info.
    fn attachment_download_req(&self, key: &str) -> reqwest::RequestBuilder;

    /// Publish this unit's fine-grained exec state (worker: redis set+publish;
    /// agent: nothing to publish to yet). `ex` is an expiry in seconds.
    async fn announce_state(&mut self, state: TaskExecState, ex: Option<u64>);

    /// Whether a `watch` dependency can be resolved at all. When false the wait
    /// is skipped and the unit runs immediately.
    fn can_watch(&self) -> bool {
        false
    }

    /// Wait until `uuid` reaches `target`. Only called when [`can_watch`] is true.
    ///
    /// [`can_watch`]: ExecClient::can_watch
    async fn watch(&mut self, uuid: &Uuid, target: TaskExecState) {
        let _ = (uuid, target);
    }
}

/// Where one produced artifact should go.
pub enum UploadTarget {
    /// PUT the artifact here.
    Url(String),
    /// Nothing to upload to; skip this artifact and carry on with the rest.
    Skip,
    /// The report target is closed, gone, or refusing writes. Abandon the
    /// remaining uploads and go straight to committing the result.
    Stop,
}

/// The per-unit execution context: a working directory, a cancellation token,
/// and the client that reports it.
pub struct Executor {
    /// Cancelled when the owning worker/agent is shutting down, or when the
    /// coordinator has told us this unit's owner is closed.
    pub cancel_token: CancellationToken,
    /// How long to wait before retrying a failed coordinator call.
    pub polling_interval: std::time::Duration,
    /// The process's working set: `result/`, `exec/` and `resource/` live here,
    /// as do the archives built from them. Never shared between concurrent
    /// units.
    pub cache_path: PathBuf,
    /// Plain HTTP client for presigned S3 transfers. Carries no credential —
    /// the coordinator-facing one lives in `client`.
    pub http_client: Client,
    pub client: Box<dyn ExecClient>,
}

impl Executor {
    async fn announce(&mut self, state: TaskExecState) {
        self.client.announce_state(state, None).await
    }

    async fn announce_ex(&mut self, state: TaskExecState, ex: u64) {
        self.client.announce_state(state, Some(ex)).await
    }

    /// Report a unit that never ran: unsuccessful, zero exit status, and a
    /// message saying why. The caller announces the states around it.
    async fn report_abandoned(&mut self, msg: TaskResultMessage) -> crate::error::Result<()> {
        let result = TaskResultSpec {
            exit_status: 0,
            msg: Some(msg),
        };
        self.client.report_finish(false, &result).await?;
        self.client.report_commit(result).await
    }
}

/// Create (or recreate, discarding whatever the last unit left) the working
/// directories a unit's process expects.
pub async fn reset_workspace(cache_path: &std::path::Path) -> crate::error::Result<()> {
    let _ = tokio::fs::remove_dir_all(cache_path).await;
    tokio::fs::create_dir_all(cache_path.join("result")).await?;
    tokio::fs::create_dir_all(cache_path.join("exec")).await?;
    tokio::fs::create_dir_all(cache_path.join("resource")).await?;
    Ok(())
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

enum ExecResult {
    Finish(ProcessOutput),
    Timeout(ProcessOutput),
}

impl ExecResult {
    fn state(&self) -> (bool, ExitStatus) {
        match self {
            ExecResult::Finish(output) => (true, output.get_exit_status()),
            ExecResult::Timeout(output) => (false, output.get_exit_status()),
        }
    }

    fn get_output(self) -> ProcessOutput {
        match self {
            ExecResult::Finish(output) => output,
            ExecResult::Timeout(output) => output,
        }
    }
}

enum ResourceError {
    /// 404: the resource does not exist.
    NotFound,
    /// 403: the resource is forbidden.
    ForbiddenStatus,
    /// The presigned S3 download itself failed.
    DownloadFailed,
    /// Exceeded the per-download or the overall fetch budget.
    Timeout,
    /// Cancelled via the shutdown token.
    Cancelled,
    /// Connection error or unexpected status — propagate to the caller.
    Other(Error),
}

/// Fetch one input to its local path, with connection retry, per-download and
/// overall budgets, and cancellation. Touches no unit lifecycle: every failure
/// comes back as a [`ResourceError`] for [`execute`] to turn into an outcome.
async fn download_resource(
    executor: &mut Executor,
    resource: RemoteResourceDownload,
    timeout_until: Instant,
) -> std::result::Result<(), ResourceError> {
    let resp = loop {
        // The request (URL + credential) is the only worker/agent-specific part;
        // the retry, timeout and status handling below are shared.
        let req = match &resource.remote_file {
            RemoteResource::Artifact { uuid, content_type } => {
                executor.client.artifact_download_req(*uuid, *content_type)
            }
            RemoteResource::Attachment { key } => executor.client.attachment_download_req(key),
        };
        match req.send().await {
            Ok(resp) => break resp,
            Err(e) => {
                if e.is_connect() && e.is_request() {
                    tracing::error!(
                        "Fetch resource info failed with connection error: {}. Retry after {:?}",
                        e,
                        executor.polling_interval
                    );
                    tokio::select! {
                        biased;
                        _ = executor.cancel_token.cancelled() => return Err(ResourceError::Cancelled),
                        _ = tokio::time::sleep(executor.polling_interval) => {},
                        _ = tokio::time::sleep_until(timeout_until) => {
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
        let local_path = executor
            .cache_path
            .join("resource")
            .join(resource.local_path);
        tokio::select! {
            biased;
            res = download_file(&executor.http_client, &download_resp, local_path, false) => {
                if let Err(e) = res {
                    tracing::error!("Failed to download resource: {}", e);
                    return Err(ResourceError::DownloadFailed);
                }
                Ok(())
            }
            _ = executor.cancel_token.cancelled() => Err(ResourceError::Cancelled),
            _ = tokio::time::sleep(SINGLE_DOWNLOAD_BUDGET) => Err(ResourceError::Timeout),
            _ = tokio::time::sleep_until(timeout_until) => Err(ResourceError::Timeout),
        }
    } else if resp.status() == StatusCode::NOT_FOUND {
        Err(ResourceError::NotFound)
    } else if resp.status() == StatusCode::FORBIDDEN {
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

/// Run one unit to completion and report it.
///
/// `Ok(())` means "the unit was handled", including the cases where it was
/// abandoned (a missing input, a timeout) and committed as such. An `Err` is an
/// infrastructure failure the caller should treat as fatal for its own loop.
pub async fn execute(
    executor: &mut Executor,
    spec: ExecSpec,
    exec_options: Option<&TaskExecOptions>,
) -> crate::error::Result<()> {
    executor
        .announce_ex(TaskExecState::FetchResource, 360)
        .await;
    let timeout_until = tokio::time::Instant::now() + RESOURCE_FETCH_BUDGET;
    for resource in spec.resources {
        match download_resource(executor, resource, timeout_until).await {
            Ok(()) => {}
            // Shutdown while fetching — just stop; teardown reclaims the unit.
            Err(ResourceError::Cancelled) => return Ok(()),
            // Connection error or unexpected status — surface as a hard error.
            Err(ResourceError::Other(e)) => {
                executor
                    .announce_ex(TaskExecState::FetchResourceError, 60)
                    .await;
                return Err(e);
            }
            Err(kind) => {
                let (state, msg) = match kind {
                    ResourceError::NotFound => (
                        TaskExecState::FetchResourceNotFound,
                        TaskResultMessage::ResourceNotFound,
                    ),
                    ResourceError::ForbiddenStatus | ResourceError::DownloadFailed => (
                        TaskExecState::FetchResourceForbidden,
                        TaskResultMessage::ResourceForbidden,
                    ),
                    _ => (
                        TaskExecState::FetchResourceTimeout,
                        TaskResultMessage::FetchResourceTimeout,
                    ),
                };
                tracing::debug!(
                    "Input unavailable for {}, commit it as cancelled",
                    executor.client.describe()
                );
                executor.report_abandoned(msg).await?;
                executor.announce_ex(state, 60).await;
                executor.announce_ex(TaskExecState::TaskCommitted, 60).await;
                return Ok(());
            }
        }
    }

    if let Some((watched_uuid, watched_state)) = exec_options.and_then(|opts| opts.watch) {
        // Wait for another task to reach a state before running this one.
        if executor.client.can_watch() {
            executor.announce(TaskExecState::Watch).await;
            let cancel_token = executor.cancel_token.clone();
            tokio::select! {
                biased;
                _ = cancel_token.cancelled() => {
                    tracing::info!("Watching interrupted by shutdown signal");
                    executor.announce_ex(TaskExecState::WorkerExited, 60).await;
                    return Ok(());
                },
                _ = executor.client.watch(&watched_uuid, watched_state) => {},
                _ = tokio::time::sleep_until(timeout_until) => {
                    tracing::debug!("Watching timeout, commit this task as cancelled");
                    executor.report_abandoned(TaskResultMessage::WatchTimeout).await?;
                    executor.announce_ex(TaskExecState::WatchTimeout, 60).await;
                    executor.announce_ex(TaskExecState::TaskCommitted, 60).await;
                    return Ok(());
                }
            }
        }
    }

    // No timeout means no deadline
    let exec_timeout = spec
        .timeout
        .and_then(|t| u64::try_from(t).ok())
        .map(std::time::Duration::from_secs);
    match exec_timeout {
        // Outlive the deadline by a little, so a watcher sees `ExecPending` for
        // the whole run rather than losing it just before the timeout fires.
        Some(timeout) => {
            executor
                .announce_ex(TaskExecState::ExecPending, timeout.as_secs() + 60)
                .await
        }
        // Nothing to expire against: the state stands until the next announce.
        None => executor.announce(TaskExecState::ExecPending).await,
    }

    // Setup the child-task hand-off file and clean up any stale one
    let new_task_path = executor.cache_path.join("new_task.json");
    let _ = tokio::fs::remove_file(&new_task_path).await; // Ignore errors if file doesn't exist

    let mut command = Command::new("/usr/bin/env");
    command
        .args(spec.args)
        .envs(spec.envs)
        .envs(executor.client.exec_env())
        .env("MITO_RESULT_DIR", executor.cache_path.join("result"))
        .env("MITO_EXEC_DIR", executor.cache_path.join("exec"))
        .env("MITO_RESOURCE_DIR", executor.cache_path.join("resource"))
        .stdin(std::process::Stdio::null());
    if executor.client.supports_child_tasks() {
        command.env("MITO_NEW_TASK", &new_task_path);
    }
    if spec.terminal_output {
        command
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
    } else {
        command
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
    }

    let mut child = command.spawn().inspect_err(|e| {
        tracing::error!("Failed to spawn {}: {}", executor.client.describe(), e);
    })?;
    executor.announce(TaskExecState::ExecSpawned).await;
    // A deadline only when one was asked for; otherwise a future that never
    // resolves, so the arm below simply never fires.
    let exec_deadline = async move {
        match exec_timeout {
            Some(timeout) => tokio::time::sleep(timeout).await,
            None => std::future::pending().await,
        }
    };
    let process_output = async {
        if spec.terminal_output {
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
        _ = executor.cancel_token.cancelled() => {
            tracing::info!("Execution interrupted by shutdown signal");
            child.kill().await.inspect_err(|e| {
                tracing::error!("Failed to kill the process: {}", e);
            })?;
            executor.announce_ex(TaskExecState::WorkerExited, 60).await;
            return Ok(());
        },
        output = process_output => {
            executor.announce_ex(TaskExecState::ExecFinished, 660).await;
            output.map(ExecResult::Finish)
        },
        _ = exec_deadline => {
            tracing::debug!("Execution timeout");
            executor.announce_ex(TaskExecState::ExecTimeout, 60).await;
            if let Some(id) = child.id() {
                // TODO: we may change this when once the `linux_pidfd` is stabilized in standard library
                // Tracking issue for std lib: [rust-lang/rust #82971](https://github.com/rust-lang/rust/issues/82971)
                // Tracking issue for tokio: [tokio-rs/tokio #6281](https://github.com/tokio-rs/tokio/issues/6281)
                let _ = signal::kill(Pid::from_raw(id as i32), Signal::SIGTERM).inspect_err(|e| {
                    tracing::error!("Failed to send SIGTERM to the process: {}", e);
                });
            }
            tokio::select! {
                biased;
                _ = child.wait() => {},
                _ = executor.cancel_token.cancelled() => {
                    child.kill().await.inspect_err(|e| {
                        tracing::error!("Failed to kill the process: {}", e);
                    })?;
                    return Ok(());
                },
                _ = tokio::time::sleep(std::time::Duration::from_secs(10)) => {
                    child.kill().await.inspect_err(|e| {
                        tracing::error!("Failed to kill the process: {}", e);
                    })?;
                },
            }
            if spec.terminal_output {
                let output = child.wait_with_output().await?;
                Ok(ExecResult::Timeout(ProcessOutput::WithLog {
                    stdout: output.stdout,
                    stderr: output.stderr,
                    exit_status: output.status,
                }))
            } else {
                let exit_status = child.wait().await?;
                Ok(ExecResult::Timeout(ProcessOutput::WithoutLog {
                    exit_status,
                }))
            }
        },
    }?;
    tracing::debug!("Execution of {} finished", executor.client.describe());
    executor.announce_ex(TaskExecState::UploadResult, 660).await;
    process_exec_result(executor, output).await?;
    Ok(())
}

/// Archive whatever the process produced, upload it, then commit the outcome.
async fn process_exec_result(
    executor: &mut Executor,
    output: ExecResult,
) -> crate::error::Result<()> {
    let (is_finished, exit_status) = output.state();
    executor
        .announce_ex(
            if is_finished {
                TaskExecState::UploadFinishedResult
            } else {
                TaskExecState::UploadCancelledResult
            },
            660,
        )
        .await;
    executor
        .client
        .report_finish(
            is_finished,
            &TaskResultSpec {
                exit_status: exit_status.into_raw(),
                msg: (!is_finished).then_some(TaskResultMessage::ExecTimeout),
            },
        )
        .await?;

    // Compress possible output and upload
    let (tx, mut rx) = mpsc::channel::<(ArtifactContentType, u64)>(3);
    // Spawn a task to archive the output
    let timeout_cancel_token = CancellationToken::new();
    let archive_timeout_cancel_token = timeout_cancel_token.clone();
    let archive_cancel_token = executor.cancel_token.clone();
    let archive_cache_path = executor.cache_path.clone();
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
                    tracing::info!("Output generation interrupted by shutdown signal");
                    return Ok(());
                }
                _ = archive_timeout_cancel_token.cancelled() => {
                    let mut encoder = ar.into_inner().await?;
                    encoder.shutdown().await?;
                    tokio::fs::remove_file(archive_cache_path.join("result.tar.gz")).await?;
                    tracing::warn!("Output generation timeout");
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
                    tracing::info!("Output generation interrupted by shutdown signal");
                    return Ok(());
                }
                _ = archive_timeout_cancel_token.cancelled() => {
                    let mut encoder = ar.into_inner().await?;
                    encoder.shutdown().await?;
                    tokio::fs::remove_file(archive_cache_path.join(&file_name)).await?;
                    tracing::warn!("Output generation timeout");
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
                    tracing::info!("Output generation interrupted by shutdown signal");
                    return Ok(());
                }
                _ = archive_timeout_cancel_token.cancelled() => {
                    let mut encoder = ar.into_inner().await?;
                    encoder.shutdown().await?;
                    tokio::fs::remove_file(archive_cache_path.join("std-log.tar.gz")).await?;
                    tracing::warn!("Output generation timeout");
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
            let url = match executor
                .client
                .request_upload(content_type, content_length)
                .await?
            {
                UploadTarget::Url(url) => url,
                UploadTarget::Skip => continue,
                UploadTarget::Stop => return Ok(()),
            };
            loop {
                let file =
                    tokio::fs::File::open(executor.cache_path.join(content_type.to_string()))
                        .await?;
                let upload_file = executor
                    .http_client
                    .put(url.as_str())
                    .header(CONTENT_LENGTH, content_length)
                    .body(file)
                    .send();
                let resp = tokio::select! {
                    biased;
                    _ = executor.cancel_token.cancelled() => {
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
                                executor.polling_interval
                            );
                            tokio::select! {
                                biased;
                                _ = executor.cancel_token.cancelled() => return Ok(()),
                                _ = timeout_cancel_token.cancelled() => {
                                    tracing::warn!("Upload failed with timeout");
                                    return Ok(());
                                }
                                _ = tokio::time::sleep(executor.polling_interval) => {},
                            }
                            continue;
                        } else {
                            return Err(RequestError::from(e).into());
                        }
                    }
                }
            }
        }
        crate::error::Result::Ok(())
    };
    let timeout_until = tokio::time::Instant::now() + UPLOAD_BUDGET;
    tokio::select! {
        biased;
        _ = tokio::time::sleep_until(timeout_until) => {
            tracing::warn!("Upload result timeout");
            timeout_cancel_token.cancel();
            executor
                .client
                .report_commit(TaskResultSpec {
                    exit_status: exit_status.into_raw(),
                    msg: Some(TaskResultMessage::UploadResultTimeout),
                })
                .await?;
            executor.announce_ex(TaskExecState::UploadResultTimeout, 60).await;
            executor.announce_ex(TaskExecState::TaskCommitted, 60).await;
            archive_hd.await??;
        }
        res = upload_artifact_fut => {
            res?;
            archive_hd.await??;
            if executor.cancel_token.is_cancelled() {
                tracing::info!("Execution interrupted by shutdown signal");
                executor.announce_ex(TaskExecState::WorkerExited, 60).await;
                return Ok(());
            }
            executor.announce_ex(TaskExecState::UploadResultFinished, 60).await;

            let child_task_submitted = submit_child_task_if_present(executor).await;
            let msg = if is_finished {
                if child_task_submitted {
                    None
                } else {
                    Some(TaskResultMessage::SubmitNewTaskFailed)
                }
            } else {
                Some(TaskResultMessage::ExecTimeout)
            };
            executor
                .client
                .report_commit(TaskResultSpec {
                    exit_status: exit_status.into_raw(),
                    msg,
                })
                .await?;
            executor.announce_ex(TaskExecState::TaskCommitted, 60).await;
        }
    }

    // Uploaded and committed: leave a clean directory for the next unit.
    reset_workspace(&executor.cache_path).await?;
    Ok(())
}

/// Submit the task the process left behind in `new_task.json`, if it left one.
/// A false answer becomes a `SubmitNewTaskFailed` message on the unit's result.
async fn submit_child_task_if_present(executor: &mut Executor) -> bool {
    if !executor.client.supports_child_tasks() {
        return true;
    }
    let new_task_path = executor.cache_path.join("new_task.json");
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

    let submit_req: SubmitTaskReq = match serde_json::from_str(&new_task_content) {
        Ok(req) => req,
        Err(e) => {
            tracing::warn!("Failed to parse new task JSON: {}", e);
            let _ = tokio::fs::remove_file(&new_task_path).await;
            return false;
        }
    };

    tracing::debug!(
        "Submitting a new task from {} to group '{}'",
        executor.client.describe(),
        submit_req.group_name
    );
    let submitted = executor.client.submit_child_task(submit_req).await;
    // Always clean up the file after processing (success or failure)
    let _ = tokio::fs::remove_file(&new_task_path).await;
    match submitted {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!("Failed to submit new task: {}", e);
            false
        }
    }
}
