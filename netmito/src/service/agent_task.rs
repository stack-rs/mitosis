//! Agent task service: batch-fetch tasks from a suite and report results.
//!
//! ## Fetch flow
//!
//! 1. Look up suite by UUID, verify agent is assigned.
//! 2. Ask the [`SuiteTaskDispatcher`] for up to `max_count` task IDs.
//! 3. For each ID: atomically `SET state=Running, runner_id=agent_uuid WHERE id=? AND state=Ready`.
//! 4. Collect succeeded IDs; fetch their full specs in a batch query.
//! 5. If the dispatcher indicated a refill is needed, spawn a background task.
//! 6. Return `FetchTasksResp` with the task specs.
//!
//! ## Report flow
//!
//! Agents use the same [`ReportTaskOp`] variants as workers:
//!
//! | Op      | Behaviour |
//! |---------|-----------|
//! | `Finish` | Marks task as `Finished` (not yet archived). |
//! | `Cancel` | Marks task as `Cancelled`. Agent follows up with `Commit`. |
//! | `Upload` | Returns a presigned S3 URL for artifact upload. |
//! | `Commit` | Archives the task and (for suite tasks) decrements `pending_tasks`. |
//! | `Submit` | **Not supported** — returns `InvalidRequest`. See Phase 7 for task chaining. |
//!
//! ## Suite completion
//!
//! On `Commit`, if the task belongs to a suite, `pending_tasks` is decremented.
//! If the count reaches 0 and the suite is in `Open` or `Closed` state, the suite
//! is atomically transitioned to `Complete` and the dispatcher buffer is dropped.

use sea_orm::{prelude::*, FromQueryResult, QueryOrder, QuerySelect, Set, TransactionTrait};
use uuid::Uuid;

use crate::{
    config::InfraPool,
    entity::{
        active_tasks as ActiveTasks, archived_tasks as ArchivedTasks,
        state::{TaskState, TaskSuiteState},
        task_suite_agent as TaskSuiteAgent, task_suites as TaskSuites, StoredTaskModel,
    },
    error::{ApiError, Error, Result},
    schema::{ExecSpec, FetchTasksResp, ReportTaskOp, TaskExecOptions, WorkerTaskResp},
    service::{
        s3::group_upload_artifact,
        suite_task_dispatcher::{FetchResult, SuiteDispatcherOp},
    },
};

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Lightweight query result used for refill queries.
#[derive(Debug, Clone, FromQueryResult)]
struct PartialTask {
    id: i64,
    priority: i32,
}

/// Spawn a background task that refills the suite's dispatcher buffer from the DB.
fn spawn_suite_refill(pool: InfraPool, suite_id: i64, max_capacity: u32) {
    tokio::spawn(async move {
        let tasks = ActiveTasks::Entity::find()
            .filter(ActiveTasks::Column::TaskSuiteId.eq(suite_id))
            .filter(ActiveTasks::Column::State.eq(TaskState::Ready))
            .order_by_desc(ActiveTasks::Column::Priority)
            .limit(max_capacity as u64)
            .into_model::<PartialTask>()
            .all(&pool.db)
            .await;

        match tasks {
            Ok(tasks) => {
                let pairs: Vec<(i64, i32)> = tasks.into_iter().map(|t| (t.id, t.priority)).collect();
                let _ = pool
                    .suite_task_dispatcher_tx
                    .send(SuiteDispatcherOp::AddRefillTasks {
                        suite_id,
                        tasks: pairs,
                    });
            }
            Err(e) => {
                tracing::error!(
                    suite_id = suite_id,
                    "Suite buffer refill query failed: {}",
                    e
                );
            }
        }

        // Always clear the is_refilling flag so the next fetch can trigger another refill.
        let _ = pool
            .suite_task_dispatcher_tx
            .send(SuiteDispatcherOp::RefillDone { suite_id });
    });
}

// ─────────────────────────────────────────────────────────────────────────────
// Public API
// ─────────────────────────────────────────────────────────────────────────────

