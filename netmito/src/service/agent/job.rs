//! `suite_agent_jobs` lifecycle helpers.
//!
//! A job row is the single authority for one attempt of an agent running a task
//! suite (accept → provision → execute → cleanup → terminal). These helpers
//! create the row, validate job-scoped agent reports against it, and write the
//! coordinator-owned terminals (`Lost`, `Killed`).

use sea_orm::{prelude::*, QueryOrder, QuerySelect, Set};
use uuid::Uuid;

use crate::entity::{
    active_tasks as ActiveTasks,
    state::{SuiteJobState, TaskState},
    suite_agent_jobs as SuiteAgentJobs, task_suites as TaskSuites,
};
use crate::error::{ApiError, Error, Result};

/// The non-terminal job states (a job is "in flight").
pub const IN_FLIGHT: [SuiteJobState; 3] = [
    SuiteJobState::Provisioning,
    SuiteJobState::Executing,
    SuiteJobState::Cleanup,
];

/// Create the job row for an agent that just accepted a suite (`Provisioning`).
///
/// `job_id` is `max(job_id) + 1` scoped to the suite — irreducibly read →
/// compute → write, so the suite row is locked here to serialize concurrent
/// accepts against the `(task_suite_id, job_id)` unique index. Taken *last*,
/// after every runnable check, so nothing is held while the caller is still
/// choosing between suites.
///
/// Must run inside the caller's transaction, or the lock is released the moment
/// the `SELECT` commits on its own and protects nothing.
pub async fn create_job<C: ConnectionTrait>(
    db: &C,
    suite_id: i64,
    agent_id: i64,
    now: TimeDateTimeWithTimeZone,
) -> Result<SuiteAgentJobs::Model> {
    TaskSuites::Entity::find_by_id(suite_id)
        .lock_exclusive()
        .one(db)
        .await?
        .ok_or_else(|| {
            Error::ApiError(ApiError::NotFound(format!("Suite {suite_id} not found")))
        })?;

    let next_job_id = SuiteAgentJobs::Entity::find()
        .filter(SuiteAgentJobs::Column::TaskSuiteId.eq(suite_id))
        .order_by_desc(SuiteAgentJobs::Column::JobId)
        .one(db)
        .await?
        .map(|m| m.job_id + 1)
        .unwrap_or(1);

    let job = SuiteAgentJobs::ActiveModel {
        task_suite_id: Set(suite_id),
        job_id: Set(next_job_id),
        agent_id: Set(Some(agent_id)),
        state: Set(SuiteJobState::Provisioning),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    };
    Ok(job.insert(db).await?)
}

/// Look up a job by its opaque internal `id` and verify it belongs to the
/// authenticated agent.
///
/// A missing job and one owned by another agent both surface as `NotFound`
pub async fn load_validate_job<C: ConnectionTrait>(
    db: &C,
    job: i64,
    agent_id: i64,
) -> Result<SuiteAgentJobs::Model> {
    let model = SuiteAgentJobs::Entity::find_by_id(job).one(db).await?;
    validate_job_owner(model, job, agent_id)
}

/// Write `to` onto a job this agent owns, but only from one of `from`.
///
/// The state check and the write are the same statement, so a coordinator
/// terminal (`Lost`/`Killed`) racing it either lands first — and this matches
/// nothing — or lands after. Loading the row, testing it in Rust and writing it
/// back needs a held `FOR UPDATE` to mean anything at all.
///
/// `Ok(None)` covers every way it can fail to match; the caller decides whether
/// that is an error, and [`explain_failed_transition`] builds the message when
/// it is.
pub async fn transition_job<C: ConnectionTrait>(
    db: &C,
    job: i64,
    agent_id: i64,
    from: &[SuiteJobState],
    to: SuiteJobState,
    now: TimeDateTimeWithTimeZone,
) -> Result<Option<SuiteAgentJobs::Model>> {
    let updated = SuiteAgentJobs::Entity::update_many()
        .col_expr(SuiteAgentJobs::Column::State, Expr::value(to))
        .col_expr(SuiteAgentJobs::Column::UpdatedAt, Expr::value(now))
        .filter(SuiteAgentJobs::Column::Id.eq(job))
        .filter(SuiteAgentJobs::Column::AgentId.eq(agent_id))
        .filter(SuiteAgentJobs::Column::State.is_in(from.iter().copied()))
        .exec_with_returning(db)
        .await?;
    Ok(updated.into_iter().next())
}

