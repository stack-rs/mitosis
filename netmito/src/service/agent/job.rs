//! The rows of `suite_agent_jobs`.
//!
//! A job is one run of a suite by one agent: accept, provision, execute, cleanup, then a terminal
//! state. These functions create the row, check the reports of an agent against it, and write the
//! terminal states that only the coordinator writes (`Lost`, `Killed`).
//!
//! A closed (terminal) job answers 404 to its agent: the job is no longer the agent's. A report
//! that comes in the wrong order answers 400.

use sea_orm::{prelude::*, QueryOrder, QuerySelect, Set};
use uuid::Uuid;

use crate::entity::{
    active_tasks as ActiveTasks, agents as Agent,
    state::{AgentState, SuiteJobState, TaskState},
    suite_agent_jobs as SuiteAgentJobs, task_suites as TaskSuites,
};
use crate::error::{ApiError, Error, Result};
use crate::service::agent::hook;

/// The job states that are not terminal.
pub const IN_FLIGHT: [SuiteJobState; 3] = [
    SuiteJobState::Provisioning,
    SuiteJobState::Executing,
    SuiteJobState::Cleanup,
];

/// Create the job row of an agent that accepted a suite, in `Provisioning`.
///
/// `job_id` is `max(job_id) + 1` in the suite. To calculate it safely, this function locks the
/// suite row, because accepts at the same time would otherwise get the same number. The lock comes
/// last, after all the checks, thus the caller holds no lock while it picks a suite.
///
/// Call it inside a transaction, or the lock is released immediately.
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

/// Find a job by its handle and make sure that it belongs to this agent. A missing job and the job
/// of a different agent both give `NotFound`.
pub async fn load_validate_job<C: ConnectionTrait>(
    db: &C,
    job: i64,
    agent_id: i64,
) -> Result<SuiteAgentJobs::Model> {
    let model = SuiteAgentJobs::Entity::find_by_id(job)
        .one(db)
        .await?
        .ok_or_else(|| Error::ApiError(ApiError::NotFound(format!("Job {job} not found"))))?;
    if model.agent_id != Some(agent_id) {
        return Err(Error::ApiError(ApiError::NotFound(format!(
            "Job {job} not found"
        ))));
    }
    Ok(model)
}

/// Change a job of this agent to `to`, but only from one of the states in `from`.
///
/// The state check and the write are one statement. Thus a terminal state that the coordinator
/// writes at the same time (`Lost`/`Killed`) either comes first, and this matches nothing, or comes
/// after.
///
/// `Ok(None)` means that nothing matched. The caller decides if that is an error, and
/// [`explain_failed_transition`] makes the error.
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

/// The error for a [`transition_job`] that matched nothing: missing or not this agent's, or
/// terminal (`NotFound`), or in the wrong state that is not terminal (`InvalidRequest`).
/// `expected` is the source state of an ordered transition. `None` means that all the states that
/// are not terminal were accepted.
///
/// The read has no lock and only makes the message, thus the successful path does not pay for it.
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
        // The state changed again between the update and this read.
        Ok(()) => Error::ApiError(ApiError::InvalidRequest(format!(
            "Job {} changed state concurrently",
            row.job_id
        ))),
    }
}

/// Reject a report for a terminal job with `NotFound`: the job is no longer the agent's.
pub fn reject_if_terminal(job: &SuiteAgentJobs::Model) -> Result<()> {
    if job.state.is_terminal() {
        return Err(Error::ApiError(ApiError::NotFound(format!(
            "Job {} is closed with state {}",
            job.job_id, job.state
        ))));
    }
    Ok(())
}

/// Make sure that a job is in the source state of an ordered transition:
/// - in `expected`: `Ok`.
/// - terminal: `NotFound`, the job is no longer the agent's.
/// - a different state that is not terminal: `InvalidRequest`. This is a protocol error, and it
///   must not look like a closed job, because the agent must not release a job that still runs.
pub fn expect_state(job: &SuiteAgentJobs::Model, expected: SuiteJobState) -> Result<()> {
    if job.state == expected {
        return Ok(());
    }
    reject_if_terminal(job)?;
    Err(Error::ApiError(ApiError::InvalidRequest(format!(
        "Job {} is in state {}, expected {} for this transition",
        job.job_id, job.state, expected
    ))))
}

/// Write `state` (`Lost` or `Killed`) to each job of the agent that is not terminal, and close
/// their open hooks. Returns the suite ids of these jobs.
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
    hook::close_still_open_hooks(db, terminated.iter().map(|j| j.id).collect(), now).await?;
    Ok(terminated.into_iter().map(|j| j.task_suite_id).collect())
}

/// Stop each job of a cancelled suite that is not terminal (`Killed`, no cleanup), close their
/// open hooks, and release each agent that is still assigned to the suite. Returns the uuids of
/// the released agents.
///
/// The agent cannot release itself: the coordinator rejects its `Complete` for a `Killed` job.
/// Call it in a transaction, so that the kill and the release occur together.
pub async fn kill_suite_jobs<C: ConnectionTrait>(
    db: &C,
    suite_id: i64,
    now: TimeDateTimeWithTimeZone,
) -> Result<Vec<Uuid>> {
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
    hook::close_still_open_hooks(db, killed.iter().map(|j| j.id).collect(), now).await?;

    // The filter is on the suite, not on the agents of the killed jobs. The suite is cancelled,
    // thus each assignment to it is old, also the assignment of a job that ended without the
    // release.
    let released = Agent::Entity::update_many()
        .col_expr(Agent::Column::State, Expr::value(AgentState::Idle))
        .col_expr(Agent::Column::AssignedTaskSuiteId, Expr::value(None::<i64>))
        .col_expr(Agent::Column::UpdatedAt, Expr::value(now))
        .filter(Agent::Column::AssignedTaskSuiteId.eq(suite_id))
        .exec_with_returning(db)
        .await?;

    Ok(released.into_iter().map(|agent| agent.uuid).collect())
}

/// The uuids of the agents that run a job of the suite that is not terminal.
pub async fn agents_running_suite<C: ConnectionTrait>(db: &C, suite_id: i64) -> Result<Vec<Uuid>> {
    Ok(SuiteAgentJobs::Entity::find()
        .select_only()
        .column(Agent::Column::Uuid)
        .join(
            sea_orm::JoinType::InnerJoin,
            SuiteAgentJobs::Relation::Agents.def(),
        )
        .filter(SuiteAgentJobs::Column::TaskSuiteId.eq(suite_id))
        .filter(SuiteAgentJobs::Column::State.is_in(IN_FLIGHT))
        .into_tuple::<Uuid>()
        .all(db)
        .await?)
}

/// Put the tasks of an agent that are not committed back to `Ready`, without a runner, so that a
/// different agent runs them again. This includes `Finished` and `Cancelled` tasks: the result of a
/// task comes only with its `Commit`, and nothing archives a task in these states.
///
/// `suite_id` limits the change: `None` takes all the tasks of the agent, `Some(id)` only its tasks
/// in that suite. Returns `(task id, suite id, priority)` for each task.
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
