//! Suite hook reporting (`POST /agents/suite/hook`).
//!
//! Append-only: a hook report is accepted even on a terminal run (run existence
//! and agent ownership are checked, but not the non-terminal guard — a hook may
//! legitimately finish after a coordinator terminal, e.g. cleanup during a
//! cancel). `Result` records the hook outcome in `suite_hook_executions`;
//! `Upload` presigns an S3 PUT for a large log and tracks it in
//! `suite_hook_artifacts` (quota-accounted). See
//! docs/plans/2026-06-15-run-interaction-design.md (A.3 / A.3.5).

use sea_orm::sea_query::OnConflict;
use sea_orm::{prelude::*, Set, TransactionTrait};
use uuid::Uuid;

use crate::config::InfraPool;
use crate::entity::{
    groups as Group,
    state::{GroupState, HookExecState, HookType},
    suite_hook_artifacts as SuiteHookArtifacts, suite_hook_executions as SuiteHookExecutions,
    task_suites as TaskSuites,
};
use crate::error::{ApiError, Error, Result};
use crate::schema::{ExecHooks, HookReportOp};
use crate::service::agent_run;
use crate::service::s3::get_presigned_upload_link;

/// Handle a hook report. Returns a presigned upload URL for the `Upload` op,
/// `None` for `Result`.
pub async fn agent_report_hook(
    agent_uuid: Uuid,
    run: i64,
    hook_type: HookType,
    op: HookReportOp,
    pool: &InfraPool,
) -> Result<Option<String>> {
    let now = TimeDateTimeWithTimeZone::now_utc();

    // Run must exist and belong to this agent. Intentionally NOT
    // `reject_if_terminal` — hook reports are append-only.
    let run_row = agent_run::load_validate_run(&pool.db, run, agent_uuid).await?;

    // The run's suite supplies the hook-spec snapshot and the quota group.
    let suite = TaskSuites::Entity::find_by_id(run_row.task_suite_id)
        .one(&pool.db)
        .await?
        .ok_or_else(|| {
            Error::ApiError(ApiError::NotFound("Suite for run not found".to_string()))
        })?;

    match op {
        // Record the hook's outcome as one suite_hook_executions row.
        HookReportOp::Result(result_spec) => {
            let state = if result_spec.exit_status == 0 {
                HookExecState::Completed
            } else {
                HookExecState::Failed
            };
            let result_json = serde_json::to_value(&result_spec)
                .map_err(|e| Error::ApiError(ApiError::InvalidRequest(e.to_string())))?;

            let row = SuiteHookExecutions::ActiveModel {
                suite_agent_run_id: Set(run),
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
            // Upsert on (run, hook_type): a re-reported Result overwrites the
            // outcome. Idempotent by design — a hook execution has no
            // exactly-once side effects (it touches no counters), so a duplicate
            // is harmless, matching how task Finish/Upload tolerate duplicates.
            SuiteHookExecutions::Entity::insert(row)
                .on_conflict(
                    OnConflict::columns([
                        SuiteHookExecutions::Column::SuiteAgentRunId,
                        SuiteHookExecutions::Column::HookType,
                    ])
                    .update_columns([
                        SuiteHookExecutions::Column::Spec,
                        SuiteHookExecutions::Column::State,
                        SuiteHookExecutions::Column::Result,
                        SuiteHookExecutions::Column::CompletedAt,
                        SuiteHookExecutions::Column::UpdatedAt,
                    ])
                    .to_owned(),
                )
                .exec(&pool.db)
                .await?;
            Ok(None)
        }

        // Presign a log upload against the hook's already-recorded execution row.
        HookReportOp::Upload {
            content_type,
            content_length,
        } => {
            // The hook's Result must precede its Upload (we key the S3 object on
            // the execution row id). `(run, hook_type)` is unique, so this is the
            // one and only execution for the hook.
            let hook_exec = SuiteHookExecutions::Entity::find()
                .filter(SuiteHookExecutions::Column::SuiteAgentRunId.eq(run))
                .filter(SuiteHookExecutions::Column::HookType.eq(hook_type))
                .one(&pool.db)
                .await?
                .ok_or_else(|| {
                    Error::ApiError(ApiError::InvalidRequest(
                        "Report the hook Result before uploading its log".to_string(),
                    ))
                })?;

            let group = Group::Entity::find_by_id(suite.group_id)
                .one(&pool.db)
                .await?
                .ok_or_else(|| {
                    Error::ApiError(ApiError::InvalidRequest(
                        "Group for the suite not found".to_string(),
                    ))
                })?;
            if group.state != GroupState::Active {
                return Err(Error::ApiError(ApiError::InvalidRequest(
                    "Group is not active".to_string(),
                )));
            }

            let content_length = content_length as i64;
            let s3_object_key = format!("hooks/{}/{content_type}", hook_exec.id);
            let s3_client = pool.s3.clone();
            let artifacts_bucket = pool.artifacts_bucket.clone();
            let hook_exec_id = hook_exec.id;
            let group_id = suite.group_id;

            // Quota check + artifact upsert + storage bump, atomically. The
            // presign is computed inside so we only commit reserved storage.
            let url = pool
                .db
                .transaction::<_, String, Error>(|txn| {
                    Box::pin(async move {
                        let group = Group::Entity::find_by_id(group_id)
                            .one(txn)
                            .await?
                            .ok_or_else(|| {
                                Error::ApiError(ApiError::InvalidRequest(
                                    "Group for the suite not found".to_string(),
                                ))
                            })?;
                        let existing = SuiteHookArtifacts::Entity::find()
                            .filter(
                                SuiteHookArtifacts::Column::SuiteHookExecutionId.eq(hook_exec_id),
                            )
                            .filter(SuiteHookArtifacts::Column::ContentType.eq(content_type))
                            .one(txn)
                            .await?;

                        let (recorded, new_used) = match &existing {
                            Some(a) => {
                                let recorded = content_length.max(a.size);
                                (recorded, group.storage_used + (recorded - a.size))
                            }
                            None => (content_length, group.storage_used + content_length),
                        };
                        if new_used > group.storage_quota {
                            return Err(Error::ApiError(ApiError::QuotaExceeded));
                        }

                        let url = get_presigned_upload_link(
                            &s3_client,
                            &artifacts_bucket,
                            s3_object_key,
                            content_length,
                        )
                        .await
                        .map_err(ApiError::from)?;

                        match existing {
                            Some(a) => {
                                let am = SuiteHookArtifacts::ActiveModel {
                                    id: Set(a.id),
                                    size: Set(recorded),
                                    updated_at: Set(now),
                                    ..Default::default()
                                };
                                am.update(txn).await?;
                            }
                            None => {
                                let am = SuiteHookArtifacts::ActiveModel {
                                    suite_hook_execution_id: Set(hook_exec_id),
                                    content_type: Set(content_type),
                                    size: Set(recorded),
                                    created_at: Set(now),
                                    updated_at: Set(now),
                                    ..Default::default()
                                };
                                am.insert(txn).await?;
                            }
                        }

                        let g = Group::ActiveModel {
                            id: Set(group_id),
                            storage_used: Set(new_used),
                            updated_at: Set(now),
                            ..Default::default()
                        };
                        g.update(txn).await?;

                        Ok(url)
                    })
                })
                .await?;

            Ok(Some(url))
        }
    }
}

/// Best-effort snapshot of a hook's spec from the suite definition, stored for
/// diagnostics on the execution row. Falls back to JSON null on absence or a
/// parse failure (the `spec` column is NOT NULL, but jsonb accepts `null`).
fn hook_spec_snapshot(suite: &TaskSuites::Model, hook_type: HookType) -> serde_json::Value {
    let Some(raw) = suite.exec_hooks.as_ref() else {
        return serde_json::Value::Null;
    };
    let hooks: ExecHooks = match serde_json::from_value(raw.clone()) {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!("failed to parse suite exec_hooks for hook snapshot: {e}");
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