/// Batch-fetch up to `max_count` tasks from a suite for an agent to execute.
pub async fn agent_fetch_tasks(
    agent_id: i64,
    agent_uuid: Uuid,
    pool: &InfraPool,
    suite_uuid: Uuid,
    max_count: u32,
) -> Result<FetchTasksResp> {
    let max_count = max_count.max(1);

    // 1. Resolve suite UUID → DB row (must be Open or Closed with pending tasks).
    let suite = TaskSuites::Entity::find()
        .filter(TaskSuites::Column::Uuid.eq(suite_uuid))
        .filter(
            TaskSuites::Column::State
                .eq(TaskSuiteState::Open)
                .or(TaskSuites::Column::State.eq(TaskSuiteState::Closed)),
        )
        .one(&pool.db)
        .await?
        .ok_or_else(|| {
            Error::ApiError(ApiError::NotFound(format!(
                "Suite {suite_uuid} not found or not available for task fetching"
            )))
        })?;

    let suite_id = suite.id;

    // 2. Verify this agent is assigned to the suite.
    let assigned = TaskSuiteAgent::Entity::find()
        .filter(TaskSuiteAgent::Column::AgentId.eq(agent_id))
        .filter(TaskSuiteAgent::Column::TaskSuiteId.eq(suite_id))
        .one(&pool.db)
        .await?;

    if assigned.is_none() {
        return Err(Error::ApiError(ApiError::NotFound(format!(
            "Suite {suite_uuid} not found or not available for task fetching"
        ))));
    }

    // 3. Ask the dispatcher for task IDs.
    let (tx, rx) = tokio::sync::oneshot::channel::<FetchResult>();
    if pool
        .suite_task_dispatcher_tx
        .send(SuiteDispatcherOp::FetchTasks {
            suite_id,
            max_count,
            tx,
        })
        .is_err()
    {
        return Err(Error::Custom("suite task dispatcher channel closed".to_string()));
    }

    let fetch_result = rx
        .await
        .map_err(|_| Error::Custom("suite task dispatcher dropped sender".to_string()))?;

    // 4. Trigger background refill if the buffer is running low.
    if fetch_result.needs_refill {
        spawn_suite_refill(pool.clone(), suite_id, fetch_result.max_capacity);
    }

    // 5. Atomically claim each task in the DB.
    let now = TimeDateTimeWithTimeZone::now_utc();
    let mut succeeded_ids: Vec<i64> = Vec::with_capacity(fetch_result.task_ids.len());

    for task_id in fetch_result.task_ids {
        let result = ActiveTasks::Entity::update_many()
            .col_expr(ActiveTasks::Column::State, Expr::value(TaskState::Running))
            .col_expr(ActiveTasks::Column::RunnerId, Expr::value(agent_uuid))
            .col_expr(ActiveTasks::Column::UpdatedAt, Expr::value(now))
            .filter(ActiveTasks::Column::Id.eq(task_id))
            .filter(ActiveTasks::Column::State.eq(TaskState::Ready))
            .filter(ActiveTasks::Column::RunnerId.is_null())
            .exec(&pool.db)
            .await?;

        if result.rows_affected > 0 {
            succeeded_ids.push(task_id);
        }
        // rows_affected == 0 means the task was cancelled or already claimed — skip it.
    }

    if succeeded_ids.is_empty() {
        return Ok(FetchTasksResp { tasks: vec![] });
    }

    // 6. Batch-fetch full task specs for the claimed tasks.
    let tasks = ActiveTasks::Entity::find()
        .filter(ActiveTasks::Column::Id.is_in(succeeded_ids))
        .all(&pool.db)
        .await?;

    let mut resp_tasks = Vec::with_capacity(tasks.len());
    for task in tasks {
        let spec: ExecSpec = serde_json::from_value(task.spec).map_err(|e| {
            Error::ApiError(ApiError::InternalServerError)
                .tap(|_| tracing::error!("task spec deserialization failed: {e}"))
        })?;
        let exec_options: Option<TaskExecOptions> = task
            .exec_options
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e| {
                Error::ApiError(ApiError::InternalServerError)
                    .tap(|_| tracing::error!("task exec_options deserialization failed: {e}"))
            })?;

        resp_tasks.push(WorkerTaskResp {
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
        count = resp_tasks.len(),
        "Agent fetched tasks from suite"
    );

    Ok(FetchTasksResp { tasks: resp_tasks })
}

/// Report the result of a task executed by an agent.
///
/// The task is looked up by its UUID; ownership is verified by checking that
/// `runner_id == agent_uuid`.
pub async fn agent_report_task(
    agent_uuid: Uuid,
    task_uuid: Uuid,
    op: ReportTaskOp,
    pool: &InfraPool,
) -> Result<Option<String>> {
    let now = TimeDateTimeWithTimeZone::now_utc();

    // Find the task in active_tasks.
    let task = ActiveTasks::Entity::find()
        .filter(ActiveTasks::Column::Uuid.eq(task_uuid))
        .one(&pool.db)
        .await?
        .ok_or_else(|| {
            Error::ApiError(ApiError::NotFound(format!("Task {task_uuid} not found")))
        })?;

    // Verify the agent owns this task.
    match task.runner_id {
        Some(rid) if rid == agent_uuid => {}
        _ => {
            return Err(Error::ApiError(ApiError::NotFound(format!(
                "Task {task_uuid} not found"
            ))))
        }
    }

    match op {
        // ──────────────────────────────────────────────────────────────────
        // Finish: mark as Finished, to be archived later via Commit.
        // ──────────────────────────────────────────────────────────────────
        ReportTaskOp::Finish => {
            tracing::debug!(agent_uuid = %agent_uuid, task_uuid = %task_uuid, "Agent finish task");
            ActiveTasks::Entity::update_many()
                .col_expr(ActiveTasks::Column::State, Expr::value(TaskState::Finished))
                .col_expr(ActiveTasks::Column::UpdatedAt, Expr::value(now))
                .filter(ActiveTasks::Column::Id.eq(task.id))
                .exec(&pool.db)
                .await?;
        }

        // ──────────────────────────────────────────────────────────────────
        // Cancel: mark as Cancelled (agent-initiated).  The agent follows up
        // with Commit to provide the result and have the task archived.
        // If the task is already Cancelled (user-cancelled), this is a no-op.
        // ──────────────────────────────────────────────────────────────────
        ReportTaskOp::Cancel => {
            tracing::debug!(agent_uuid = %agent_uuid, task_uuid = %task_uuid, "Agent cancel task");
            if task.state == TaskState::Cancelled {
                // Already cancelled (e.g., user cancelled while agent was working).
                // Acknowledge gracefully.
                return Ok(None);
            }
            ActiveTasks::Entity::update_many()
                .col_expr(ActiveTasks::Column::State, Expr::value(TaskState::Cancelled))
                .col_expr(ActiveTasks::Column::UpdatedAt, Expr::value(now))
                .filter(ActiveTasks::Column::Id.eq(task.id))
                .exec(&pool.db)
                .await?;
        }

        // ──────────────────────────────────────────────────────────────────
        // Commit: archive the task (requires Finished or Cancelled state)
        // and, for suite tasks, update suite counters + state.
        // ──────────────────────────────────────────────────────────────────
        ReportTaskOp::Commit(res) => {
            tracing::debug!(agent_uuid = %agent_uuid, task_uuid = %task_uuid, "Agent commit task");

            if task.state != TaskState::Finished && task.state != TaskState::Cancelled {
                return Err(Error::ApiError(ApiError::InvalidRequest(
                    "Task must be Finished or Cancelled before Commit".to_string(),
                )));
            }

            let result = serde_json::to_value(res)
                .map_err(|e| Error::ApiError(ApiError::InvalidRequest(e.to_string())))?;

            let archived_task = ArchivedTasks::ActiveModel {
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
                runner_id: Set(task.runner_id),
                priority: Set(task.priority),
                spec: Set(task.spec.clone()),
                exec_options: Set(task.exec_options.clone()),
                result: Set(Some(result)),
                upstream_task_uuid: Set(task.upstream_task_uuid),
                downstream_task_uuid: Set(task.downstream_task_uuid),
                task_suite_id: Set(task.task_suite_id),
            };

            // Archive + delete from active_tasks in a transaction.
            pool.db
                .transaction(|txn| {
                    let archived = archived_task.clone();
                    let task_id = task.id;
                    Box::pin(async move {
                        archived.insert(txn).await?;
                        ActiveTasks::Entity::delete_by_id(task_id).exec(txn).await?;
                        Ok::<_, crate::error::Error>(())
                    })
                })
                .await?;

            // Update suite counters for suite tasks.
            if let Some(suite_id) = task.task_suite_id {
                commit_suite_task(pool, suite_id, now).await?;
            }
        }

        // ──────────────────────────────────────────────────────────────────
        // Upload: return a presigned S3 URL.
        // ──────────────────────────────────────────────────────────────────
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

        // ──────────────────────────────────────────────────────────────────
        // Submit: not supported for agent-executed tasks.
        // Deferred to a future phase; record this decision here.
        // ──────────────────────────────────────────────────────────────────
        ReportTaskOp::Submit(_) => {
            return Err(Error::ApiError(ApiError::InvalidRequest(
                "Submit (sub-task spawning) is not supported for agent-executed tasks in the current implementation. \
                 This will be added in a future phase."
                    .to_string(),
            )));
        }
    }

    Ok(None)
}

// ─────────────────────────────────────────────────────────────────────────────
// Suite counter helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Decrement `pending_tasks` for a suite after a task is committed.
/// Transitions the suite to `Complete` if `pending_tasks` reaches 0.
async fn commit_suite_task(
    pool: &InfraPool,
    suite_id: i64,
    now: TimeDateTimeWithTimeZone,
) -> Result<()> {
    // Decrement pending_tasks (guard against going below 0).
    let updated = TaskSuites::Entity::update_many()
        .col_expr(
            TaskSuites::Column::PendingTasks,
            Expr::col(TaskSuites::Column::PendingTasks).sub(1),
        )
        .col_expr(TaskSuites::Column::UpdatedAt, Expr::value(now))
        .filter(TaskSuites::Column::Id.eq(suite_id))
        .filter(TaskSuites::Column::PendingTasks.gt(0))
        .exec_with_returning(&pool.db)
        .await?;

    // If pending_tasks reached 0 and the suite is Open or Closed → mark Complete.
    if let Some(suite) = updated.into_iter().next() {
        if suite.pending_tasks == 0
            && matches!(suite.state, TaskSuiteState::Open | TaskSuiteState::Closed)
        {
            // Conditional UPDATE is idempotent: concurrent commits that also see 0 are no-ops.
            TaskSuites::Entity::update_many()
                .col_expr(
                    TaskSuites::Column::State,
                    Expr::value(TaskSuiteState::Complete),
                )
                .col_expr(TaskSuites::Column::CompletedAt, Expr::value(Some(now)))
                .col_expr(TaskSuites::Column::UpdatedAt, Expr::value(now))
                .filter(TaskSuites::Column::Id.eq(suite_id))
                .filter(TaskSuites::Column::PendingTasks.eq(0))
                .filter(
                    TaskSuites::Column::State
                        .eq(TaskSuiteState::Open)
                        .or(TaskSuites::Column::State.eq(TaskSuiteState::Closed)),
                )
                .exec(&pool.db)
                .await?;

            tracing::info!(suite_id = suite_id, "Suite transitioned to Complete");

            // Release the in-memory buffer for this suite.
            let _ = pool
                .suite_task_dispatcher_tx
                .send(SuiteDispatcherOp::DropBuffer { suite_id });
        }
    }

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Extension trait (tiny helper to avoid nested closures in error paths)
// ─────────────────────────────────────────────────────────────────────────────

trait Tap {
    fn tap(self, f: impl FnOnce(&Self)) -> Self;
}

impl<T> Tap for T {
    fn tap(self, f: impl FnOnce(&Self)) -> Self {
        f(&self);
        self
    }
}
