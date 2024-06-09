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
use tokio_tar::{Builder, Header};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::fmt::writer::MakeWriterExt;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use url::Url;

use crate::entity::content::ArtifactContentType;
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
    cache_path: PathBuf,
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
        match Self::setup(&cli).await {
            Ok((mut worker, _guards)) => {
                if let Err(e) = worker.run().await {
                    tracing::error!("{}", e);
                }
                worker.cleanup().await;
            }
            Err(e) => {
                tracing::error!("{}", e);
            }
        }
    }

    pub async fn setup(
        cli: &WorkerConfigCli,
    ) -> crate::error::Result<(
        Self,
        Option<(
            tracing::subscriber::DefaultGuard,
            tracing_appender::non_blocking::WorkerGuard,
        )>,
    )> {
        tracing::debug!("Worker is setting up");
        let config = WorkerConfig::new(cli)?;
        let http_client = Client::new();
        let (_, credential) = get_user_credential(
            config.credential_path.as_ref(),
            &http_client,
            config.coordinator_addr.clone(),
            config.user.as_ref(),
            config.password.as_ref(),
        )
        .await?;
        let mut url = config.coordinator_addr.clone();
        url.set_path("worker");
        let req = RegisterWorkerReq {
            tags: config.tags.clone(),
            groups: config.groups.clone(),
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
            let guards = if !config.no_log_file {
                config
                    .log_file
                    .as_ref()
                    .map(|p| p.relative())
                    .or_else(|| {
                        dirs::cache_dir().map(|mut p| {
                            p.push("mitosis");
                            p.push("worker");
                            p
                        })
                    })
                    .map(|log_dir| {
                        let file_logger = tracing_appender::rolling::daily(
                            log_dir,
                            format!("{}.log", resp.worker_id),
                        );
                        let stdout = std::io::stdout.with_max_level(tracing::Level::INFO);
                        let (non_blocking, _guard) = tracing_appender::non_blocking(file_logger);
                        let _coordinator_subscriber = tracing_subscriber::registry()
                            .with(
                                tracing_subscriber::EnvFilter::try_from_default_env()
                                    .unwrap_or_else(|_| "netmito=info".into()),
                            )
                            .with(
                                tracing_subscriber::fmt::layer()
                                    .with_writer(stdout.and(non_blocking)),
                            )
                            .set_default();
                        (_coordinator_subscriber, _guard)
                    })
            } else {
                None
            };
            tracing::info!("Worker registered with ID: {}", resp.worker_id);
            Ok((
                MitoWorker {
                    config,
                    http_client,
                    credential: resp.token,
                    cancel_token: CancellationToken::new(),
                    cache_path,
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
        heartbeat_url.set_path("worker/heartbeat");
        let heartbeat_client = self.http_client.clone();
        let heartbeat_credential = self.credential.clone();
        let heartbeat_cancel_token = self.cancel_token.clone();
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
        let mut task_url = self.config.coordinator_addr.clone();
        let task_client = self.http_client.clone();
        let task_credential = self.credential.clone();
        let task_cancel_token = self.cancel_token.clone();
        let task_fetch_interval = self.config.fetch_task_interval;
        let task_cache_path = self.cache_path.clone();
        let task_hd = tokio::spawn(async move {
            loop {
                if task_cancel_token.is_cancelled() {
                    break;
                }
                task_url.set_path("worker/task");
                let resp = task_client
                    .get(task_url.as_str())
                    .bearer_auth(&task_credential)
                    .send()
                    .await;
                match resp {
                    Ok(resp) => {
                        if resp.status().is_success() {
                            match resp.json::<Option<WorkerTaskResp>>().await {
                                Ok(task) => match task {
                                    Some(task) => {
                                        match execute_task(
                                            task,
                                            (&task_client, &task_credential),
                                            &mut task_url,
                                            &task_cancel_token,
                                            task_fetch_interval,
                                            &task_cache_path,
                                        )
                                        .await
                                        {
                                            Ok(_) => {}
                                            Err(e) => {
                                                tracing::error!("Task execution failed: {}", e);
                                                task_cancel_token.cancel();
                                            }
                                        }
                                    }
                                    None => {
                                        tracing::debug!(
                                            "No task fetched. Next fetch after {:?}",
                                            task_fetch_interval
                                        );
                                        tokio::select! {
                                            biased;
                                            _ = task_cancel_token.cancelled() => break,
                                            _ = tokio::time::sleep(task_fetch_interval) => {},
                                        }
                                    }
                                },
                                Err(e) => {
                                    tracing::error!("Failed to parse task specification: {}", e);
                                    task_cancel_token.cancel();
                                    break;
                                }
                            }
                        } else if resp.status() == StatusCode::UNAUTHORIZED {
                            tracing::info!("Task fetch failed with coordinator force exit");
                            task_cancel_token.cancel();
                            break;
                        } else {
                            let resp: ErrorMsg = resp
                                .json()
                                .await
                                .unwrap_or_else(|e| ErrorMsg { msg: e.to_string() });
                            tracing::error!("Task fetch failed with error: {}", resp.msg);
                            task_cancel_token.cancel();
                            break;
                        }
                    }
                    Err(e) => {
                        if e.is_connect() && e.is_request() {
                            tracing::error!(
                                "Fetching task failed with connection error: {}. Retry after {:?}",
                                e,
                                task_fetch_interval
                            );
                            tokio::select! {
                                biased;
                                _ = task_cancel_token.cancelled() => break,
                                _ = tokio::time::sleep(task_fetch_interval) => {},
                            }
                            continue;
                        } else {
                            tracing::error!("Fetching task failed with error: {}", e);
                            task_cancel_token.cancel();
                            break;
                        }
                    }
                }
            }
        });
        tokio::select! {
            biased;
            _ = shutdown_signal() => {
                tracing::info!("Worker exits due to terminate signal received. Wait for resource cleanup");
                self.cancel_token.cancel();
                heartbeat_hd.await?;
                task_hd.await?;
            },
            _ = self.cancel_token.cancelled() => {
                tracing::info!("Worker exits due to internal execution error. Wait for resource cleanup");
                heartbeat_hd.await?;
                task_hd.await?;
            },
        }
        Ok(())
    }

    pub async fn cleanup(&self) {
        tracing::debug!("Worker is cleaning up.");
        let mut url = self.config.coordinator_addr.clone();
        url.set_path("worker");
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
    (task_client, task_credential): (&Client, &String),
    task_url: &mut Url,
    task_cancel_token: &CancellationToken,
    task_fetch_interval: std::time::Duration,
    cache_path: &PathBuf,
) -> crate::error::Result<()> {
    // TODO: may set the task state to pending while downloading resources
    let timeout_duration = task.timeout;
    let timeout_until = tokio::time::Instant::now() + timeout_duration;
    for resource in task.spec.resources {
        match resource.remote_file {
            RemoteResource::Artifact { uuid, content_type } => {
                let content_serde_val = serde_json::to_value(content_type)?;
                let content_serde_str = content_serde_val.as_str().unwrap_or("result");
                task_url.set_path(&format!("worker/artifacts/{}/{}", uuid, content_serde_str));
            }
            RemoteResource::Attachment { key } => {
                task_url.set_path(&format!("worker/attachments/{}", key));
            }
        };
        let resp = loop {
            match task_client
                .get(task_url.as_str())
                .bearer_auth(task_credential)
                .send()
                .await
            {
                Ok(resp) => break resp,
                Err(e) => {
                    if e.is_connect() && e.is_request() {
                        tracing::error!(
                            "Fetch resource info failed with connection error: {}. Retry after {:?}",
                            e,
                            task_fetch_interval
                        );
                        tokio::select! {
                            biased;
                            _ = task_cancel_token.cancelled() => return Ok(()),
                            _ = tokio::time::sleep(task_fetch_interval) => {},
                            _ = tokio::time::sleep_until(timeout_until) => {
                                tracing::debug!("Fetching resource timeout, commit this task as canceled");
                                let req = ReportTaskReq {
                                    id: task.id,
                                    op: ReportTaskOp::Cancel,
                                };
                                report_task(
                                    (task_client, task_credential),
                                    task_url,
                                    task_cancel_token,
                                    task_fetch_interval,
                                    req,
                                )
                                .await?;
                                let req = ReportTaskReq {
                                    id: task.id,
                                    op: ReportTaskOp::Commit(TaskResultSpec {
                                        exit_status: 0,
                                        msg: Some(TaskResultMessage::Timeout),
                                    }),
                                };
                                report_task(
                                    (task_client, task_credential),
                                    task_url,
                                    task_cancel_token,
                                    task_fetch_interval,
                                    req,
                                )
                                .await?;
                                return Ok(());
                            }
                        }
                        continue;
                    } else {
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
            tokio::select! {
                biased;
                res = download_file(task_client, &download_resp, resource.local_path) => {
                    if let Err(e) = res {
                        tracing::error!("Failed to download resource: {}", e);
                        let req = ReportTaskReq {
                            id: task.id,
                            op: ReportTaskOp::Cancel,
                        };
                        report_task(
                            (task_client, task_credential),
                            task_url,
                            task_cancel_token,
                            task_fetch_interval,
                            req,
                        )
                        .await?;
                        let req = ReportTaskReq {
                            id: task.id,
                            op: ReportTaskOp::Commit(TaskResultSpec {
                                exit_status: 0,
                                msg: Some(TaskResultMessage::ResourceForbidden),
                            }),
                        };
                        report_task(
                            (task_client, task_credential),
                            task_url,
                            task_cancel_token,
                            task_fetch_interval,
                            req,
                        )
                        .await?;
                        return Ok(());
                    }
                }
                _ = task_cancel_token.cancelled() => return Ok(()),
                _ = tokio::time::sleep_until(timeout_until) => {
                    tracing::debug!("Fetching resource timeout, commit this task as canceled");
                    let req = ReportTaskReq {
                        id: task.id,
                        op: ReportTaskOp::Cancel,
                    };
                    report_task(
                        (task_client, task_credential),
                        task_url,
                        task_cancel_token,
                        task_fetch_interval,
                        req,
                    )
                    .await?;
                    let req = ReportTaskReq {
                        id: task.id,
                        op: ReportTaskOp::Commit(TaskResultSpec {
                            exit_status: 0,
                            msg: Some(TaskResultMessage::Timeout),
                        }),
                    };
                    report_task(
                        (task_client, task_credential),
                        task_url,
                        task_cancel_token,
                        task_fetch_interval,
                        req,
                    )
                    .await?;
                    return Ok(());
                }
            }
        } else if resp.status() == StatusCode::NOT_FOUND {
            tracing::debug!("Resource not found, commit this task as canceled");
            let req = ReportTaskReq {
                id: task.id,
                op: ReportTaskOp::Cancel,
            };
            report_task(
                (task_client, task_credential),
                task_url,
                task_cancel_token,
                task_fetch_interval,
                req,
            )
            .await?;
            let req = ReportTaskReq {
                id: task.id,
                op: ReportTaskOp::Commit(TaskResultSpec {
                    exit_status: 0,
                    msg: Some(TaskResultMessage::ResourceNotFound),
                }),
            };
            report_task(
                (task_client, task_credential),
                task_url,
                task_cancel_token,
                task_fetch_interval,
                req,
            )
            .await?;
            return Ok(());
        } else if resp.status() == StatusCode::FORBIDDEN {
            tracing::debug!("Resource is forbidden to be fetched, commit this task as canceled");
            let req = ReportTaskReq {
                id: task.id,
                op: ReportTaskOp::Cancel,
            };
            report_task(
                (task_client, task_credential),
                task_url,
                task_cancel_token,
                task_fetch_interval,
                req,
            )
            .await?;
            let req = ReportTaskReq {
                id: task.id,
                op: ReportTaskOp::Commit(TaskResultSpec {
                    exit_status: 0,
                    msg: Some(TaskResultMessage::ResourceForbidden),
                }),
            };
            report_task(
                (task_client, task_credential),
                task_url,
                task_cancel_token,
                task_fetch_interval,
                req,
            )
            .await?;
            return Ok(());
        } else {
            let resp: ErrorMsg = resp
                .json()
                .await
                .unwrap_or_else(|e| ErrorMsg { msg: e.to_string() });
            return Err(Error::Custom(format!(
                "Fetch resource info failed with error: {}",
                resp.msg
            )));
        }
    }
    let mut command = Command::new(task.spec.command.as_ref());
    command
        .args(task.spec.args)
        .envs(task.spec.envs)
        .env("MITO_RESULT_DIR", cache_path.join("result"))
        .env("MITO_EXEC_DIR", cache_path.join("exec"))
        .env("MITO_RESOURCE_DIR", cache_path.join("resource"))
        .stdin(std::process::Stdio::null());
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
        _ = task_cancel_token.cancelled() => {
            tracing::info!("Task execution interrupted by shutdown signal");
            child.kill().await.inspect_err(|e| {
                tracing::error!("Failed to kill task: {}", e);
            })?;
            return Ok(());
        },
        output = process_output => output.map(TaskResult::Finish),
        _ = tokio::time::sleep_until(timeout_until) => {
            tracing::debug!("Task execution timeout");
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
                _ = task_cancel_token.cancelled() => {
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
    process_task_result(
        task.id,
        (task_client, task_credential),
        task_url,
        task_cancel_token,
        task_fetch_interval,
        cache_path,
        output,
    )
    .await?;
    Ok(())
}

async fn process_task_result(
    id: i64,
    (task_client, task_credential): (&Client, &String),
    task_url: &mut Url,
    task_cancel_token: &CancellationToken,
    task_fetch_interval: std::time::Duration,
    cache_path: &PathBuf,
    output: TaskResult,
) -> crate::error::Result<()> {
    let (is_finished, exit_status) = output.state();
    let req = ReportTaskReq {
        id,
        op: if is_finished {
            ReportTaskOp::Finish
        } else {
            ReportTaskOp::Cancel
        },
    };
    report_task(
        (task_client, task_credential),
        task_url,
        task_cancel_token,
        task_fetch_interval,
        req,
    )
    .await?;
    // Compress possible output and upload
    let (tx, mut rx) = mpsc::channel::<(ArtifactContentType, i64)>(3);
    // Spawn a task to archive the output
    let archive_cancel_token = task_cancel_token.clone();
    let archive_cache_path = cache_path.clone();
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
                }
                res = compress_task => {
                    match res {
                        Ok(_) => {
                            ar.finish().await?;
                            let mut encoder = ar.into_inner().await?;
                            encoder.shutdown().await?;
                            let file = encoder.into_inner();
                            let size = file.metadata().await?.len() as i64;
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
                }
                res = compress_task => {
                    match res {
                        Ok(_) => {
                            ar.finish().await?;
                            let mut encoder = ar.into_inner().await?;
                            encoder.shutdown().await?;
                            let file = encoder.into_inner();
                            let size = file.metadata().await?.len() as i64;
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
                }
                res = compress_task => {
                    match res {
                        Ok(_) => {
                            ar.finish().await?;
                            let mut encoder = ar.into_inner().await?;
                            encoder.shutdown().await?;
                            let file = encoder.into_inner();
                            let size = file.metadata().await?.len() as i64;
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
            task_url.set_path("worker/task");
            let resp = loop {
                let resp = task_client
                    .post(task_url.as_str())
                    .json(&req)
                    .bearer_auth(task_credential)
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
                            task_cancel_token.cancel();
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
                            task_cancel_token.cancel();
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
                                    task_fetch_interval
                                );
                            tokio::select! {
                                biased;
                                _ = task_cancel_token.cancelled() => return Ok(()),
                                _ = tokio::time::sleep(task_fetch_interval) => {},
                            }
                            continue;
                        } else {
                            task_cancel_token.cancel();
                            return Err(RequestError::from(e).into());
                        }
                    }
                }
            };
            if let Some(url) = resp.url {
                loop {
                    let file =
                        tokio::fs::File::open(cache_path.join(&content_type.to_string())).await?;
                    let upload_file = task_client
                        .put(url.as_str())
                        .header(CONTENT_LENGTH, content_length)
                        .body(file)
                        .send();
                    let resp = tokio::select! {
                        biased;
                        _ = task_cancel_token.cancelled() => {
                            tracing::info!("Upload failed with shutdown signal");
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
                                    "Upload failed with status code: {}",
                                    status
                                )));
                            }
                        }
                        Err(e) => {
                            if e.is_connect() && e.is_request() {
                                tracing::error!(
                                    "Upload failed with connection error: {}. Retry after {:?}",
                                    e,
                                    task_fetch_interval
                                );
                                tokio::select! {
                                    biased;
                                    _ = task_cancel_token.cancelled() => return Ok(()),
                                    _ = tokio::time::sleep(task_fetch_interval) => {},
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
    upload_artifact_fut.await?;
    archive_hd.await??;

    // Commit the task result
    let req = ReportTaskReq {
        id,
        op: ReportTaskOp::Commit(TaskResultSpec {
            exit_status: exit_status.into_raw(),
            msg: if is_finished {
                None
            } else {
                Some(TaskResultMessage::Timeout)
            },
        }),
    };
    report_task(
        (task_client, task_credential),
        task_url,
        task_cancel_token,
        task_fetch_interval,
        req,
    )
    .await?;
    // clean the directory after all the artifacts uploaded and the task committed
    tokio::fs::remove_dir_all(cache_path).await?;
    tokio::fs::create_dir_all(cache_path).await?;
    tokio::fs::create_dir_all(&cache_path.join("result")).await?;
    tokio::fs::create_dir_all(&cache_path.join("exec")).await?;
    Ok(())
}

async fn report_task(
    (task_client, task_credential): (&Client, &String),
    task_url: &mut Url,
    task_cancel_token: &CancellationToken,
    task_fetch_interval: std::time::Duration,
    req: ReportTaskReq,
) -> crate::error::Result<()> {
    task_url.set_path("worker/task");
    loop {
        let resp = task_client
            .post(task_url.as_str())
            .json(&req)
            .bearer_auth(task_credential)
            .send()
            .await;
        match resp {
            Ok(resp) => {
                if resp.status().is_success() {
                    break;
                } else if resp.status() == StatusCode::UNAUTHORIZED {
                    tracing::info!("Report task failed with coordinator force exit");
                    task_cancel_token.cancel();
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
                        task_fetch_interval
                    );
                    tokio::select! {
                        biased;
                        _ = task_cancel_token.cancelled() => break,
                        _ = tokio::time::sleep(task_fetch_interval) => {},
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
