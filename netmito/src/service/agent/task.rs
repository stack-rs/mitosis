//! Task claims and task reports of agents.
//!
//! ## Claims
//!
//! Agents claim from the database. One transaction selects the `Ready` tasks of the suite with the
//! highest priority `FOR UPDATE SKIP LOCKED` and changes them to `Running`. Because of `SKIP
//! LOCKED`, agents at the same time step over the rows that the others locked. Thus two agents
//! cannot claim the same task, and no agent waits.
//!
//! ## Reports
//!
//! Agents send the same [`ReportTaskOp`] as workers on `POST /tasks/report`, with the job handle.
//! The job must exist, belong to the agent, and not be terminal. A terminal job answers 404: the
//! job is closed and the task is no longer the agent's.
//!
//! | Op | Result |
//! |---|---|
//! | `Finish` | task → `Finished`, not archived yet |
//! | `Cancel` | task → `Cancelled`, the agent sends `Commit` after it |
//! | `Upload` | a presigned S3 url for an artifact. Records the artifact and charges the group |
//! | `Commit` | archives the task with its result, decreases `incomplete_tasks` of the suite, and makes the downstream task `Ready` |
//! | `Submit` | submits a downstream task in the group of the parent, in a suite of that group or in no suite |

use sea_orm::{prelude::*, QueryOrder, QuerySelect, Set, TransactionTrait};
use uuid::Uuid;

use crate::{
    config::InfraPool,
    entity::{
        active_tasks as ActiveTasks, archived_tasks as ArchivedTasks, groups as Group,
        state::{TaskState, TaskSuiteState},
        task_suites as TaskSuites, StoredTaskModel,
    },
    error::{ApiError, Error, Result},
    schema::{
        ExecSpec, FetchTasksResp, ReportTaskOp, TaskExecOptions, WorkerSchedulePlan, WorkerTaskResp,
    },
    service::{agent::job, agent::matching, s3::group_upload_artifact},
};

/// The maximum number of tasks in one claim, so that a large `worker_count` cannot lock too many
/// rows.
const MAX_FETCH_BATCH: u32 = 256;

/// The maximum number of tasks that one job can hold: the tasks that its workers run, and with
/// `prefetch`, one more task for each worker. Then a worker that becomes free gets its next task
/// without a round trip.
pub fn task_budget(schedule: &WorkerSchedulePlan) -> u32 {
    match schedule {
        WorkerSchedulePlan::FixedWorkers {
            worker_count,
            prefetch,
            ..
        } => ((*worker_count).max(1))
            .saturating_mul(1 + u32::from(*prefetch))
            .min(MAX_FETCH_BATCH),
    }
}

