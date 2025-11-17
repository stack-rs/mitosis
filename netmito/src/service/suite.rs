//! Task Suite service for managing suite lifecycles and counters

use sea_orm::prelude::*;

use crate::entity::{
    state::TaskSuiteState,
    task_suites::{self as TaskSuites},
};
use crate::error::Result;

/// Increments total_tasks and pending_tasks when a task is submitted to a suite.
/// Also updates last_task_submitted_at timestamp.
///
/// This function should be called within the same transaction as task submission.
pub async fn increment_suite_task_counters<C>(
    db: &C,
    task_suite_id: i64,
    now: TimeDateTimeWithTimeZone,
) -> Result<()>
where
    C: ConnectionTrait,
{
    TaskSuites::Entity::update_many()
        .col_expr(
            TaskSuites::Column::TotalTasks,
            Expr::col(TaskSuites::Column::TotalTasks).add(1),
        )
        .col_expr(
            TaskSuites::Column::PendingTasks,
            Expr::col(TaskSuites::Column::PendingTasks).add(1),
        )
        .col_expr(TaskSuites::Column::LastTaskSubmittedAt, Expr::value(now))
        .col_expr(TaskSuites::Column::UpdatedAt, Expr::value(now))
        .filter(TaskSuites::Column::Id.eq(task_suite_id))
        .exec(db)
        .await?;

    Ok(())
}

/// Decrements pending_tasks when a task completes or is cancelled.
/// If pending_tasks reaches 0, transitions the suite to Complete state.
///
/// This function should be called within the same transaction as task archival.
pub async fn decrement_suite_task_counter<C>(
    db: &C,
    task_suite_id: i64,
    now: TimeDateTimeWithTimeZone,
) -> Result<()>
where
    C: ConnectionTrait,
{
    // First, decrement the pending_tasks counter
    // We only decrement if pending_tasks > 0 to prevent negative values
    let updated = TaskSuites::Entity::update_many()
        .col_expr(
            TaskSuites::Column::PendingTasks,
            Expr::col(TaskSuites::Column::PendingTasks).sub(1),
        )
        .col_expr(TaskSuites::Column::UpdatedAt, Expr::value(now))
        .filter(TaskSuites::Column::Id.eq(task_suite_id))
        .filter(TaskSuites::Column::PendingTasks.gt(0))
        .exec_with_returning(db)
        .await?;

    // Check if this was the last pending task and transition to Complete if needed
    if let Some(suite) = updated.into_iter().next() {
        if suite.pending_tasks == 0 && !suite.state.is_terminal() {
            // Passively transition to Complete state
            TaskSuites::Entity::update_many()
                .col_expr(
                    TaskSuites::Column::State,
                    Expr::value(TaskSuiteState::Complete),
                )
                .col_expr(TaskSuites::Column::CompletedAt, Expr::value(now))
                .col_expr(TaskSuites::Column::UpdatedAt, Expr::value(now))
                .filter(TaskSuites::Column::Id.eq(task_suite_id))
                .filter(TaskSuites::Column::PendingTasks.eq(0))
                .exec(db)
                .await?;

            tracing::debug!(
                task_suite_id = task_suite_id,
                "Task suite transitioned to Complete (all tasks finished)"
            );
        }
    }

    Ok(())
}

/// Manually closes a task suite, preventing new tasks from being added.
/// This transitions the suite from Open to Closed state.
pub async fn close_task_suite(
    db: &DatabaseConnection,
    task_suite_id: i64,
) -> Result<TaskSuites::Model> {
    let now = TimeDateTimeWithTimeZone::now_utc();

    let updated = TaskSuites::Entity::update_many()
        .col_expr(
            TaskSuites::Column::State,
            Expr::value(TaskSuiteState::Closed),
        )
        .col_expr(TaskSuites::Column::UpdatedAt, Expr::value(now))
        .filter(TaskSuites::Column::Id.eq(task_suite_id))
        .filter(TaskSuites::Column::State.eq(TaskSuiteState::Open))
        .exec_with_returning(db)
        .await?;

    updated.into_iter().next().ok_or_else(|| {
        crate::error::Error::ApiError(crate::error::ApiError::NotFound(
            "Task suite not found or already closed".to_string(),
        ))
    })
}

/// Manually cancels a task suite, marking it as Cancelled.
/// This will not affect already-running tasks.
pub async fn cancel_task_suite(
    db: &DatabaseConnection,
    task_suite_id: i64,
) -> Result<TaskSuites::Model> {
    let now = TimeDateTimeWithTimeZone::now_utc();

    let updated = TaskSuites::Entity::update_many()
        .col_expr(
            TaskSuites::Column::State,
            Expr::value(TaskSuiteState::Cancelled),
        )
        .col_expr(TaskSuites::Column::UpdatedAt, Expr::value(now))
        .col_expr(TaskSuites::Column::CompletedAt, Expr::value(now))
        .filter(TaskSuites::Column::Id.eq(task_suite_id))
        .filter(TaskSuites::Column::State.ne(TaskSuiteState::Cancelled))
        .exec_with_returning(db)
        .await?;

    updated.into_iter().next().ok_or_else(|| {
        crate::error::Error::ApiError(crate::error::ApiError::NotFound(
            "Task suite not found or already in terminal state".to_string(),
        ))
    })
}