/// Why a [`transition_job`] matched nothing: gone or another agent's
/// (`NotFound`), terminal (`Conflict`), or the wrong non-terminal state (`400`).
/// `expected` names the source state for an ordered transition; `None` means any
/// non-terminal state was acceptable.
///
/// The re-read is unlocked and only builds the message, so it costs nothing on
/// the path that succeeds.
pub async fn explain_failed_transition<C: ConnectionTrait>(
    db: &C,
    job: i64,
    agent_id: i64,
    expected: Option<SuiteJobState>,
) -> Error {
    let row = match load_validate_job(db, job, agent_id).await {
        Ok(row) => row,
        Err(e) => return e,
    };
    let rejection = match expected {
        Some(expected) => expect_state(&row, expected),
        None => reject_if_terminal(&row),
    };
    match rejection {
        Err(e) => e,
        // It moved again between the update and this read.
        Ok(()) => Error::ApiError(ApiError::InvalidRequest(format!(
            "Job {} changed state concurrently",
            row.job_id
        ))),
    }
}

fn validate_job_owner(
    model: Option<SuiteAgentJobs::Model>,
    job: i64,
    agent_id: i64,
) -> Result<SuiteAgentJobs::Model> {
    let model =
        model.ok_or_else(|| Error::ApiError(ApiError::NotFound(format!("Job {job} not found"))))?;
    if model.agent_id != Some(agent_id) {
        return Err(Error::ApiError(ApiError::NotFound(format!(
            "Job {job} not found"
        ))));
    }
    Ok(model)
}

/// Reject a state-mutating report against an already-terminal job with `400
/// Bad Request`. The agent reads that as "job already closed → I'm free"; teardown
/// already owns any stranded task's fate. Used where any non-terminal state is a
/// valid source (`/complete`, task reports).
pub fn reject_if_terminal(job: &SuiteAgentJobs::Model) -> Result<()> {
    if job.state.is_terminal() {
        return Err(Error::ApiError(ApiError::InvalidRequest(format!(
            "Job {} is already in terminal state {}",
            job.job_id, job.state
        ))));
    }
    Ok(())
}

/// Validate that a job is in the exact `expected` source state for an ordered
/// transition (`/start` expects `Provisioning`, `/cleanup` expects `Executing`):
/// - already in `expected` → `Ok`.
/// - terminal → `400 Bad Request` (the agent frees itself).
/// - any other non-terminal state → `400` — an out-of-order protocol error, and
///   deliberately *not* the "closed" signal, so the agent does not release
///   itself off a still-live job.
pub fn expect_state(job: &SuiteAgentJobs::Model, expected: SuiteJobState) -> Result<()> {
    if job.state == expected {
        return Ok(());
    }
    if job.state.is_terminal() {
        return Err(Error::ApiError(ApiError::InvalidRequest(format!(
            "Job {} is already in terminal state {}",
            job.job_id, job.state
        ))));
    }
    Err(Error::ApiError(ApiError::InvalidRequest(format!(
        "Job {} is in state {}, expected {} for this transition",
        job.job_id, job.state, expected
    ))))
}

/// Coordinator-written terminal: mark every in-flight job owned by `agent_id`
/// with `state` (`Lost` on heartbeat timeout, `Killed` on a force shutdown).
/// Returns the suite ids those jobs were running, so the caller can drop them from
/// the in-memory dispatch state and re-offer their work.
pub async fn terminate_agent_jobs<C: ConnectionTrait>(
    db: &C,
    agent_id: i64,
    state: SuiteJobState,
    now: TimeDateTimeWithTimeZone,
) -> Result<Vec<i64>> {
    let terminated = SuiteAgentJobs::Entity::update_many()
        .col_expr(SuiteAgentJobs::Column::State, Expr::value(state))
        .col_expr(SuiteAgentJobs::Column::UpdatedAt, Expr::value(now))
        .filter(SuiteAgentJobs::Column::AgentId.eq(agent_id))
        .filter(SuiteAgentJobs::Column::State.is_in(IN_FLIGHT))
        .exec_with_returning(db)
        .await?;
    Ok(terminated.into_iter().map(|j| j.task_suite_id).collect())
}