/// Claim the ready tasks of a suite, up to the free room of the job of this agent.
///
/// The agent does not give a number. The schedule of the suite gives the budget of one job, and
/// the tasks that the agent holds in the suite use part of it.
pub async fn agent_fetch_tasks(
    agent_id: i64,
    agent_uuid: Uuid,
    pool: &InfraPool,
    suite_uuid: Uuid,
) -> Result<FetchTasksResp> {
    let suite = TaskSuites::Entity::find()
        .filter(TaskSuites::Column::Uuid.eq(suite_uuid))
        .one(&pool.db)
        .await?
        .ok_or_else(|| Error::ApiError(ApiError::NotFound(format!("Suite {suite_uuid}"))))?;

    if !matching::is_agent_eligible(&pool.db, suite.id, agent_id).await? {
        return Err(Error::ApiError(ApiError::NotFound(format!(
            "Suite {suite_uuid}"
        ))));
    }
    if !suite.state.allows_task_execution() {
        return Err(Error::ApiError(ApiError::InvalidRequest(format!(
            "Suite {suite_uuid} is {} and gives no more tasks",
            suite.state
        ))));
    }

    let now = TimeDateTimeWithTimeZone::now_utc();
    let suite_id = suite.id;

    // The state comes from the same read as the checks above. If the sweep closed the suite just
    // now, a wrong `true` costs only one more fetch.
    let hold_job_open = matches!(suite.state, TaskSuiteState::Open);

    let budget = task_budget(
        &serde_json::from_value(suite.worker_schedule).inspect_err(|e| {
            tracing::error!("Stored worker schedule is unreadable: {e}");
        })?,
    );
    let group_name = Group::Entity::find_by_id(suite.group_id)
        .one(&pool.db)
        .await?
        .ok_or_else(|| Error::ApiError(ApiError::NotFound("Group of the suite".to_string())))?
        .group_name;

    let claimed = pool
        .db
        .transaction::<_, Vec<ActiveTasks::Model>, Error>(|txn| {
            Box::pin(async move {
                // The tasks that the agent holds and has not committed use part of the budget.
                let held = ActiveTasks::Entity::find()
                    .filter(ActiveTasks::Column::TaskSuiteId.eq(suite_id))
                    .filter(ActiveTasks::Column::RunnerUuid.eq(agent_uuid))
                    .count(txn)
                    .await?;
                let room = (budget as u64).saturating_sub(held);
                if room == 0 {
                    return Ok(Vec::new());
                }
                let candidates = ActiveTasks::Entity::find()
                    .filter(ActiveTasks::Column::TaskSuiteId.eq(suite_id))
                    .filter(ActiveTasks::Column::State.eq(TaskState::Ready))
                    .order_by_desc(ActiveTasks::Column::Priority)
                    .order_by_asc(ActiveTasks::Column::Id)
                    .limit(room)
                    .lock_with_behavior(
                        sea_orm::sea_query::LockType::Update,
                        sea_orm::sea_query::LockBehavior::SkipLocked,
                    )
                    .all(txn)
                    .await?;
                if candidates.is_empty() {
                    return Ok(Vec::new());
                }

                let ids: Vec<i64> = candidates.iter().map(|t| t.id).collect();
                ActiveTasks::Entity::update_many()
                    .col_expr(ActiveTasks::Column::State, Expr::value(TaskState::Running))
                    .col_expr(ActiveTasks::Column::RunnerUuid, Expr::value(agent_uuid))
                    .col_expr(ActiveTasks::Column::UpdatedAt, Expr::value(now))
                    .filter(ActiveTasks::Column::Id.is_in(ids))
                    .exec(txn)
                    .await?;

                Ok(candidates)
            })
        })
        .await?;

    let mut tasks = Vec::with_capacity(claimed.len());
    for task in claimed {
        let spec: ExecSpec = serde_json::from_value(task.spec).inspect_err(|e| {
            tracing::error!(task_uuid = %task.uuid, "Stored task spec is unreadable: {e}");
        })?;
        let exec_options: Option<TaskExecOptions> = task
            .exec_options
            .map(serde_json::from_value)
            .transpose()
            .inspect_err(|e| {
                tracing::error!(task_uuid = %task.uuid, "Stored exec_options are unreadable: {e}");
            })?;
        tasks.push(WorkerTaskResp {
            id: task.id,
            uuid: task.uuid,
            upstream_task_uuid: task.upstream_task_uuid,
            group_name: group_name.clone(),
            spec,
            exec_options,
        });
    }

    tracing::debug!(count = tasks.len(), "Agent claimed tasks from a suite");
    Ok(FetchTasksResp {
        tasks,
        hold_job_open,
    })
}

