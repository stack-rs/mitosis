//! Runs one task or one hook. The executor gets the inputs, starts the process,
//! archives and uploads the output, and then commits the result to the coordinator.

use std::path::PathBuf;
use std::process::ExitStatus;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use nix::sys::signal::{self, Signal};
use nix::unistd::Pid;
use redis::aio::{MultiplexedConnection, PubSub};
use reqwest::{Client, StatusCode};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::entity::state::TaskExecState;
use crate::error::{Error, ErrorMsg, RequestError};
use crate::schema::*;
use crate::service::s3::download_file;

mod report;
mod state;
mod upload;
mod workspace;

pub use workspace::{
    create_shared_dir, ensure_traversable_dir, reset_workspace, warn_unreachable_ancestors,
};

// Default time budgets. Each executor keeps its own copy, so a runner can set
// different budgets on each executor that it builds.
/// The maximum time to get all inputs of a task.
pub(crate) const RESOURCE_FETCH_BUDGET: std::time::Duration = std::time::Duration::from_secs(1800);
/// The maximum time to download one input.
pub(crate) const SINGLE_DOWNLOAD_BUDGET: std::time::Duration = std::time::Duration::from_secs(120);
/// The maximum time to archive and upload the output after the process stops.
pub(crate) const UPLOAD_BUDGET: std::time::Duration = std::time::Duration::from_secs(600);

/// What the executor runs. The kind sets the fields of the reports, and only a task can submit a
/// downstream task.
#[derive(Debug, Clone, Copy)]
pub enum ExecKind {
    /// A task. An agent gives the handle of the job that claimed the task. A worker gives `None`.
    Task { job: Option<i64> },
    /// A hook of an agent job.
    Hook,
}

impl ExecKind {
    fn report_req(self, id: i64, op: ReportTaskOp) -> ReportTaskReq {
        let (kind, job) = match self {
            ExecKind::Task { job } => (TaskKind::Task, job),
            ExecKind::Hook => (TaskKind::Hook, None),
        };
        ReportTaskReq { id, kind, job, op }
    }
}

pub struct Executor {
    pub(crate) http_client: Client,
    pub(crate) task_credential: String,
    pub(crate) http_url: Url,
    /// Stops the execution. The executor kills the process and reports nothing.
    pub(crate) task_cancel_token: CancellationToken,
    /// Stops the process, then reports it with its output: a SIGTERM to the process group, and a
    /// SIGKILL if the process does not stop in a grace time. The result has the message
    /// `StoppedByAgent`. A worker gives a token that it does not cancel.
    pub(crate) task_stop_token: CancellationToken,
    /// Stops the worker or the agent. The executor cancels it when the coordinator rejects the
    /// credential. A worker gives the same token as `task_cancel_token`.
    pub(crate) runner_cancel_token: CancellationToken,
    pub(crate) coordinator_force_exit: Arc<AtomicBool>,
    pub(crate) polling_interval: std::time::Duration,
    pub(crate) task_cache_path: PathBuf,
    /// More environment variables for the process.
    pub(crate) extra_envs: Vec<(String, String)>,
    pub(crate) task_redis_conn: Option<MultiplexedConnection>,
    pub(crate) task_redis_pubsub: Option<PubSub>,
    pub(crate) resource_fetch_budget: std::time::Duration,
    pub(crate) single_download_budget: std::time::Duration,
    pub(crate) upload_budget: std::time::Duration,
}