/// Coordinator-written terminal: force-stop every in-flight job of a suite
/// (`Killed`, no cleanup). Returns the agent ids whose jobs were stopped, so the
/// caller can notify them.
pub async fn kill_suite_jobs<C: ConnectionTrait>(
    db: &C,
    suite_id: i64,
    now: TimeDateTimeWithTimeZone,
) -> Result<Vec<i64>> {
    let killed = SuiteAgentJobs::Entity::update_many()
        .col_expr(
            SuiteAgentJobs::Column::State,
            Expr::value(SuiteJobState::Killed),
        )
        .col_expr(SuiteAgentJobs::Column::UpdatedAt, Expr::value(now))
        .filter(SuiteAgentJobs::Column::TaskSuiteId.eq(suite_id))
        .filter(SuiteAgentJobs::Column::State.is_in(IN_FLIGHT))
        .exec_with_returning(db)
        .await?;
    Ok(killed.into_iter().filter_map(|j| j.agent_id).collect())
}

/// The agent ids currently running an in-flight job of a suite.
pub async fn agents_running_suite<C: ConnectionTrait>(db: &C, suite_id: i64) -> Result<Vec<i64>> {
    let jobs = SuiteAgentJobs::Entity::find()
        .select_only()
        .column(SuiteAgentJobs::Column::AgentId)
        .filter(SuiteAgentJobs::Column::TaskSuiteId.eq(suite_id))
        .filter(SuiteAgentJobs::Column::State.is_in(IN_FLIGHT))
        .into_tuple::<Option<i64>>()
        .all(db)
        .await?;
    Ok(jobs.into_iter().flatten().collect())
}

/// Whether the agent owns any job that has not reached a terminal state.
///
/// Used to tell a live suite assignment from a stale one: `accept` writes the
/// assignment and the job row in the same transaction, so an agent that is
/// genuinely running something always has an in-flight job here.
pub async fn agent_has_in_flight_job<C: ConnectionTrait>(db: &C, agent_id: i64) -> Result<bool> {
    let count = SuiteAgentJobs::Entity::find()
        .filter(SuiteAgentJobs::Column::AgentId.eq(agent_id))
        .filter(SuiteAgentJobs::Column::State.is_in(IN_FLIGHT))
        .count(db)
        .await?;
    Ok(count > 0)
}

/// Reclaim an agent's executed-but-uncommitted tasks: every `Running`,
/// `Finished` **and** `Cancelled` task still owned by `agent_uuid` goes back to
/// `Ready` with its `runner_uuid` cleared, so another agent re-runs it.
/// `Finished` and `Cancelled` count because a task's result reaches the
/// coordinator only with its `Commit`, which never arrived — and the state alone
/// is not terminal, so a row left in it would never be archived by anything.
/// Returns what was reclaimed, as `(task id, suite id, priority)` — the suites
/// are how the caller knows whom to offer the work to now that it is claimable
/// again, and the priority is where it goes in their queue.
///
/// `suite_id` scopes the sweep: `None` takes back everything the agent holds,
/// `Some(id)` only what it holds in that suite. Runs on any connection,
/// including the caller's transaction.
pub async fn reclaim_agent_tasks<C: ConnectionTrait>(
    db: &C,
    agent_uuid: Uuid,
    suite_id: Option<i64>,
    now: TimeDateTimeWithTimeZone,
) -> Result<Vec<(i64, Option<i64>, i32)>> {
    let mut query = ActiveTasks::Entity::update_many()
        .col_expr(ActiveTasks::Column::State, Expr::value(TaskState::Ready))
        .col_expr(ActiveTasks::Column::RunnerUuid, Expr::value(None::<Uuid>))
        .col_expr(ActiveTasks::Column::UpdatedAt, Expr::value(now))
        .filter(ActiveTasks::Column::RunnerUuid.eq(agent_uuid))
        .filter(ActiveTasks::Column::State.is_in([
            TaskState::Running,
            TaskState::Finished,
            TaskState::Cancelled,
        ]));
    if let Some(suite_id) = suite_id {
        query = query.filter(ActiveTasks::Column::TaskSuiteId.eq(suite_id));
    }
    let reclaimed = query.exec_with_returning(db).await?;
    if !reclaimed.is_empty() {
        tracing::info!(
            agent_uuid = %agent_uuid,
            reclaimed = reclaimed.len(),
            "Reclaimed uncommitted tasks from an agent"
        );
    }
    Ok(reclaimed
        .into_iter()
        .map(|task| (task.id, task.task_suite_id, task.priority))
        .collect())
}