/// Apply a report of the agent to a task that it claimed.
pub async fn agent_report_task(
    agent_id: i64,
    agent_uuid: Uuid,
    job_handle: i64,
    task_id: i64,
    op: ReportTaskOp,
    pool: &InfraPool,
) -> Result<Option<String>> {
    let now = TimeDateTimeWithTimeZone::now_utc();

    let job_row = job::load_validate_job(&pool.db, job_handle, agent_id).await?;
    job::reject_if_terminal(&job_row)?;

    let task = ActiveTasks::Entity::find_by_id(task_id)
        .one(&pool.db)
        .await?
        .ok_or_else(|| Error::ApiError(ApiError::NotFound(format!("Task {task_id}"))))?;

    // Only the agent that claimed the task can report it. A reclaimed task has no runner, thus a
    // late report from the old agent gets `NotFound` and does not change the new run.
    if task.runner_uuid != Some(agent_uuid) {
        return Err(Error::ApiError(ApiError::NotFound(format!(
            "Task {task_id}"
        ))));
    }

    match op {
        ReportTaskOp::Finish => {
            tracing::debug!(agent_uuid = %agent_uuid, task_uuid = %task.uuid, "Agent finished a task");
            ActiveTasks::Entity::update_many()
                .col_expr(ActiveTasks::Column::State, Expr::value(TaskState::Finished))
                .col_expr(ActiveTasks::Column::UpdatedAt, Expr::value(now))
                .filter(ActiveTasks::Column::Id.eq(task.id))
                .exec(&pool.db)
                .await?;
        }

        ReportTaskOp::Cancel => {
            tracing::debug!(agent_uuid = %agent_uuid, task_uuid = %task.uuid, "Agent cancelled a task");
            // If the user cancelled the task first, the report only confirms it.
            if task.state != TaskState::Cancelled {
                ActiveTasks::Entity::update_many()
                    .col_expr(
                        ActiveTasks::Column::State,
                        Expr::value(TaskState::Cancelled),
                    )
                    .col_expr(ActiveTasks::Column::UpdatedAt, Expr::value(now))
                    .filter(ActiveTasks::Column::Id.eq(task.id))
                    .exec(&pool.db)
                    .await?;
            }
        }

        ReportTaskOp::Commit(res) => {
            tracing::debug!(agent_uuid = %agent_uuid, task_uuid = %task.uuid, "Agent committed a task");
            if task.state != TaskState::Finished && task.state != TaskState::Cancelled {
                return Err(Error::ApiError(ApiError::InvalidRequest(
                    "Task must be Finished or Cancelled before Commit".to_string(),
                )));
            }
            let result = serde_json::to_value(res)?;
            let archived = ArchivedTasks::ActiveModel {
                id: Set(task.id),
                creator_id: Set(task.creator_id),
                group_id: Set(task.group_id),
                task_id: Set(task.task_id),
                uuid: Set(task.uuid),
                tags: Set(task.tags.clone()),
                labels: Set(task.labels.clone()),
                created_at: Set(task.created_at),
                updated_at: Set(now),
                state: Set(task.state),
                runner_uuid: Set(task.runner_uuid),
                priority: Set(task.priority),
                spec: Set(task.spec.clone()),
                exec_options: Set(task.exec_options.clone()),
                result: Set(Some(result)),
                upstream_task_uuid: Set(task.upstream_task_uuid),
                downstream_task_uuid: Set(task.downstream_task_uuid),
                task_suite_id: Set(task.task_suite_id),
            };

            let inner_task_id = task.id;
            let suite_id = task.task_suite_id;
            pool.db
                .transaction::<_, (), Error>(|txn| {
                    Box::pin(async move {
                        // The suite row before the task rows, the lock order of all writers.
                        if let Some(suite_id) = suite_id {
                            crate::service::suite::decrement_incomplete_tasks(
                                txn, suite_id, 1, now,
                            )
                            .await?;
                        }
                        archived.insert(txn).await?;
                        ActiveTasks::Entity::delete_by_id(inner_task_id)
                            .exec(txn)
                            .await?;
                        Ok(())
                    })
                })
                .await?;

            // An earlier `Submit` made a `Pending` downstream task. The parent committed, thus the
            // downstream task becomes `Ready`.
            if let Some(downstream_uuid) = task.downstream_task_uuid {
                crate::service::task::worker_trigger_pending_task(pool, downstream_uuid).await?;
            }
        }

        ReportTaskOp::Upload {
            content_type,
            content_length,
        } => {
            let (_, url) = group_upload_artifact(
                pool,
                StoredTaskModel::Active(task),
                content_type,
                content_length,
            )
            .await?;
            return Ok(Some(url));
        }

        ReportTaskOp::Submit(req) => {
            if task.state != TaskState::Finished && task.state != TaskState::Cancelled {
                return Err(Error::ApiError(ApiError::InvalidRequest(
                    "Task must be Finished or Cancelled before it submits a task".to_string(),
                )));
            }
            // The downstream task stays in the group of the parent, in a suite of that group or in
            // no suite. The submit path checks this rule.
            let resp = crate::service::task::worker_submit_pending_task(
                pool,
                task.creator_id,
                task.uuid,
                task.group_id,
                *req,
            )
            .await?;

            ActiveTasks::Entity::update_many()
                .col_expr(
                    ActiveTasks::Column::DownstreamTaskUuid,
                    Expr::value(Some(resp.uuid)),
                )
                .col_expr(ActiveTasks::Column::UpdatedAt, Expr::value(now))
                .filter(ActiveTasks::Column::Id.eq(task.id))
                .exec(&pool.db)
                .await?;
        }
    }

    Ok(None)
}
