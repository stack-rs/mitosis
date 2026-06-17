//! Suite agent run lifecycle helpers.
//!
//! A `suite_agent_runs` row is the single authority for one attempt of an
//! agent running a task suite (accept → provision → execute → cleanup →
//! terminal). These helpers create the row, validate run-scoped agent reports
//! against it, and write the coordinator-owned terminal states (`Lost`,
//! `Cancelled`). See docs/plans/2026-06-15-run-interaction-design.md (Part A).

use sea_orm::{prelude::*, QueryOrder, Set};
use uuid::Uuid;

use crate::config::InfraPool;
use crate::entity::{
    active_tasks as ActiveTasks,
    state::{SuiteRunState, TaskState},
    suite_agent_runs as SuiteAgentRuns,
};
use crate::error::{ApiError, Error, Result};
use crate::schema::{RunFailureKind, RunFailureReason};
use crate::service::suite_task_dispatcher::SuiteDispatcherOp;

/// Non-terminal run states (a run is "in flight").
const IN_FLIGHT: [SuiteRunState; 3] = [
    SuiteRunState::Provision,
    SuiteRunState::Executing,
    SuiteRunState::Cleanup,
];

/// Create a new run row for an agent that just accepted a suite (state
/// `Provision`).
///
/// Allocates `run_id = max(run_id)+1` scoped to the suite. The caller MUST hold
/// an exclusive lock on the suite row (fetched with `.lock_exclusive()` inside
/// the same transaction) so concurrent accepts of the same suite serialize and
/// the `(task_suite_id, run_id)` unique index never trips.
pub async fn create_run<C: ConnectionTrait>(
    db: &C,
    suite_id: i64,
    agent_id: i64,
    agent_uuid: Uuid,
    machine_id: i64,
    now: TimeDateTimeWithTimeZone,
) -> Result<SuiteAgentRuns::Model> {
    let next_run_id = SuiteAgentRuns::Entity::find()
        .filter(SuiteAgentRuns::Column::TaskSuiteId.eq(suite_id))
        .order_by_desc(SuiteAgentRuns::Column::RunId)
        .one(db)
        .await?
        .map(|m| m.run_id + 1)
        .unwrap_or(1);

    let run = SuiteAgentRuns::ActiveModel {
        task_suite_id: Set(suite_id),
        run_id: Set(next_run_id),
        agent_id: Set(Some(agent_id)),
        agent_uuid: Set(agent_uuid),
        machine_id: Set(machine_id),
        state: Set(SuiteRunState::Provision),
        tasks_completed: Set(0),
        tasks_failed: Set(0),
        failure_reason: Set(None),
        created_at: Set(now),
        started_at: Set(None),
        finished_at: Set(None),
        updated_at: Set(now),
        ..Default::default()
    };
    Ok(run.insert(db).await?)
}

/// Look up a run by its opaque internal `id` and verify it belongs to the
/// authenticated agent (A.5 validation steps 1–2).
///
/// A missing run, or one owned by a different agent, both surface as `NotFound`
/// (no information leak about other agents' runs). The caller applies the
/// per-endpoint state check (e.g. [`reject_if_terminal`]).
pub async fn load_validate_run<C: ConnectionTrait>(
    db: &C,
    run: i64,
    agent_uuid: Uuid,
) -> Result<SuiteAgentRuns::Model> {
    let model = SuiteAgentRuns::Entity::find_by_id(run)
        .one(db)
        .await?
        .ok_or_else(|| Error::ApiError(ApiError::NotFound(format!("Run {run} not found"))))?;
    if model.agent_uuid != agent_uuid {
        return Err(Error::ApiError(ApiError::NotFound(format!(
            "Run {run} not found"
        ))));
    }
    Ok(model)
}

/// Reject a state-mutating report (`/complete`, `report_task`) that targets an
/// already-terminal run with `409 Conflict` (A.5). The agent treats this as
/// "run already closed → I'm free". Used where any non-terminal state is a valid
/// source (e.g. `/complete`, which can terminate from any phase).
pub fn reject_if_terminal(run: &SuiteAgentRuns::Model) -> Result<()> {
    if run.state.is_terminal() {
        return Err(Error::ApiError(ApiError::Conflict(format!(
            "Run {} is already in terminal state {}",
            run.run_id, run.state
        ))));
    }
    Ok(())
}

