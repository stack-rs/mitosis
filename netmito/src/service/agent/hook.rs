//! `POST /agents/job/hook` — provision / cleanup / background hook reports.
//!
//! Append-only, and deliberately **not** guarded against a terminal job: the job
//! existing and belonging to this agent is checked, but a cleanup hook may
//! legitimately finish after the coordinator already terminated the job (a
//! cancel, for instance), and losing that record helps nobody.
//!
//! `Result` writes the `hook_tasks` row; `Upload` presigns an S3 PUT for a log
//! or artifact of a hook whose result is already recorded. Hook artifacts live
//! in the shared `artifacts` table keyed by `hook_tasks.uuid` — there is no
//! separate hook-artifact table.

use sea_orm::sea_query::OnConflict;
use sea_orm::{prelude::*, Set, TransactionTrait};

use crate::config::InfraPool;
use crate::entity::{
    hook_tasks::{self as HookTasks, HookType},
    state::HookExecState,
    task_suites as TaskSuites,
};
use crate::error::{ApiError, Error, Result};
use crate::schema::{ExecHooks, HookReportOp, HookReportResp};
use crate::service::agent::job;
use crate::service::s3::reserve_artifact_upload;

pub async fn agent_report_hook(
    agent_id: i64,
    job_handle: i64,
    hook_type: HookType,
    op: HookReportOp,
    pool: &InfraPool,
) -> Result<HookReportResp> {
    let now = TimeDateTimeWithTimeZone::now_utc();

    let job_row = job::load_validate_job(&pool.db, job_handle, agent_id).await?;
    let suite = TaskSuites::Entity::find_by_id(job_row.task_suite_id)
        .one(&pool.db)
        .await?
        .ok_or_else(|| Error::ApiError(ApiError::NotFound("Suite of the job".to_string())))?;

    match op {
        HookReportOp::Result(result_spec) => {
            let state = if result_spec.exit_status == 0 {
                HookExecState::Completed
            } else {
                HookExecState::Failed
            };
            let result_json = serde_json::to_value(&result_spec)?;

            let row = HookTasks::ActiveModel {
                uuid: Set(Uuid::new_v4()),
                suite_agent_job_id: Set(job_handle),
                hook_type: Set(hook_type),
                spec: Set(hook_spec_snapshot(&suite, hook_type)),
                state: Set(state),
                result: Set(Some(result_json)),
                started_at: Set(None),
                completed_at: Set(Some(now)),
                created_at: Set(now),
                updated_at: Set(now),
                ..Default::default()
            };
            // Upsert on (job, hook_type): re-reporting a hook overwrites its
            // outcome.
            // `uuid` is deliberately absent from the update list: it is the key
            // this hook's artifacts are already filed under.
            let row = HookTasks::Entity::insert(row)
                .on_conflict(
                    OnConflict::columns([
                        HookTasks::Column::SuiteAgentJobId,
                        HookTasks::Column::HookType,
                    ])
                    .update_columns([
                        HookTasks::Column::Spec,
                        HookTasks::Column::State,
                        HookTasks::Column::Result,
                        HookTasks::Column::CompletedAt,
                        HookTasks::Column::UpdatedAt,
                    ])
                    .to_owned(),
                )
                .exec_with_returning(&pool.db)
                .await?;

            Ok(HookReportResp {
                hook_uuid: row.uuid,
                url: None,
            })
        }

        HookReportOp::Upload {
            content_type,
            content_length,
        } => {
            // The result must land first: it is what mints the hook uuid the
            // artifact is keyed by.
            let hook = HookTasks::Entity::find()
                .filter(HookTasks::Column::SuiteAgentJobId.eq(job_handle))
                .filter(HookTasks::Column::HookType.eq(hook_type))
                .one(&pool.db)
                .await?
                .ok_or_else(|| {
                    Error::ApiError(ApiError::InvalidRequest(
                        "Should report the hook Result before uploading its artifacts".to_string(),
                    ))
                })?;

            let hook_uuid = hook.uuid;
            let group_id = suite.group_id;
            let content_length = content_length as i64;
            let pool_cloned = pool.clone();

            let (_, url) = pool
                .db
                .transaction::<_, (bool, String), Error>(|txn| {
                    Box::pin(async move {
                        reserve_artifact_upload(
                            txn,
                            &pool_cloned,
                            group_id,
                            hook_uuid,
                            content_type,
                            content_length,
                            now,
                        )
                        .await
                    })
                })
                .await?;

            Ok(HookReportResp {
                hook_uuid,
                url: Some(url),
            })
        }
    }
}

/// Snapshot of the hook's spec from the suite definition, stored on the row for
/// diagnostics. Falls back to JSON `null` when absent or unparseable — the
/// column is NOT NULL, but jsonb accepts `null`, and a missing snapshot must
/// never cost us the outcome record.
fn hook_spec_snapshot(suite: &TaskSuites::Model, hook_type: HookType) -> serde_json::Value {
    let Some(raw) = suite.exec_hooks.as_ref() else {
        return serde_json::Value::Null;
    };
    let hooks: ExecHooks = match serde_json::from_value(raw.clone()) {
        Ok(hooks) => hooks,
        Err(e) => {
            tracing::warn!("Failed to parse the suite's exec_hooks for a hook snapshot: {e}");
            return serde_json::Value::Null;
        }
    };
    let chosen = match hook_type {
        HookType::Provision => hooks.provision,
        HookType::Cleanup => hooks.cleanup,
        HookType::Background => hooks.background,
    };
    chosen
        .map(|spec| serde_json::to_value(spec).unwrap_or(serde_json::Value::Null))
        .unwrap_or(serde_json::Value::Null)
}
