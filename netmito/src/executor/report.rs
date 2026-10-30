//! Sends task reports to the coordinator, including downstream-task submissions.

use reqwest::StatusCode;

use crate::error::{Error, ErrorMsg, RequestError};
use crate::schema::*;

use super::{ExecKind, Executor};

impl Executor {
    pub(super) async fn report_task(
        &mut self,
        kind: ExecKind,
        id: i64,
        op: ReportTaskOp,
    ) -> crate::error::Result<()> {
        let req = kind.report_req(id, op);
        self.http_url.set_path("tasks/report");
        loop {
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
                        break;
                    } else if resp.status() == StatusCode::UNAUTHORIZED {
                        tracing::info!("Report task failed with coordinator force exit");
                        self.coordinator_force_exit
                            .store(true, std::sync::atomic::Ordering::Release);
                        self.runner_cancel_token.cancel();
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
                            self.polling_interval
                        );
                        tokio::select! {
                            biased;
                            _ = self.task_cancel_token.cancelled() => break,
                            _ = tokio::time::sleep(self.polling_interval) => {},
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

    /// Submits the downstream task that the process wrote to `MITO_NEW_TASK`.
    /// Returns false only if the file is present and the submission fails.
    pub(super) async fn submit_new_task_if_present(
        &mut self,
        kind: ExecKind,
        task_id: i64,
    ) -> bool {
        // A hook has no `MITO_NEW_TASK` and cannot submit a task.
        if let ExecKind::Hook = kind {
            return true;
        }
        let new_task_path = self.task_cache_path.join("new_task.json");
        if !new_task_path.exists() {
            return true;
        }

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

        let submit_req: crate::schema::SubmitTaskReq = match serde_json::from_str(&new_task_content)
        {
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
        if let Err(e) = self
            .report_task(kind, task_id, ReportTaskOp::Submit(Box::new(submit_req)))
            .await
        {
            tracing::warn!("Failed to submit new task: {}", e);
            return false;
        }

        let _ = tokio::fs::remove_file(&new_task_path).await;
        true
    }
}
