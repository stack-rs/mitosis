//! Task claim and result reporting for agents.
//!
//! ## Claiming
//!
//! Agents claim straight from the database: one transaction selects the
//! highest-priority `Ready` tasks of the suite `FOR UPDATE SKIP LOCKED` and
//! flips them to `Running`. `SKIP LOCKED` is what makes it safe — concurrent
//! agents step over each other's locked rows instead of contending, so no two
//! agents can claim the same task and none of them block.
//!
//! ## Reporting
//!
//! Agents use the same [`ReportTaskOp`] variants as workers, with the suite job
//! handle attached. Every report is validated against the job first: it must
//! exist, belong to this agent, and be non-terminal — a terminal job answers
//! `409 Conflict`, which the agent reads as "the job is closed, stop".
//!
//! | Op | Behaviour |
//! |---|---|
//! | `Finish` | task → `Finished` (not yet archived) |
//! | `Cancel` | task → `Cancelled`; the agent follows up with `Commit` |
//! | `Upload` | request for presigned S3 URL for an artifact (records metadata, charges group quota) |
//! | `Commit` | archives the task with its result, decrements the suite's `incomplete_tasks` (completing the suite if that empties an already-`Closed` one), triggers the downstream task if one was spawned |
//! | `Submit` | spawns a downstream child in the parent's group, in any suite that group owns or none |

use sea_orm::{prelude::*, QueryOrder, QuerySelect, Set, TransactionTrait};
use uuid::Uuid;

use crate::{
    config::InfraPool,
    entity::{
        active_tasks as ActiveTasks, archived_tasks as ArchivedTasks,
        state::{TaskState, TaskSuiteState},
        task_suites as TaskSuites, StoredTaskModel,
    },
    error::{ApiError, Error, Result},
    schema::{ExecSpec, FetchTasksResp, ReportTaskOp, TaskExecOptions, WorkerTaskResp},
    service::{agent::job, agent::matching, s3::group_upload_artifact},
};

/// Upper bound on a single claim batch, so a malformed `max_count` cannot ask
/// the coordinator to lock an unbounded number of rows.
const MAX_FETCH_BATCH: u32 = 256;

/// Claim up to `max_count` ready tasks of a suite for this agent.
pub async fn agent_fetch_tasks(
    agent_id: i64,
    agent_uuid: Uuid,
    pool: &InfraPool,
    suite_uuid: Uuid,
    max_count: u32,
) -> Result<FetchTasksResp> {
    let max_count = max_count.clamp(1, MAX_FETCH_BATCH);

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
        return Err(Error::ApiError(ApiError::Conflict(format!(
            "Suite {suite_uuid} is {} and hands out no more tasks",
            suite.state
        ))));
    }

    let now = TimeDateTimeWithTimeZone::now_utc();
    let suite_id = suite.id;
    let claimed = pool
        .db
        .transaction::<_, Vec<ActiveTasks::Model>, Error>(|txn| {
            Box::pin(async move {
                // TODO: serve claims from a per-suite in-memory priority queue
                // refilled from the database in batches, falling back to this
                // query
                let candidates = ActiveTasks::Entity::find()
                    .filter(ActiveTasks::Column::TaskSuiteId.eq(suite_id))
                    .filter(ActiveTasks::Column::State.eq(TaskState::Ready))
                    .order_by_desc(ActiveTasks::Column::Priority)
                    .order_by_asc(ActiveTasks::Column::Id)
                    .limit(max_count as u64)
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
            spec,
            exec_options,
        });
    }

    tracing::debug!(
        agent_uuid = %agent_uuid,
        suite_uuid = %suite_uuid,
        count = tasks.len(),
        "Agent claimed tasks from a suite"
    );
    // Read from the same snapshot as the eligibility check above, which is at
    // most a claim-transaction old. Erring on the side of `true` for a suite the
    // sweep has just settled only costs one more poll.
    let hold_job_open = matches!(suite.state, TaskSuiteState::Open);
    Ok(FetchTasksResp {
        tasks,
        hold_job_open,
    })
}

/// Report the result of a task this agent claimed.
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

    // Ownership: only the agent the task was handed to may report on it. A task
    // reclaimed after a heartbeat lapse has had its runner cleared, so a late
    // report from the old owner lands here as "not found" rather than
    // overwriting the re-run.
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
            // Already cancelled (the user got there first) is an acknowledgement,
            // not an error.
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
                        archived.insert(txn).await?;
                        ActiveTasks::Entity::delete_by_id(inner_task_id)
                            .exec(txn)
                            .await?;
                        if let Some(suite_id) = suite_id {
                            crate::service::suite::decrement_incomplete_tasks(
                                txn, suite_id, 1, now,
                            )
                            .await?;
                        }
                        Ok(())
                    })
                })
                .await?;

            // Task chaining: a child registered by an earlier Submit goes
            // Pending → Ready now that its parent has committed.
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
                    "Task must be Finished or Cancelled before spawning a new task".to_string(),
                )));
            }
            // The child stays in the parent's group, but is free to name any
            // suite that group owns — the parent's own suite, a sibling one, or
            // none at all for a task the workers pick up. The submit path
            // enforces the group rule.
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
