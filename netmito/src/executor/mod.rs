//! Runs one task. The executor gets the task inputs, starts the process, archives
//! and uploads the output, and then commits the result to the coordinator.

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

// Default time budgets. Each executor keeps its own copy, so a runner can set
// different budgets on each executor that it builds.
/// The maximum time to get all inputs of a task.
pub(crate) const RESOURCE_FETCH_BUDGET: std::time::Duration = std::time::Duration::from_secs(1800);
/// The maximum time to download one input.
pub(crate) const SINGLE_DOWNLOAD_BUDGET: std::time::Duration = std::time::Duration::from_secs(120);
/// The maximum time to archive and upload the output after the process stops.
pub(crate) const UPLOAD_BUDGET: std::time::Duration = std::time::Duration::from_secs(600);

pub struct Executor {
    pub(crate) http_client: Client,
    pub(crate) task_credential: String,
    pub(crate) http_url: Url,
    pub(crate) task_cancel_token: CancellationToken,
    pub(crate) coordinator_force_exit: Arc<AtomicBool>,
    pub(crate) polling_interval: std::time::Duration,
    pub(crate) task_cache_path: PathBuf,
    pub(crate) task_redis_conn: Option<MultiplexedConnection>,
    pub(crate) task_redis_pubsub: Option<PubSub>,
    pub(crate) resource_fetch_budget: std::time::Duration,
    pub(crate) single_download_budget: std::time::Duration,
    pub(crate) upload_budget: std::time::Duration,
}