impl Executor {
    /// Runs `task` and reports it as `kind`. For a hook, `task` holds the data of the hook.
    ///
    /// Returns the committed result, or `None` if the execution stopped before the commit.
    pub async fn execute(
        &mut self,
        kind: ExecKind,
        task: WorkerTaskResp,
    ) -> crate::error::Result<Option<TaskResultSpec>> {
        self.announce_task_state_ex(&task.uuid, TaskExecState::FetchResource as i32, 360)
            .await;
        let timeout_until = tokio::time::Instant::now() + self.resource_fetch_budget;
        for resource in task.spec.resources {
            match resource.remote_file {
                RemoteResource::Artifact { uuid, content_type } => {
                    let content_serde_val = serde_json::to_value(content_type)?;
                    let content_serde_str = content_serde_val.as_str().unwrap_or("result");
                    self.http_url.set_path(&format!(
                        "tasks/{uuid}/download/artifacts/{content_serde_str}"
                    ));
                }
                RemoteResource::Attachment { key } => {
                    self.http_url.set_path(&format!(
                        "groups/{}/download/attachments/{key}",
                        task.group_name
                    ));
                }
            };
            let resp = loop {
                match self
                    .http_client
                    .get(self.http_url.as_str())
                    .bearer_auth(&self.task_credential)
                    .send()
                    .await
                {
                    Ok(resp) => break resp,
                    Err(e) => {
                        if e.is_connect() && e.is_request() {
                            tracing::error!(
                                "Fetch resource info failed with connection error: {}. Retry after {:?}",
                                e,
                                self.polling_interval
                            );
                            tokio::select! {
                                biased;
                                _ = self.task_cancel_token.cancelled() => return Ok(None),
                                _ = self.task_stop_token.cancelled() => return Ok(None),
                                _ = tokio::time::sleep(self.polling_interval) => {},
                                _ = tokio::time::sleep_until(timeout_until) => {
                                    tracing::debug!("Fetching resource timeout, commit this task as canceled");
                                    let result = TaskResultSpec {
                                        exit_status: 0,
                                        msg: Some(TaskResultMessage::FetchResourceTimeout),
                                    };
                                    self.report_task(kind, task.id, ReportTaskOp::Cancel).await?;
                                    self.report_task(kind, task.id, ReportTaskOp::Commit(result.clone()))
                                        .await?;
                                    self
                                        .announce_task_state_ex(
                                            &task.uuid,
                                            TaskExecState::FetchResourceTimeout as i32,
                                            60,
                                        )
                                        .await;
                                    self
                                        .announce_task_state_ex(
                                            &task.uuid,
                                            TaskExecState::TaskCommitted as i32,
                                            60,
                                        )
                                        .await;
                                    return Ok(Some(result));
                                }
                            }
                            continue;
                        } else {
                            self.announce_task_state_ex(
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
                let resource_root = self.task_cache_path.join("resource");
                let local_path = resource_root.join(resource.local_path);
                tokio::select! {
                    biased;
                    res = download_file(&self.http_client, &download_resp, &local_path, false) => {
                        if let Err(e) = res {
                            tracing::error!("Failed to download resource: {}", e);
                            let result = TaskResultSpec {
                                exit_status: 0,
                                msg: Some(TaskResultMessage::ResourceForbidden),
                            };
                            self.report_task(kind, task.id, ReportTaskOp::Cancel).await?;
                            self.report_task(kind, task.id, ReportTaskOp::Commit(result.clone()))
                                .await?;
                            self
                                .announce_task_state(
                                    &task.uuid,
                                    TaskExecState::FetchResourceForbidden as i32,
                                )
                                .await;
                            self
                                .announce_task_state_ex(
                                    &task.uuid,
                                    TaskExecState::TaskCommitted as i32,
                                    60,
                                )
                                .await;
                            return Ok(Some(result));
                        }
                        workspace::share_fetched_resource(&resource_root, &local_path).await;
                    }
                    _ = self.task_cancel_token.cancelled() => return Ok(None),
                    _ = self.task_stop_token.cancelled() => return Ok(None),
                    _ = tokio::time::sleep(self.single_download_budget) => {
                        tracing::debug!("Fetching resource timeout, commit this task as canceled");
                        let result = TaskResultSpec {
                            exit_status: 0,
                            msg: Some(TaskResultMessage::FetchResourceTimeout),
                        };
                        self.report_task(kind, task.id, ReportTaskOp::Cancel).await?;
                        self.report_task(kind, task.id, ReportTaskOp::Commit(result.clone()))
                            .await?;
                        self
                            .announce_task_state_ex(
                                &task.uuid,
                                TaskExecState::FetchResourceTimeout as i32,
                                60,
                            )
                            .await;
                        self
                            .announce_task_state_ex(&task.uuid, TaskExecState::TaskCommitted as i32, 60)
                            .await;
                        return Ok(Some(result));
                    }
                    _ = tokio::time::sleep_until(timeout_until) => {
                        tracing::debug!("Fetching resource timeout, commit this task as canceled");
                        let result = TaskResultSpec {
                            exit_status: 0,
                            msg: Some(TaskResultMessage::FetchResourceTimeout),
                        };
                        self.report_task(kind, task.id, ReportTaskOp::Cancel).await?;
                        self.report_task(kind, task.id, ReportTaskOp::Commit(result.clone()))
                            .await?;
                        self
                            .announce_task_state_ex(
                                &task.uuid,
                                TaskExecState::FetchResourceTimeout as i32,
                                60,
                            )
                            .await;
                        self
                            .announce_task_state_ex(&task.uuid, TaskExecState::TaskCommitted as i32, 60)
                            .await;
                        return Ok(Some(result));
                    }
                }
            } else if resp.status() == StatusCode::NOT_FOUND {
                tracing::debug!("Resource not found, commit this task as canceled");
                let result = TaskResultSpec {
                    exit_status: 0,
                    msg: Some(TaskResultMessage::ResourceNotFound),
                };
                self.report_task(kind, task.id, ReportTaskOp::Cancel)
                    .await?;
                self.report_task(kind, task.id, ReportTaskOp::Commit(result.clone()))
                    .await?;
                self.announce_task_state_ex(
                    &task.uuid,
                    TaskExecState::FetchResourceNotFound as i32,
                    60,
                )
                .await;
                self.announce_task_state_ex(&task.uuid, TaskExecState::TaskCommitted as i32, 60)
                    .await;
                return Ok(Some(result));
            } else if resp.status() == StatusCode::FORBIDDEN {
                tracing::debug!(
                    "Resource is forbidden to be fetched, commit this task as canceled"
                );
                let result = TaskResultSpec {
                    exit_status: 0,
                    msg: Some(TaskResultMessage::ResourceForbidden),
                };
                self.report_task(kind, task.id, ReportTaskOp::Cancel)
                    .await?;
                self.report_task(kind, task.id, ReportTaskOp::Commit(result.clone()))
                    .await?;
                self.announce_task_state_ex(
                    &task.uuid,
                    TaskExecState::FetchResourceForbidden as i32,
                    60,
                )
                .await;
                self.announce_task_state_ex(&task.uuid, TaskExecState::TaskCommitted as i32, 60)
                    .await;
                return Ok(Some(result));
            } else {
                let resp: ErrorMsg = resp
                    .json()
                    .await
                    .unwrap_or_else(|e| ErrorMsg { msg: e.to_string() });
                self.announce_task_state_ex(
                    &task.uuid,
                    TaskExecState::FetchResourceError as i32,
                    60,
                )
                .await;
                return Err(Error::Custom(format!(
                    "Fetch resource info failed with error: {}",
                    resp.msg
                )));
            }
        }

        if let Some((watched_task_uuid, watched_task_state)) =
            task.exec_options.as_ref().and_then(|o| o.watch)
        {
            // Wait until the watched task reaches the specified state. Then start this task.
            // Without Redis, the executor asks the coordinator for the state at intervals.
            self.announce_task_state(&task.uuid, TaskExecState::Watch as i32)
                .await;
            let tmp_cancel_token = self.task_cancel_token.clone();
            tokio::select! {
                biased;
                _ = tmp_cancel_token.cancelled() => {
                    tracing::info!("Task watching interrupted by shutdown signal");
                    self.unsubscribe_task_exec_state(&watched_task_uuid).await;
                    self
                        .announce_task_state_ex(&task.uuid, TaskExecState::WorkerExited as i32, 60)
                        .await;
                    return Ok(None);
                },
                _ = self.watch_task(&watched_task_uuid, watched_task_state) => {},
                _ = tokio::time::sleep_until(timeout_until) => {
                    tracing::debug!("Watching timeout, commit this task as canceled");
                    self.unsubscribe_task_exec_state(&watched_task_uuid).await;
                    let result = TaskResultSpec {
                        exit_status: 0,
                        msg: Some(TaskResultMessage::WatchTimeout),
                    };
                    self.report_task(kind, task.id, ReportTaskOp::Cancel).await?;
                    self.report_task(kind, task.id, ReportTaskOp::Commit(result.clone()))
                        .await?;
                    self
                        .announce_task_state_ex(
                            &task.uuid,
                            TaskExecState::WatchTimeout as i32,
                            60,
                        )
                        .await;
                    self
                        .announce_task_state_ex(&task.uuid, TaskExecState::TaskCommitted as i32, 60)
                        .await;
                    return Ok(Some(result));
                }
            }
        }

        // TODO: If the spec has no timeout, let the process run with no time limit.
        let exec_timeout = task
            .spec
            .timeout
            .and_then(|t| u64::try_from(t).ok())
            .unwrap_or(600);
        let exec_timeout = std::time::Duration::from_secs(exec_timeout);
        self.announce_task_state_ex(
            &task.uuid,
            TaskExecState::ExecPending as i32,
            exec_timeout.as_secs() + 60,
        )
        .await;
        let timeout_until = tokio::time::Instant::now() + exec_timeout;

        // The process writes a downstream task to this file. Remove a file that a
        // previous task left. A missing file is not an error.
        let new_task_path = self.task_cache_path.join("new_task.json");
        let _ = tokio::fs::remove_file(&new_task_path).await;

        let mut command = Command::new("/usr/bin/env");
        command
            .args(task.spec.args)
            .envs(task.spec.envs)
            .envs(self.extra_envs.iter().cloned())
            .env("MITO_RESULT_DIR", self.task_cache_path.join("result"))
            .env("MITO_EXEC_DIR", self.task_cache_path.join("exec"))
            .env("MITO_RESOURCE_DIR", self.task_cache_path.join("resource"))
            .env("MITO_TASK_UUID", task.uuid.to_string())
            .stdin(std::process::Stdio::null());
        // The task leads its own process group. Thus a kill of the full group from the task (for
        // example, a task that removes its namespaces when it stops) does not reach the worker or
        // the agent. The timeout SIGTERM also reaches all processes of the task.
        #[cfg(unix)]
        command.process_group(0);
        if let ExecKind::Task { .. } = kind {
            command.env("MITO_NEW_TASK", &new_task_path);
        }
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

        // A stop before the process starts ends the execution with no report, as a cancel does.
        if self.task_stop_token.is_cancelled() {
            return Ok(None);
        }
        let mut child = command.spawn().inspect_err(|e| {
            tracing::error!("Failed to spawn task: {}", e);
        })?;
        let pid = child.id();
        self.announce_task_state(&task.uuid, TaskExecState::ExecSpawned as i32)
            .await;
        // Boxed, thus the executor can drop it to use `child` again. After a SIGTERM, the executor
        // continues to await this future. Thus the output that it read before the signal stays.
        let mut process_output = Box::pin(async {
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
        });

        let end = tokio::select! {
            biased;
            _ = self.task_cancel_token.cancelled() => {
                tracing::info!("Task execution interrupted by shutdown signal");
                drop(process_output);
                // The task has its own process group, thus a Ctrl-C at the terminal does not reach
                // it. Kill the group, or the processes of the task continue after the runner stops.
                #[cfg(unix)]
                if let Some(id) = child.id() {
                    let _ = signal::kill(Pid::from_raw(-(id as i32)), Signal::SIGKILL);
                }
                child.kill().await.inspect_err(|e| {
                    tracing::error!("Failed to kill task: {}", e);
                })?;
                self
                    .announce_task_state_ex(&task.uuid, TaskExecState::WorkerExited as i32, 60)
                    .await;
                return Ok(None);
            },
            output = &mut process_output => ProcessEnd::Exited(output),
            _ = tokio::time::sleep_until(timeout_until) => ProcessEnd::Timeout,
            _ = self.task_stop_token.cancelled() => ProcessEnd::Stopped,
        };
        let output = match end {
            ProcessEnd::Exited(output) => {
                self.announce_task_state_ex(&task.uuid, TaskExecState::ExecFinished as i32, 660)
                    .await;
                TaskResult::Finish(output?)
            }
            ProcessEnd::Timeout | ProcessEnd::Stopped => {
                if let ProcessEnd::Timeout = end {
                    tracing::debug!("Task execution timeout");
                    self.announce_task_state_ex(&task.uuid, TaskExecState::ExecTimeout as i32, 60)
                        .await;
                } else {
                    tracing::info!("Task execution stopped by the agent");
                }
                if let Some(id) = pid {
                    // TODO: Send the signal through a pidfd when `linux_pidfd` is stable.
                    // A raw PID can refer to a different process if the system uses the PID again.
                    // Tracking issue for std lib: [rust-lang/rust #82971](https://github.com/rust-lang/rust/issues/82971)
                    // Tracking issue for tokio: [tokio-rs/tokio #6281](https://github.com/tokio-rs/tokio/issues/6281)
                    // A negative PID sends the signal to the process group. The task leads its own
                    // group, thus the signal reaches all processes of the task, not only `env`.
                    let _ = signal::kill(Pid::from_raw(-(id as i32)), Signal::SIGTERM).inspect_err(
                        |e| {
                            tracing::error!(
                                "Failed to send SIGTERM to the task process group: {}",
                                e
                            );
                        },
                    );
                }
                let stopped = tokio::select! {
                    biased;
                    output = &mut process_output => Some(output),
                    _ = self.task_cancel_token.cancelled() => {
                        drop(process_output);
                        child.kill().await.inspect_err(|e| {
                            tracing::error!("Failed to kill task: {}", e);
                        })?;
                        return Ok(None);
                    },
                    _ = tokio::time::sleep(std::time::Duration::from_secs(10)) => None,
                };
                let output = match stopped {
                    Some(output) => output?,
                    None => {
                        // The process did not stop in the grace time. Kill its group. A process
                        // that left the group can keep the pipes open, thus wait only a short time
                        // for the remaining output.
                        if let Some(id) = pid {
                            let _ = signal::kill(Pid::from_raw(-(id as i32)), Signal::SIGKILL);
                        }
                        let killed = tokio::select! {
                            output = &mut process_output => Some(output),
                            _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => None,
                        };
                        match killed {
                            Some(output) => output?,
                            None => {
                                drop(process_output);
                                child.kill().await.inspect_err(|e| {
                                    tracing::error!("Failed to kill task: {}", e);
                                })?;
                                let exit_status = child.wait().await?;
                                if task.spec.terminal_output {
                                    ProcessOutput::WithLog {
                                        stdout: Vec::new(),
                                        stderr: Vec::new(),
                                        exit_status,
                                    }
                                } else {
                                    ProcessOutput::WithoutLog { exit_status }
                                }
                            }
                        }
                    }
                };
                if let ProcessEnd::Timeout = end {
                    TaskResult::Timeout(output)
                } else {
                    TaskResult::Stopped(output)
                }
            }
        };
        tracing::debug!("Task execution finished");
        self.announce_task_state_ex(&task.uuid, TaskExecState::UploadResult as i32, 660)
            .await;
        self.process_task_result(kind, task.id, task.uuid, output)
            .await
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

/// How the process ended, before the executor collects its output.
enum ProcessEnd {
    Exited(std::io::Result<ProcessOutput>),
    Timeout,
    Stopped,
}

enum TaskResult {
    Finish(ProcessOutput),
    Timeout(ProcessOutput),
    /// The agent stopped the process with `task_stop_token`.
    Stopped(ProcessOutput),
}

impl TaskResult {
    fn get_output(self) -> ProcessOutput {
        match self {
            TaskResult::Finish(output) => output,
            TaskResult::Timeout(output) => output,
            TaskResult::Stopped(output) => output,
        }
    }
}