/// Validate that a run is in the exact `expected` source state for an ordered
/// lifecycle transition (`/start` expects `Provision`, `/cleanup` expects
/// `Executing`). Mirrors the worker's `Commit` precondition (reject and mutate
/// nothing on a state mismatch):
/// - already in `expected` → Ok.
/// - terminal → `409 Conflict` (agent treats as "run closed → go Idle").
/// - any other non-terminal state → `400 InvalidRequest` (an out-of-order
///   protocol error; *not* the "closed" signal, so the agent does not free
///   itself off a still-live run).
pub fn expect_state(run: &SuiteAgentRuns::Model, expected: SuiteRunState) -> Result<()> {
    if run.state == expected {
        return Ok(());
    }
    if run.state.is_terminal() {
        return Err(Error::ApiError(ApiError::Conflict(format!(
            "Run {} is already in terminal state {}",
            run.run_id, run.state
        ))));
    }
    Err(Error::ApiError(ApiError::InvalidRequest(format!(
        "Run {} is in state {}, expected {} for this transition",
        run.run_id, run.state, expected
    ))))
}

/// Coordinator-written terminal: mark every in-flight run owned by `agent_uuid`
/// as `Lost` (heartbeat timeout / agent removed). Returns rows affected.
pub async fn mark_agent_runs_lost<C: ConnectionTrait>(
    db: &C,
    agent_uuid: Uuid,
    message: impl Into<String>,
    now: TimeDateTimeWithTimeZone,
) -> Result<u64> {
    let reason = serde_json::to_value(RunFailureReason {
        kind: RunFailureKind::AgentLost,
        message: message.into(),
    })?;
    let res = SuiteAgentRuns::Entity::update_many()
        .col_expr(
            SuiteAgentRuns::Column::State,
            Expr::value(SuiteRunState::Lost),
        )
        .col_expr(SuiteAgentRuns::Column::FailureReason, Expr::value(reason))
        .col_expr(SuiteAgentRuns::Column::FinishedAt, Expr::value(Some(now)))
        .col_expr(SuiteAgentRuns::Column::UpdatedAt, Expr::value(now))
        .filter(SuiteAgentRuns::Column::AgentUuid.eq(agent_uuid))
        .filter(SuiteAgentRuns::Column::State.is_in(IN_FLIGHT))
        .exec(db)
        .await?;
    Ok(res.rows_affected)
}

/// Coordinator-written terminal: mark every in-flight run of `suite_id` as
/// `Cancelled` (the suite was cancelled mid-run). Returns rows affected.
pub async fn mark_suite_runs_cancelled<C: ConnectionTrait>(
    db: &C,
    suite_id: i64,
    now: TimeDateTimeWithTimeZone,
) -> Result<u64> {
    let reason = serde_json::to_value(RunFailureReason {
        kind: RunFailureKind::SuiteCancelled,
        message: "Suite was cancelled while the run was in flight".to_string(),
    })?;
    let res = SuiteAgentRuns::Entity::update_many()
        .col_expr(
            SuiteAgentRuns::Column::State,
            Expr::value(SuiteRunState::Cancelled),
        )
        .col_expr(SuiteAgentRuns::Column::FailureReason, Expr::value(reason))
        .col_expr(SuiteAgentRuns::Column::FinishedAt, Expr::value(Some(now)))
        .col_expr(SuiteAgentRuns::Column::UpdatedAt, Expr::value(now))
        .filter(SuiteAgentRuns::Column::TaskSuiteId.eq(suite_id))
        .filter(SuiteAgentRuns::Column::State.is_in(IN_FLIGHT))
        .exec(db)
        .await?;
    Ok(res.rows_affected)
}

/// Reclaim a `Lost` agent's executed-but-uncommitted tasks: every `Running`
/// **and** `Finished` task still owned by `agent_uuid` is reset to `Ready`,
/// its `runner_uuid` cleared, and re-added to the suite dispatcher buffer so it
/// re-runs (A.5 stranded-task reclaim). Returns the number reclaimed.
pub async fn reclaim_agent_tasks(
    pool: &InfraPool,
    agent_uuid: Uuid,
    now: TimeDateTimeWithTimeZone,
) -> Result<usize> {
    let reclaimed = ActiveTasks::Entity::update_many()
        .col_expr(ActiveTasks::Column::State, Expr::value(TaskState::Ready))
        .col_expr(ActiveTasks::Column::RunnerUuid, Expr::value(None::<Uuid>))
        .col_expr(ActiveTasks::Column::UpdatedAt, Expr::value(now))
        .filter(ActiveTasks::Column::RunnerUuid.eq(agent_uuid))
        .filter(ActiveTasks::Column::State.is_in([TaskState::Running, TaskState::Finished]))
        .exec_with_returning(&pool.db)
        .await?;

    for task in &reclaimed {
        if let Some(suite_id) = task.task_suite_id {
            let _ = pool
                .suite_task_dispatcher_tx
                .send(SuiteDispatcherOp::AddTask {
                    suite_id,
                    task_id: task.id,
                    priority: task.priority,
                });
        }
    }
    Ok(reclaimed.len())
}