impl Executor {
    pub async fn execute(&mut self, task: WorkerTaskResp) -> crate::error::Result<()> {
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
                                _ = self.task_cancel_token.cancelled() => return Ok(()),
                                _ = tokio::time::sleep(self.polling_interval) => {},
                                _ = tokio::time::sleep_until(timeout_until) => {
                                    tracing::debug!("Fetching resource timeout, commit this task as canceled");
                                    let req = ReportTaskReq {
                                        id: task.id,
                                        op: ReportTaskOp::Cancel,
                                    };
                                    self.report_task(req).await?;
                                    let req = ReportTaskReq {
                                        id: task.id,
                                        op: ReportTaskOp::Commit(TaskResultSpec {
                                            exit_status: 0,
                                            msg: Some(TaskResultMessage::FetchResourceTimeout),
                                        }),
                                    };
                                    self.report_task(req).await?;
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
                                    return Ok(());
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
                let local_path = self
                    .task_cache_path
                    .join("resource")
                    .join(resource.local_path);
                tokio::select! {
                    biased;
                    res = download_file(&self.http_client, &download_resp, local_path, false) => {
                        if let Err(e) = res {
                            tracing::error!("Failed to download resource: {}", e);
                            let req = ReportTaskReq {
                                id: task.id,
                                op: ReportTaskOp::Cancel,
                            };
                            self.report_task(req).await?;
                            let req = ReportTaskReq {
                                id: task.id,
                                op: ReportTaskOp::Commit(TaskResultSpec {
                                    exit_status: 0,
                                    msg: Some(TaskResultMessage::ResourceForbidden),
                                }),
                            };
                            self.report_task(req).await?;
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
                            return Ok(());
                        }
                    }
                    _ = self.task_cancel_token.cancelled() => return Ok(()),
                    _ = tokio::time::sleep(self.single_download_budget) => {
                        tracing::debug!("Fetching resource timeout, commit this task as canceled");
                        let req = ReportTaskReq {
                            id: task.id,
                            op: ReportTaskOp::Cancel,
                        };
                        self.report_task(req).await?;
                        let req = ReportTaskReq {
                            id: task.id,
                            op: ReportTaskOp::Commit(TaskResultSpec {
                                exit_status: 0,
                                msg: Some(TaskResultMessage::FetchResourceTimeout),
                            }),
                        };
                        self.report_task(req).await?;
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
                        return Ok(());
                    }
                    _ = tokio::time::sleep_until(timeout_until) => {
                        tracing::debug!("Fetching resource timeout, commit this task as canceled");
                        let req = ReportTaskReq {
                            id: task.id,
                            op: ReportTaskOp::Cancel,
                        };
                        self.report_task(req).await?;
                        let req = ReportTaskReq {
                            id: task.id,
                            op: ReportTaskOp::Commit(TaskResultSpec {
                                exit_status: 0,
                                msg: Some(TaskResultMessage::FetchResourceTimeout),
                            }),
                        };
                        self.report_task(req).await?;
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
                        return Ok(());
                    }
                }
            } else if resp.status() == StatusCode::NOT_FOUND {
                tracing::debug!("Resource not found, commit this task as canceled");
                let req = ReportTaskReq {
                    id: task.id,
                    op: ReportTaskOp::Cancel,
                };
                self.report_task(req).await?;
                let req = ReportTaskReq {
                    id: task.id,
                    op: ReportTaskOp::Commit(TaskResultSpec {
                        exit_status: 0,
                        msg: Some(TaskResultMessage::ResourceNotFound),
                    }),
                };
                self.report_task(req).await?;
                self.announce_task_state_ex(
                    &task.uuid,
                    TaskExecState::FetchResourceNotFound as i32,
                    60,
                )
                .await;
                self.announce_task_state_ex(&task.uuid, TaskExecState::TaskCommitted as i32, 60)
                    .await;
                return Ok(());
            } else if resp.status() == StatusCode::FORBIDDEN {
                tracing::debug!(
                    "Resource is forbidden to be fetched, commit this task as canceled"
                );
                let req = ReportTaskReq {
                    id: task.id,
                    op: ReportTaskOp::Cancel,
                };
                self.report_task(req).await?;
                let req = ReportTaskReq {
                    id: task.id,
                    op: ReportTaskOp::Commit(TaskResultSpec {
                        exit_status: 0,
                        msg: Some(TaskResultMessage::ResourceForbidden),
                    }),
                };
                self.report_task(req).await?;
                self.announce_task_state_ex(
                    &task.uuid,
                    TaskExecState::FetchResourceForbidden as i32,
                    60,
                )
                .await;
                self.announce_task_state_ex(&task.uuid, TaskExecState::TaskCommitted as i32, 60)
                    .await;
                return Ok(());
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
            if self.task_redis_conn.is_some() && self.task_redis_pubsub.is_some() {
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
                        return Ok(());
                    },
                    _ = self.watch_task(&watched_task_uuid, watched_task_state) => {},
                    _ = tokio::time::sleep_until(timeout_until) => {
                        tracing::debug!("Watching timeout, commit this task as canceled");
                        self.unsubscribe_task_exec_state(&watched_task_uuid).await;
                        let req = ReportTaskReq {
                            id: task.id,
                            op: ReportTaskOp::Cancel,
                        };
                        self.report_task(req).await?;
                        let req = ReportTaskReq {
                            id: task.id,
                            op: ReportTaskOp::Commit(TaskResultSpec {
                                exit_status: 0,
                                msg: Some(TaskResultMessage::WatchTimeout),
                            }),
                        };
                        self.report_task(req).await?;
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
                        return Ok(());
                    }
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
            .env("MITO_RESULT_DIR", self.task_cache_path.join("result"))
            .env("MITO_EXEC_DIR", self.task_cache_path.join("exec"))
            .env("MITO_RESOURCE_DIR", self.task_cache_path.join("resource"))
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
        self.announce_task_state(&task.uuid, TaskExecState::ExecSpawned as i32)
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
            _ = self.task_cancel_token.cancelled() => {
                tracing::info!("Task execution interrupted by shutdown signal");
                child.kill().await.inspect_err(|e| {
                    tracing::error!("Failed to kill task: {}", e);
                })?;
                self
                    .announce_task_state_ex(&task.uuid, TaskExecState::WorkerExited as i32, 60)
                    .await;
                return Ok(());
            },
            output = process_output => {
                self
                    .announce_task_state_ex(&task.uuid, TaskExecState::ExecFinished as i32, 660)
                    .await;
                output.map(TaskResult::Finish)
            },
            _ =  tokio::time::sleep_until(timeout_until) => {
                tracing::debug!("Task execution timeout");
                self
                    .announce_task_state_ex(&task.uuid, TaskExecState::ExecTimeout as i32, 60)
                    .await;
                if let Some(id) = child.id() {
                    // TODO: Send the signal through a pidfd when `linux_pidfd` is stable.
                    // A raw PID can refer to a different process if the system uses the PID again.
                    // Tracking issue for std lib: [rust-lang/rust #82971](https://github.com/rust-lang/rust/issues/82971)
                    // Tracking issue for tokio: [tokio-rs/tokio #6281](https://github.com/tokio-rs/tokio/issues/6281)
                    let _ = signal::kill(Pid::from_raw(id as i32), Signal::SIGTERM).inspect_err(|e| {
                        tracing::error!("Failed to send SIGTERM to task: {}", e);
                    });
                }
                tokio::select! {
                    biased;
                    _ = child.wait() => {},
                    _ = self.task_cancel_token.cancelled() => {
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
        self.announce_task_state_ex(&task.uuid, TaskExecState::UploadResult as i32, 660)
            .await;
        self.process_task_result(task.id, task.uuid, output).await?;
        Ok(())
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
