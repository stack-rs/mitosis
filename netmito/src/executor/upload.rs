//! Archives and uploads the process output after the process stops, and then
//! commits the result.

use std::os::unix::process::ExitStatusExt;

use async_compression::tokio::write::GzipEncoder;
use reqwest::header::CONTENT_LENGTH;
use reqwest::StatusCode;
use tokio::io::AsyncWriteExt;
use tokio_tar::{Builder, Header};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::entity::content::ArtifactContentType;
use crate::entity::state::TaskExecState;
use crate::error::{Error, ErrorMsg, RequestError};
use crate::schema::*;

use super::{Executor, ProcessOutput, TaskResult};

impl Executor {
    pub(super) async fn process_task_result(
        &mut self,
        id: i64,
        uuid: Uuid,
        output: TaskResult,
    ) -> crate::error::Result<()> {
        let (is_finished, exit_status) = output.state();
        let req = ReportTaskReq {
            id,
            op: if is_finished {
                self.announce_task_state_ex(&uuid, TaskExecState::UploadFinishedResult as i32, 660)
                    .await;
                ReportTaskOp::Finish
            } else {
                self.announce_task_state_ex(
                    &uuid,
                    TaskExecState::UploadCancelledResult as i32,
                    660,
                )
                .await;
                ReportTaskOp::Cancel
            },
        };
        self.report_task(req).await?;
        // A spawned task archives each output, and this task uploads each archive when it is ready.
        // The channel sends the content type and size of each archive.
        let (tx, rx) = crossfire::spsc::unbounded_async::<(ArtifactContentType, u64)>();
        let timeout_cancel_token = CancellationToken::new();
        let archive_timeout_cancel_token = timeout_cancel_token.clone();
        let archive_cancel_token = self.task_cancel_token.clone();
        let archive_cache_path = self.task_cache_path.clone();
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
                                if let Err(e) = tx.send((ArtifactContentType::Result, size)) {
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
                                if let Err(e) = tx.send((ArtifactContentType::ExecLog, size)) {
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
                                if let Err(e) = tx.send((ArtifactContentType::StdLog, size)) {
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
            // Move `rx` into this future. When the upload stops, `rx` drops and the
            // next `send` fails. Thus the archiver stops before it compresses output
            // that no upload uses.
            let rx = rx;
            while let Ok((content_type, content_length)) = rx.recv().await {
                let req = ReportTaskReq {
                    id,
                    op: ReportTaskOp::Upload {
                        content_type,
                        content_length,
                    },
                };
                self.http_url.set_path("tasks/report");
                let resp = loop {
                    let resp = self
                        .http_client
                        .post(self.http_url.as_str())
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
                                break resp;
                            } else if resp.status() == StatusCode::UNAUTHORIZED {
                                tracing::info!(
                                    "Request upload url failed with coordinator force exit"
                                );
                                self.coordinator_force_exit
                                    .store(true, std::sync::atomic::Ordering::Release);
                                self.task_cancel_token.cancel();
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
                                self.task_cancel_token.cancel();
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
                                        self.polling_interval
                                    );
                                tokio::select! {
                                    biased;
                                    _ = self.task_cancel_token.cancelled() => return Ok(()),
                                    _ = tokio::time::sleep(self.polling_interval) => {},
                                }
                                continue;
                            } else {
                                self.task_cancel_token.cancel();
                                return Err(RequestError::from(e).into());
                            }
                        }
                    }
                };
                if let Some(url) = resp.url {
                    loop {
                        let file = tokio::fs::File::open(
                            self.task_cache_path.join(content_type.to_string()),
                        )
                        .await?;
                        let upload_file = self
                            .http_client
                            .put(url.as_str())
                            .header(CONTENT_LENGTH, content_length)
                            .body(file)
                            .send();
                        let resp = tokio::select! {
                            biased;
                            _ = self.task_cancel_token.cancelled() => {
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
                                        self.polling_interval
                                    );
                                    tokio::select! {
                                        biased;
                                        _ = self.task_cancel_token.cancelled() => return Ok(()),
                                        _ = timeout_cancel_token.cancelled() => {
                                            tracing::warn!("Upload failed with timeout");
                                            return Ok(());
                                        }
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
            }
            crate::error::Result::Ok(())
        };
        let timeout_until = tokio::time::Instant::now() + self.upload_budget;
        tokio::select! {
            biased;
            _ = tokio::time::sleep_until(timeout_until) => {
                tracing::warn!("Upload result timeout");
                timeout_cancel_token.cancel();
                let req = ReportTaskReq {
                    id,
                    op: ReportTaskOp::Commit(TaskResultSpec {
                        exit_status: exit_status.into_raw(),
                        msg: Some(TaskResultMessage::UploadResultTimeout),
                    }),
                };
                self.report_task(req).await?;
                self
                    .announce_task_state_ex(&uuid, TaskExecState::UploadResultTimeout as i32, 60)
                    .await;
                self
                    .announce_task_state_ex(&uuid, TaskExecState::TaskCommitted as i32, 60)
                    .await;
                archive_hd.await??;
            }
            res = upload_artifact_fut => {
                res?;
                archive_hd.await??;
                if self.task_cancel_token.is_cancelled() {
                    tracing::info!("Task execution interrupted by shutdown signal");
                    self
                        .announce_task_state_ex(&uuid, TaskExecState::WorkerExited as i32, 60)
                        .await;
                    return Ok(());
                }
                self
                    .announce_task_state_ex(&uuid, TaskExecState::UploadResultFinished as i32, 60)
                    .await;

                let new_task_scceed = self.submit_new_task_if_present(id).await;
                let msg = if is_finished {
                    if new_task_scceed {
                        None
                    } else {
                        Some(TaskResultMessage::SubmitNewTaskFailed)
                    }
                } else {
                    Some(TaskResultMessage::ExecTimeout)
                };
                let req = ReportTaskReq {
                    id,
                    op: ReportTaskOp::Commit(TaskResultSpec {
                        exit_status: exit_status.into_raw(),
                        msg
                    }),
                };
                self.report_task(req).await?;
                self
                    .announce_task_state_ex(&uuid, TaskExecState::TaskCommitted as i32, 60)
                    .await;

            }
        }

        // Reset the cache directory for the next task.
        tokio::fs::remove_dir_all(&self.task_cache_path).await?;
        tokio::fs::create_dir_all(&self.task_cache_path).await?;
        tokio::fs::create_dir_all(&self.task_cache_path.join("result")).await?;
        tokio::fs::create_dir_all(&self.task_cache_path.join("exec")).await?;
        Ok(())
    }
}
