//! The hook rows of a job (`hook_tasks`), and the hook reports on `POST /tasks/report`.
//!
//! The coordinator creates one `Pending` row for each hook of the suite when the agent accepts the
//! suite. The agent reports a hook like a task: `Finish` or `Cancel`, then `Upload` for each
//! artifact, then `Commit` with the result. The artifacts of a hook use the hook uuid as their key
//! in the `artifacts` table.
//!
//! A row is closed when it has a result. When the job ends, the coordinator closes the rows that
//! the agent did not finish ([`close_open_hooks`]). A row without a result still accepts `Upload`
//! and `Commit` after the job ends, because a hook can finish after the coordinator stopped the
//! job, and its result is still useful.

use sea_orm::{prelude::*, Set, TransactionTrait};

use crate::config::InfraPool;
use crate::entity::{
    hook_tasks::{self as HookTasks, HookType},
    state::HookExecState,
    task_suites as TaskSuites,
};
use crate::error::{ApiError, Error, Result};
use crate::schema::{ExecHooks, JobHook, ReportTaskOp};
use crate::service::agent::job;
use crate::service::s3::reserve_artifact_upload;

/// Create one `Pending` row for each hook in `exec_hooks`.
pub async fn create_job_hooks<C: ConnectionTrait>(
    db: &C,
    job: i64,
    exec_hooks: Option<&ExecHooks>,
    now: TimeDateTimeWithTimeZone,
) -> Result<Vec<JobHook>> {
    let Some(exec_hooks) = exec_hooks else {
        return Ok(Vec::new());
    };
    let hooks = [
        (HookType::Provision, &exec_hooks.provision),
        (HookType::Background, &exec_hooks.background),
        (HookType::Cleanup, &exec_hooks.cleanup),
    ];
    let mut created = Vec::new();
    for (hook_type, spec) in hooks {
        let Some(spec) = spec else { continue };
        let row = HookTasks::ActiveModel {
            uuid: Set(Uuid::new_v4()),
            suite_agent_job_id: Set(job),
            hook_type: Set(hook_type),
            spec: Set(serde_json::to_value(spec)?),
            state: Set(HookExecState::Pending),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(db)
        .await?;
        created.push(JobHook {
            id: row.id,
            uuid: row.uuid,
            hook_type,
        });
    }
    Ok(created)
}

/// Close the hook rows of these jobs that have no result: `Pending` becomes `Cancelled` (the hook
/// did not run) and `Finished` becomes `Failed` (the result did not come).
pub async fn close_still_open_hooks<C: ConnectionTrait>(
    db: &C,
    jobs: Vec<i64>,
    now: TimeDateTimeWithTimeZone,
) -> Result<()> {
    if jobs.is_empty() {
        return Ok(());
    }
    for (from, to) in [
        (HookExecState::Pending, HookExecState::Cancelled),
        (HookExecState::Finished, HookExecState::Failed),
    ] {
        HookTasks::Entity::update_many()
            .col_expr(HookTasks::Column::State, Expr::value(to))
            .col_expr(HookTasks::Column::UpdatedAt, Expr::value(now))
            .filter(HookTasks::Column::SuiteAgentJobId.is_in(jobs.clone()))
            .filter(HookTasks::Column::State.eq(from))
            .filter(HookTasks::Column::Result.is_null())
            .exec(db)
            .await?;
    }
    Ok(())
}

/// Apply a report of the agent to one of its hook rows. Returns the upload url for `Upload`.
///
/// A missing row, a row of a different agent and a closed row give `NotFound`: the hook is no
/// longer the agent's. `Finish` and `Cancel` also give `NotFound` when the job is closed.
pub async fn agent_report_hook(
    agent_id: i64,
    hook_id: i64,
    op: ReportTaskOp,
    pool: &InfraPool,
) -> Result<Option<String>> {
    let now = TimeDateTimeWithTimeZone::now_utc();

    let hook = HookTasks::Entity::find_by_id(hook_id)
        .one(&pool.db)
        .await?
        .ok_or_else(|| Error::ApiError(ApiError::NotFound(format!("Hook {hook_id}"))))?;
    let job_row = job::load_validate_job(&pool.db, hook.suite_agent_job_id, agent_id).await?;
    if hook.result.is_some() {
        return Err(Error::ApiError(ApiError::NotFound(format!(
            "Hook {} is closed with state {}",
            hook.uuid, hook.state
        ))));
    }

    match op {
        ReportTaskOp::Finish | ReportTaskOp::Cancel => {
            job::reject_if_terminal(&job_row)?;
            let state = match op {
                ReportTaskOp::Finish => HookExecState::Finished,
                _ => HookExecState::Cancelled,
            };
            tracing::debug!(hook_uuid = %hook.uuid, %state, "Agent reported a hook");
            HookTasks::Entity::update_many()
                .col_expr(HookTasks::Column::State, Expr::value(state))
                .col_expr(HookTasks::Column::UpdatedAt, Expr::value(now))
                .filter(HookTasks::Column::Id.eq(hook.id))
                .filter(HookTasks::Column::Result.is_null())
                .exec(&pool.db)
                .await?;
        }

        ReportTaskOp::Submit(_) => {
            return Err(Error::ApiError(ApiError::InvalidRequest(
                "A hook cannot submit a task".to_string(),
            )));
        }

        ReportTaskOp::Upload {
            content_type,
            content_length,
        } => {
            let group_id = TaskSuites::Entity::find_by_id(job_row.task_suite_id)
                .one(&pool.db)
                .await?
                .ok_or_else(|| Error::ApiError(ApiError::NotFound("Suite of the job".to_string())))?
                .group_id;
            let hook_uuid = hook.uuid;
            let s3_client = pool.s3.clone();
            let artifacts_bucket = pool.artifacts_bucket.clone();
            let (_, url) = pool
                .db
                .transaction::<_, (bool, String), Error>(|txn| {
                    Box::pin(async move {
                        reserve_artifact_upload(
                            txn,
                            &s3_client,
                            &artifacts_bucket,
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
            return Ok(Some(url));
        }

        ReportTaskOp::Commit(res) => {
            let state = match hook.state {
                HookExecState::Pending => {
                    return Err(Error::ApiError(ApiError::InvalidRequest(
                        "Hook must be Finished or Cancelled before Commit".to_string(),
                    )));
                }
                HookExecState::Cancelled => HookExecState::Cancelled,
                _ if res.exit_status == 0 => HookExecState::Completed,
                _ => HookExecState::Failed,
            };
            tracing::debug!(hook_uuid = %hook.uuid, %state, "Agent committed a hook");
            let result = serde_json::to_value(res)?;
            HookTasks::Entity::update_many()
                .col_expr(HookTasks::Column::State, Expr::value(state))
                .col_expr(HookTasks::Column::Result, Expr::value(result))
                .col_expr(HookTasks::Column::CompletedAt, Expr::value(now))
                .col_expr(HookTasks::Column::UpdatedAt, Expr::value(now))
                .filter(HookTasks::Column::Id.eq(hook.id))
                .filter(HookTasks::Column::Result.is_null())
                .exec(&pool.db)
                .await?;
        }
    }
    Ok(None)
}
