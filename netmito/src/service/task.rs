use sea_orm::sea_query::{
    extension::postgres::PgExpr, Alias, CommonTableExpression, DeleteStatement, ExprTrait,
    InsertStatement, PgFunc, Query,
};
use sea_orm::ActiveValue::NotSet;
use sea_orm::{prelude::*, ConnectionTrait, FromQueryResult, Set, TransactionTrait};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

use crate::entity::role::{GroupWorkerRole, UserGroupRole};
use crate::entity::state::{TaskState, TaskSuiteState};
use crate::error::{ApiError, ErrorMsg};
use crate::schema::{
    ArtifactQueryResp, ChangeTaskReq, CountQuery, ParsedTaskQueryInfo, SubmitTaskReq,
    SubmitTaskResp, TaskQueryInfo, TaskQueryResp, TaskResultSpec, TasksCancelByFilterReq,
    TasksCancelByFilterResp, TasksCancelByUuidsReq, TasksCancelByUuidsResp, TasksQueryReq,
    TasksQueryResp, UpdateTaskLabelsReq,
};
use crate::{config::InfraPool, schema::ExecSpec};

use crate::{
    entity::{
        active_tasks as ActiveTasks, archived_tasks as ArchivedTasks, artifacts as Artifact,
        group_worker as GroupWorker, groups as Group, task_suites as TaskSuites,
        user_group as UserGroup, users as User, workers as Worker,
    },
    error::Error,
};

use super::worker::{remove_task, TaskDispatcherOp};

// XXX: Not sure if we can relax the constrains on local path checking.
// We currently only check if the path is absolute or contains `..` and not check for `.`.
fn check_exec_spec(spec: &ExecSpec) -> crate::error::Result<()> {
    if spec.resources.iter().any(|r| {
        r.local_path.is_absolute()
            || r.local_path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
    }) {
        return Err(Error::ApiError(crate::error::ApiError::InvalidRequest(
            "Resource local path is absolute or contains `..`".to_string(),
        )));
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum Submitter {
    User,
    /// A running task spawning a downstream child, identified by the parent's
    /// uuid and the group the parent itself lives in.
    Task {
        upstream_task_uuid: Uuid,
        parent_group_id: i64,
    },
}

/// Take one more task onto a suite: both counters up by column expression, the
/// submission clock reset, the suite back to `Open`. Matches no row, and so
/// returns none, when the suite is `Cancelled`.
fn accept_task_into_suite_query(
    suite_id: i64,
    now: TimeDateTimeWithTimeZone,
) -> sea_orm::UpdateMany<TaskSuites::Entity> {
    TaskSuites::Entity::update_many()
        .col_expr(
            TaskSuites::Column::TotalTasks,
            Expr::col(TaskSuites::Column::TotalTasks).add(1),
        )
        .col_expr(
            TaskSuites::Column::IncompleteTasks,
            Expr::col(TaskSuites::Column::IncompleteTasks).add(1),
        )
        .col_expr(
            TaskSuites::Column::LastTaskSubmittedAt,
            Expr::value(Some(now)),
        )
        .col_expr(TaskSuites::Column::UpdatedAt, Expr::value(now))
        .col_expr(TaskSuites::Column::State, Expr::value(TaskSuiteState::Open))
        .col_expr(
            TaskSuites::Column::CompletedAt,
            Expr::value(None::<TimeDateTimeWithTimeZone>),
        )
        .filter(TaskSuites::Column::Id.eq(suite_id))
        .filter(TaskSuites::Column::State.ne(TaskSuiteState::Cancelled))
}

async fn internal_submit_task(
    pool: &InfraPool,
    creator_id: i64,
    submitter: Submitter,
    SubmitTaskReq {
        group_name,
        suite_uuid,
        tags,
        labels,
        priority,
        spec,
        exec_options,
    }: SubmitTaskReq,
) -> crate::error::Result<SubmitTaskResp> {
    let tags = Vec::from_iter(tags);
    let labels = Vec::from_iter(labels);
    let now = TimeDateTimeWithTimeZone::now_utc();

    // Used later when creating active tasks active model
    let (state, upstream_task_uuid) = match submitter {
        Submitter::User => (Set(crate::entity::state::TaskState::Ready), NotSet),
        Submitter::Task {
            upstream_task_uuid, ..
        } => (
            Set(crate::entity::state::TaskState::Pending),
            Set(Some(upstream_task_uuid)),
        ),
    };

    check_exec_spec(&spec)?;
    let spec_json = serde_json::to_value(&spec)?;
    let exec_options_json = exec_options
        .as_ref()
        .map(serde_json::to_value)
        .transpose()?;

    let (task, suite) = pool
        .db
        .transaction::<_, (ActiveTasks::Model, Option<TaskSuites::Model>), crate::error::Error>(
            |txn| {
                Box::pin(async move {
                    // Resolve the designated suite, if any. How it is authorized
                    // depends on the submitter: a user reaches a suite through their
                    // own group memberships, while a task spawning a child may target
                    // any suite owned by the group the parent task itself lives in.
                    //
                    // The parent's creator is deliberately not re-checked, so a long-running suite
                    // keeps spawning work after that user leaves the group.
                    let suite = match (suite_uuid, submitter) {
                        (None, _) => None,
                        (Some(suite_uuid), Submitter::User) => {
                            // Resolve the suite and the caller's membership in its
                            // owning group in one join.
                            let builder = txn.get_database_backend();
                            let suite_stmt = Query::select()
                                .columns([
                                    (TaskSuites::Entity, TaskSuites::Column::Id),
                                    (TaskSuites::Entity, TaskSuites::Column::Uuid),
                                    (TaskSuites::Entity, TaskSuites::Column::Name),
                                    (TaskSuites::Entity, TaskSuites::Column::Description),
                                    (TaskSuites::Entity, TaskSuites::Column::GroupId),
                                    (TaskSuites::Entity, TaskSuites::Column::CreatorId),
                                    (TaskSuites::Entity, TaskSuites::Column::Tags),
                                    (TaskSuites::Entity, TaskSuites::Column::Labels),
                                    (TaskSuites::Entity, TaskSuites::Column::Priority),
                                    (TaskSuites::Entity, TaskSuites::Column::WorkerSchedule),
                                    (TaskSuites::Entity, TaskSuites::Column::ExecHooks),
                                    (TaskSuites::Entity, TaskSuites::Column::State),
                                    (TaskSuites::Entity, TaskSuites::Column::LastTaskSubmittedAt),
                                    (TaskSuites::Entity, TaskSuites::Column::TotalTasks),
                                    (TaskSuites::Entity, TaskSuites::Column::IncompleteTasks),
                                    (TaskSuites::Entity, TaskSuites::Column::CreatedAt),
                                    (TaskSuites::Entity, TaskSuites::Column::UpdatedAt),
                                    (TaskSuites::Entity, TaskSuites::Column::CompletedAt),
                                ])
                                .from(TaskSuites::Entity)
                                .join(
                                    sea_orm::JoinType::Join,
                                    UserGroup::Entity,
                                    Expr::col((UserGroup::Entity, UserGroup::Column::GroupId)).eq(
                                        Expr::col((
                                            TaskSuites::Entity,
                                            TaskSuites::Column::GroupId,
                                        )),
                                    ),
                                )
                                .and_where(
                                    Expr::col((TaskSuites::Entity, TaskSuites::Column::Uuid))
                                        .eq(suite_uuid),
                                )
                                .and_where(
                                    Expr::col((UserGroup::Entity, UserGroup::Column::UserId))
                                        .eq(creator_id),
                                )
                                .to_owned();
                            let suite =
                                TaskSuites::Model::find_by_statement(builder.build(&suite_stmt))
                                    .one(txn)
                                    .await?
                                    .ok_or_else(|| {
                                        Error::ApiError(crate::error::ApiError::NotFound(format!(
                                            "User doesn't have permission or suite with uuid {}",
                                            suite_uuid
                                        )))
                                    })?;
                            Some(suite)
                        }
                        (
                            Some(suite_uuid),
                            Submitter::Task {
                                parent_group_id, ..
                            },
                        ) => Some(
                            TaskSuites::Entity::find()
                                .filter(TaskSuites::Column::Uuid.eq(suite_uuid))
                                .filter(TaskSuites::Column::GroupId.eq(parent_group_id))
                                .one(txn)
                                .await?
                                .ok_or_else(|| {
                                    // A suite owned by another group is reported the
                                    // same way as one that does not exist.
                                    Error::ApiError(crate::error::ApiError::NotFound(format!(
                                        "Suite with uuid {} in the parent task's group",
                                        suite_uuid
                                    )))
                                })?,
                        ),
                    };

                    // The suite must be able to accept new tasks.
                    if let Some(suite) = &suite {
                        if !suite.state.can_accept_tasks() {
                            return Err(Error::ApiError(crate::error::ApiError::InvalidRequest(
                                format!(
                                    "Suite is in {} state and cannot accept new tasks",
                                    suite.state
                                ),
                            )));
                        }
                    }

                    // The owning group, when it is already pinned by something other
                    // than `group_name`: a designated suite owns the task, and a
                    // task-spawned child always stays in its parent's group. `None`
                    // is the plain user path, where the group is resolved from
                    // `group_name` and the caller's membership.
                    let pinned_group_id = match (&suite, submitter) {
                        (Some(suite), _) => Some(suite.group_id),
                        (
                            None,
                            Submitter::Task {
                                parent_group_id, ..
                            },
                        ) => Some(parent_group_id),
                        (None, Submitter::User) => None,
                    };

                    // Bump the owning group's task counter atomically.
                    let (group_id, task_id) = match pinned_group_id {
                        Some(pinned_group_id) => {
                            let group = Group::Entity::update_many()
                                .col_expr(
                                    Group::Column::TaskCount,
                                    Expr::col(Group::Column::TaskCount).add(1),
                                )
                                .col_expr(Group::Column::UpdatedAt, Expr::value(now))
                                .filter(Group::Column::Id.eq(pinned_group_id))
                                .exec_with_returning(txn)
                                .await?
                                .into_iter()
                                .next()
                                .ok_or_else(|| {
                                    // The row must exist: both the suite and the
                                    // parent task hold an FK to it.
                                    tracing::error!("Owning group {} not found", pinned_group_id);
                                    Error::ApiError(crate::error::ApiError::InternalServerError)
                                })?;
                            // The request's group_name must match
                            if group.group_name != group_name {
                                let owner = match &suite {
                                    Some(suite) => {
                                        format!(
                                            "Suite {} belongs to group {}",
                                            suite.uuid, group.group_name
                                        )
                                    }
                                    None => format!(
                                        "The parent task belongs to group {}",
                                        group.group_name
                                    ),
                                };
                                return Err(Error::ApiError(
                                    crate::error::ApiError::InvalidRequest(format!(
                                        "{}, not {}",
                                        owner, group_name
                                    )),
                                ));
                            }
                            (group.id, group.task_count)
                        }
                        None => {
                            // Resolve the user-specified group, verify the caller's
                            // membership, and bump its task counter in one statement.
                            // Membership goes through a subquery rather than a
                            // joined `FROM user_group`: sea-orm emits an
                            // unqualified `RETURNING "id", …`, which Postgres
                            // rejects as ambiguous the moment a second table
                            // with an `id` column is in scope. Every suite-less
                            // `POST /tasks` 500s without this.
                            let member_of = Query::select()
                                .column(UserGroup::Column::GroupId)
                                .from(UserGroup::Entity)
                                .and_where(Expr::col(UserGroup::Column::UserId).eq(creator_id))
                                // TODO: when there is 'exec' access level, enforce access check here.
                                //
                                // .and_where(
                                //     Expr::col(UserGroup::Column::Role)
                                //         .gte(UserGroupRole::Write),
                                // )
                                .to_owned();
                            let group = Group::Entity::update_many()
                                .col_expr(
                                    Group::Column::TaskCount,
                                    Expr::col((Group::Entity, Group::Column::TaskCount)).add(1),
                                )
                                .col_expr(Group::Column::UpdatedAt, Expr::value(now))
                                .filter(Group::Column::GroupName.eq(&group_name))
                                .filter(Group::Column::Id.in_subquery(member_of))
                                .exec_with_returning(txn)
                                .await?
                                .into_iter()
                                .next()
                                .ok_or_else(|| {
                                    Error::ApiError(crate::error::ApiError::NotFound(format!(
                                        "User doesn't have permission or group with name {}",
                                        group_name
                                    )))
                                })?;
                            (group.id, group.task_count)
                        }
                    };

                    let suite = match suite {
                        None => None,
                        Some(suite) => Some(
                            accept_task_into_suite_query(suite.id, now)
                                .exec_with_returning(txn)
                                .await?
                                .into_iter()
                                .next()
                                .ok_or_else(|| {
                                    Error::ApiError(crate::error::ApiError::InvalidRequest(
                                        format!(
                                            "Suite {} was cancelled and cannot accept new tasks",
                                            suite.uuid
                                        ),
                                    ))
                                })?,
                        ),
                    };

                    let task_uuid = Uuid::new_v4();

                    let task = ActiveTasks::ActiveModel {
                        creator_id: Set(creator_id),
                        group_id: Set(group_id),
                        task_id: Set(task_id),
                        uuid: Set(task_uuid),
                        tags: Set(tags),
                        labels: Set(labels),
                        created_at: Set(now),
                        updated_at: Set(now),
                        state,
                        runner_uuid: Set(None),
                        priority: Set(priority),
                        spec: Set(spec_json),
                        exec_options: Set(exec_options_json),
                        result: Set(None),
                        upstream_task_uuid,
                        task_suite_id: Set(suite.as_ref().map(|s| s.id)),
                        ..Default::default()
                    };
                    let task = task.insert(txn).await?;
                    Ok((task, suite))
                })
            },
        )
        .await?;

    // If task is pending, then we have done
    if matches!(task.state, TaskState::Pending) {
        return Ok(SubmitTaskResp {
            task_id: task.task_id,
            uuid: task.uuid,
        });
    };

    // Not pending, notify agent/worker depending on whether it belongs to a suite
    match suite {
        Some(suite) => {
            // Suite tasks are pulled by agents from the suite, not pushed into
            // worker queues; all this does is wake the idle eligible agents so
            // one picks the suite up without waiting for its next heartbeat.
            crate::service::agent::notify_suite_available(pool, suite.id).await;
        }
        None => {
            let builder = pool.db.get_database_backend();
            let tasks_stmt = Query::select()
                .column((Worker::Entity, ActiveTasks::Column::Id))
                .from(Worker::Entity)
                .join(
                    sea_orm::JoinType::Join,
                    GroupWorker::Entity,
                    Expr::col((GroupWorker::Entity, GroupWorker::Column::WorkerId))
                        .eq(Expr::col((Worker::Entity, Worker::Column::Id))),
                )
                .and_where(
                    Expr::col((GroupWorker::Entity, GroupWorker::Column::GroupId))
                        .eq(task.group_id),
                )
                .and_where(
                    Expr::col((GroupWorker::Entity, GroupWorker::Column::Role)).eq(PgFunc::any(
                        vec![GroupWorkerRole::Write, GroupWorkerRole::Admin],
                    )),
                )
                .and_where(Expr::col((Worker::Entity, Worker::Column::Tags)).contains(task.tags))
                .to_owned();
            let workers: Vec<PartialWorkerId> =
                PartialWorkerId::find_by_statement(builder.build(&tasks_stmt))
                    .all(&pool.db)
                    .await?;
            let op = TaskDispatcherOp::BatchAddTask(
                workers.into_iter().map(i64::from).collect(),
                task.id,
                task.priority,
            );
            if pool.worker_task_queue_tx.send(op).is_err() {
                return Err(Error::Custom("send batch add task failed".to_string()));
            }
        }
    }
    Ok(SubmitTaskResp {
        task_id: task.task_id,
        uuid: task.uuid,
    })
}

pub async fn user_submit_task(
    pool: &InfraPool,
    creator_id: i64,
    req: SubmitTaskReq,
) -> crate::error::Result<SubmitTaskResp> {
    internal_submit_task(pool, creator_id, Submitter::User, req).await
}

/// Submit the downstream child of a running task, on behalf of the worker or
/// agent executing it.
///
/// The child stays in the parent's group, and `req.suite_uuid` may name any
/// suite that group owns — the parent's own suite, a sibling one, or `None` for
/// a suite-less task dispatched to workers.
pub async fn worker_submit_pending_task(
    pool: &InfraPool,
    creator_id: i64,
    upstream_task_uuid: Uuid,
    parent_group_id: i64,
    req: SubmitTaskReq,
) -> crate::error::Result<SubmitTaskResp> {
    internal_submit_task(
        pool,
        creator_id,
        Submitter::Task {
            upstream_task_uuid,
            parent_group_id,
        },
        req,
    )
    .await
}

pub async fn worker_trigger_pending_task(pool: &InfraPool, uuid: Uuid) -> crate::error::Result<()> {
    tracing::debug!("Trigger pending task {uuid}");
    let task = ActiveTasks::Entity::find()
        .filter(ActiveTasks::Column::Uuid.eq(uuid))
        .filter(ActiveTasks::Column::State.eq(TaskState::Pending))
        .one(&pool.db)
        .await?
        .ok_or(Error::ApiError(crate::error::ApiError::NotFound(format!(
            "Pending task with uuid {uuid}"
        ))))?;
    let mut task: ActiveTasks::ActiveModel = task.into();
    task.state = Set(TaskState::Ready);
    task.updated_at = Set(TimeDateTimeWithTimeZone::now_utc());
    let task = task.update(&pool.db).await?;
    // A suite task never enters the worker queues — agents pull it from its
    // suite. Wake the idle eligible agents instead.
    if let Some(suite_id) = task.task_suite_id {
        crate::service::agent::notify_suite_available(pool, suite_id).await;
        return Ok(());
    }
    // Batch add task to worker task queues
    let builder = pool.db.get_database_backend();
    let tasks_stmt = Query::select()
        .column((Worker::Entity, ActiveTasks::Column::Id))
        .from(Worker::Entity)
        .join(
            sea_orm::JoinType::Join,
            GroupWorker::Entity,
            Expr::col((GroupWorker::Entity, GroupWorker::Column::WorkerId))
                .eq(Expr::col((Worker::Entity, Worker::Column::Id))),
        )
        .and_where(Expr::col((GroupWorker::Entity, GroupWorker::Column::GroupId)).eq(task.group_id))
        .and_where(
            Expr::col((GroupWorker::Entity, GroupWorker::Column::Role)).eq(PgFunc::any(vec![
                GroupWorkerRole::Write,
                GroupWorkerRole::Admin,
            ])),
        )
        .and_where(Expr::col((Worker::Entity, Worker::Column::Tags)).contains(task.tags))
        .to_owned();
    let workers: Vec<PartialWorkerId> =
        PartialWorkerId::find_by_statement(builder.build(&tasks_stmt))
            .all(&pool.db)
            .await?;
    let op = TaskDispatcherOp::BatchAddTask(
        workers.into_iter().map(i64::from).collect(),
        task.id,
        task.priority,
    );
    if pool.worker_task_queue_tx.send(op).is_err() {
        Err(Error::Custom("send batch add task failed".to_string()))
    } else {
        Ok(())
    }
}

pub async fn user_change_task(
    pool: &InfraPool,
    user_id: i64,
    uuid: Uuid,
    ChangeTaskReq {
        tags,
        priority,
        spec,
        exec_options,
    }: ChangeTaskReq,
) -> crate::error::Result<()> {
    if tags.is_none() && priority.is_none() && spec.is_none() && exec_options.is_none() {
        return Err(Error::ApiError(crate::error::ApiError::InvalidRequest(
            "No change specified".to_string(),
        )));
    }
    let task = pool
        .db
        .transaction::<_, ActiveTasks::Model, crate::error::Error>(|txn| {
            Box::pin(async move {
                let task = ActiveTasks::Entity::find()
                    .filter(ActiveTasks::Column::Uuid.eq(uuid))
                    .filter(ActiveTasks::Column::State.eq(TaskState::Ready))
                    .one(txn)
                    .await?
                    .ok_or(Error::ApiError(crate::error::ApiError::NotFound(format!(
                        "Task with uuid {uuid}"
                    ))))?;
                let user_group_role = UserGroup::Entity::find()
                    .filter(UserGroup::Column::UserId.eq(user_id))
                    .filter(UserGroup::Column::GroupId.eq(task.group_id))
                    .one(txn)
                    .await?
                    .ok_or(Error::ApiError(crate::error::ApiError::InvalidRequest(
                        "User is not in the group".to_string(),
                    )))?;
                match user_group_role.role {
                    UserGroupRole::Admin | UserGroupRole::Write => {}
                    _ => {
                        return Err(Error::AuthError(crate::error::AuthError::PermissionDenied));
                    }
                }
                let mut task: ActiveTasks::ActiveModel = task.into();
                let now = TimeDateTimeWithTimeZone::now_utc();
                task.updated_at = Set(now);
                if let Some(tags) = tags {
                    task.tags = Set(Vec::from_iter(tags));
                }
                if let Some(spec) = spec {
                    check_exec_spec(&spec)?;
                    let spec_json = serde_json::to_value(spec)?;
                    task.spec = Set(spec_json);
                }
                if let Some(exec_options) = exec_options {
                    task.exec_options = Set(Some(serde_json::to_value(exec_options)?));
                }
                if let Some(priority) = priority {
                    task.priority = Set(priority);
                }
                let task = task.update(txn).await?;

                Ok(task)
            })
        })
        .await?;
    // Suite-bound tasks never live in the worker task queues, so there is nothing to
    // re-dispatch after a change.
    // TODO: notify change to agents if this task belongs to a task suite
    if task.task_suite_id.is_some() {
        return Ok(());
    }
    let builder = pool.db.get_database_backend();
    let tasks_stmt = Query::select()
        .column((Worker::Entity, ActiveTasks::Column::Id))
        .from(Worker::Entity)
        .join(
            sea_orm::JoinType::Join,
            GroupWorker::Entity,
            Expr::col((GroupWorker::Entity, GroupWorker::Column::WorkerId))
                .eq(Expr::col((Worker::Entity, Worker::Column::Id))),
        )
        .and_where(Expr::col((GroupWorker::Entity, GroupWorker::Column::GroupId)).eq(task.group_id))
        .and_where(
            Expr::col((GroupWorker::Entity, GroupWorker::Column::Role)).eq(PgFunc::any(vec![
                GroupWorkerRole::Write,
                GroupWorkerRole::Admin,
            ])),
        )
        .and_where(Expr::col((Worker::Entity, Worker::Column::Tags)).contains(task.tags))
        .to_owned();
    let workers: Vec<PartialWorkerId> =
        PartialWorkerId::find_by_statement(builder.build(&tasks_stmt))
            .all(&pool.db)
            .await?;
    let op = TaskDispatcherOp::RemoveTask(task.id);
    if pool.worker_task_queue_tx.send(op).is_err() {
        return Err(Error::Custom("send remove task op failed".to_string()));
    }
    let op = TaskDispatcherOp::BatchAddTask(
        workers.into_iter().map(i64::from).collect(),
        task.id,
        task.priority,
    );
    if pool.worker_task_queue_tx.send(op).is_err() {
        Err(Error::Custom("send batch add task op failed".to_string()))
    } else {
        Ok(())
    }
}

pub async fn user_change_task_labels(
    pool: &InfraPool,
    user_id: i64,
    uuid: Uuid,
    req: UpdateTaskLabelsReq,
) -> crate::error::Result<()> {
    let labels = req.labels.into_iter().collect::<Vec<_>>();
    pool.db
        .transaction::<_, (), crate::error::Error>(|txn| {
            Box::pin(async move {
                let task = ActiveTasks::Entity::find()
                    .filter(ActiveTasks::Column::Uuid.eq(uuid))
                    .one(txn)
                    .await?;
                if let Some(task) = task {
                    let user_group_role = UserGroup::Entity::find()
                        .filter(UserGroup::Column::UserId.eq(user_id))
                        .filter(UserGroup::Column::GroupId.eq(task.group_id))
                        .one(txn)
                        .await?
                        .ok_or(Error::ApiError(crate::error::ApiError::InvalidRequest(
                            "User is not in the group".to_string(),
                        )))?;
                    match user_group_role.role {
                        UserGroupRole::Admin | UserGroupRole::Write => {}
                        _ => {
                            return Err(Error::AuthError(
                                crate::error::AuthError::PermissionDenied,
                            ));
                        }
                    }
                    let mut task: ActiveTasks::ActiveModel = task.into();
                    let now = TimeDateTimeWithTimeZone::now_utc();
                    task.updated_at = Set(now);
                    task.labels = Set(labels);
                    task.update(txn).await?;
                } else {
                    let task = ArchivedTasks::Entity::find()
                        .filter(ArchivedTasks::Column::Uuid.eq(uuid))
                        .one(txn)
                        .await?
                        .ok_or(Error::ApiError(crate::error::ApiError::NotFound(format!(
                            "Task with uuid {uuid}"
                        ))))?;
                    let user_group_role = UserGroup::Entity::find()
                        .filter(UserGroup::Column::UserId.eq(user_id))
                        .filter(UserGroup::Column::GroupId.eq(task.group_id))
                        .one(txn)
                        .await?
                        .ok_or(Error::ApiError(crate::error::ApiError::InvalidRequest(
                            "User is not in the group".to_string(),
                        )))?;
                    match user_group_role.role {
                        UserGroupRole::Admin | UserGroupRole::Write => {}
                        _ => {
                            return Err(Error::AuthError(
                                crate::error::AuthError::PermissionDenied,
                            ));
                        }
                    }
                    let mut task: ArchivedTasks::ActiveModel = task.into();
                    let now = TimeDateTimeWithTimeZone::now_utc();
                    task.updated_at = Set(now);
                    task.labels = Set(labels);
                    task.update(txn).await?;
                }
                Ok(())
            })
        })
        .await?;
    Ok(())
}

#[derive(FromQueryResult)]
struct UserGroupRoleWithName {
    role: UserGroupRole,
    username: String,
}

pub async fn user_cancel_task(
    pool: &InfraPool,
    user_id: i64,
    uuid: Uuid,
) -> crate::error::Result<()> {
    let (task_id, username) = pool
        .db
        .transaction::<_, (i64, String), crate::error::Error>(|txn| {
            Box::pin(async move {
                let task = ActiveTasks::Entity::find()
                    .filter(ActiveTasks::Column::Uuid.eq(uuid))
                    .filter(ActiveTasks::Column::State.eq(TaskState::Ready))
                    .one(txn)
                    .await?
                    .ok_or(Error::ApiError(crate::error::ApiError::NotFound(format!(
                        "Task with uuid {uuid}"
                    ))))?;
                let builder = txn.get_database_backend();
                let role_stmt = Query::select()
                    .column((UserGroup::Entity, UserGroup::Column::Role))
                    .column((User::Entity, User::Column::Username))
                    .from(UserGroup::Entity)
                    .join(
                        sea_orm::JoinType::Join,
                        User::Entity,
                        Expr::col((User::Entity, User::Column::Id))
                            .eq(Expr::col((UserGroup::Entity, UserGroup::Column::UserId))),
                    )
                    .and_where(
                        Expr::col((UserGroup::Entity, UserGroup::Column::UserId)).eq(user_id),
                    )
                    .and_where(
                        Expr::col((UserGroup::Entity, UserGroup::Column::GroupId))
                            .eq(task.group_id),
                    )
                    .to_owned();
                let user_group_role =
                    UserGroupRoleWithName::find_by_statement(builder.build(&role_stmt))
                        .one(txn)
                        .await?
                        .ok_or(Error::ApiError(crate::error::ApiError::InvalidRequest(
                            "User is not in the group".to_string(),
                        )))?;
                match user_group_role.role {
                    UserGroupRole::Admin | UserGroupRole::Write => {}
                    _ => {
                        return Err(Error::AuthError(crate::error::AuthError::PermissionDenied));
                    }
                }
                let now = TimeDateTimeWithTimeZone::now_utc();
                let suite_id = task.task_suite_id;
                let res = TaskResultSpec {
                    exit_status: 0,
                    msg: Some(crate::schema::TaskResultMessage::UserCancellation),
                };
                let result = serde_json::to_value(res).inspect_err(|e| tracing::error!("{}", e))?;
                let archived_task = ArchivedTasks::ActiveModel {
                    id: Set(task.id),
                    creator_id: Set(task.creator_id),
                    group_id: Set(task.group_id),
                    task_id: Set(task.task_id),
                    uuid: Set(task.uuid),
                    tags: Set(task.tags),
                    labels: Set(task.labels),
                    created_at: Set(task.created_at),
                    updated_at: Set(now),
                    state: Set(TaskState::Cancelled),
                    runner_uuid: Set(task.runner_uuid),
                    priority: Set(task.priority),
                    spec: Set(task.spec),
                    exec_options: Set(task.exec_options),
                    result: Set(Some(result)),
                    upstream_task_uuid: Set(task.upstream_task_uuid),
                    downstream_task_uuid: Set(task.downstream_task_uuid),
                    task_suite_id: Set(task.task_suite_id),
                };
                archived_task.insert(txn).await?;
                ActiveTasks::Entity::delete_by_id(task.id).exec(txn).await?;
                // A cancelled suite task never reaches an agent `Commit`, so
                // this is the only chance to give the suite its count back.
                if let Some(suite_id) = suite_id {
                    crate::service::suite::decrement_incomplete_tasks(txn, suite_id, 1, now)
                        .await?;
                }
                Ok((task.id, user_group_role.username))
            })
        })
        .await?;
    tracing::info!("User {} cancelled task {}", username, uuid);
    let _ = remove_task(task_id, pool)
        .inspect_err(|e| tracing::warn!("Failed to remove task {}: {:?}", task_id, e));
    Ok(())
}

pub async fn get_task_by_uuid(pool: &InfraPool, uuid: Uuid) -> crate::error::Result<TaskQueryResp> {
    let active_task_stmt = Query::select()
        .columns([
            (ActiveTasks::Entity, ActiveTasks::Column::Uuid),
            (ActiveTasks::Entity, ActiveTasks::Column::TaskId),
            (ActiveTasks::Entity, ActiveTasks::Column::Tags),
            (ActiveTasks::Entity, ActiveTasks::Column::Labels),
            (ActiveTasks::Entity, ActiveTasks::Column::CreatedAt),
            (ActiveTasks::Entity, ActiveTasks::Column::UpdatedAt),
            (ActiveTasks::Entity, ActiveTasks::Column::State),
            (ActiveTasks::Entity, ActiveTasks::Column::Priority),
            (ActiveTasks::Entity, ActiveTasks::Column::Spec),
            (ActiveTasks::Entity, ActiveTasks::Column::ExecOptions),
            (ActiveTasks::Entity, ActiveTasks::Column::Result),
            (ActiveTasks::Entity, ActiveTasks::Column::UpstreamTaskUuid),
            (ActiveTasks::Entity, ActiveTasks::Column::DownstreamTaskUuid),
            (ActiveTasks::Entity, ActiveTasks::Column::RunnerUuid),
        ])
        .expr_as(
            Expr::col((User::Entity, User::Column::Username)),
            Alias::new("creator_username"),
        )
        .expr_as(
            Expr::col((Group::Entity, Group::Column::GroupName)),
            Alias::new("group_name"),
        )
        .from(ActiveTasks::Entity)
        .join(
            sea_orm::JoinType::Join,
            User::Entity,
            Expr::col((User::Entity, User::Column::Id)).eq(Expr::col((
                ActiveTasks::Entity,
                ActiveTasks::Column::CreatorId,
            ))),
        )
        .join(
            sea_orm::JoinType::Join,
            Group::Entity,
            Expr::col((ActiveTasks::Entity, ActiveTasks::Column::GroupId))
                .eq(Expr::col((Group::Entity, Group::Column::Id))),
        )
        .and_where(Expr::col((ActiveTasks::Entity, ActiveTasks::Column::Uuid)).eq(uuid))
        .limit(1)
        .to_owned();
    let archive_task_stmt = Query::select()
        .columns([
            (ArchivedTasks::Entity, ArchivedTasks::Column::Uuid),
            (ArchivedTasks::Entity, ArchivedTasks::Column::TaskId),
            (ArchivedTasks::Entity, ArchivedTasks::Column::Tags),
            (ArchivedTasks::Entity, ArchivedTasks::Column::Labels),
            (ArchivedTasks::Entity, ArchivedTasks::Column::CreatedAt),
            (ArchivedTasks::Entity, ArchivedTasks::Column::UpdatedAt),
            (ArchivedTasks::Entity, ArchivedTasks::Column::State),
            (ArchivedTasks::Entity, ArchivedTasks::Column::Priority),
            (ArchivedTasks::Entity, ArchivedTasks::Column::Spec),
            (ArchivedTasks::Entity, ArchivedTasks::Column::ExecOptions),
            (ArchivedTasks::Entity, ArchivedTasks::Column::Result),
            (
                ArchivedTasks::Entity,
                ArchivedTasks::Column::UpstreamTaskUuid,
            ),
            (
                ArchivedTasks::Entity,
                ArchivedTasks::Column::DownstreamTaskUuid,
            ),
            (ArchivedTasks::Entity, ArchivedTasks::Column::RunnerUuid),
        ])
        .expr_as(
            Expr::col((User::Entity, User::Column::Username)),
            Alias::new("creator_username"),
        )
        .expr_as(
            Expr::col((Group::Entity, Group::Column::GroupName)),
            Alias::new("group_name"),
        )
        .from(ArchivedTasks::Entity)
        .join(
            sea_orm::JoinType::Join,
            User::Entity,
            Expr::col((User::Entity, User::Column::Id)).eq(Expr::col((
                ArchivedTasks::Entity,
                ArchivedTasks::Column::CreatorId,
            ))),
        )
        .join(
            sea_orm::JoinType::Join,
            Group::Entity,
            Expr::col((ArchivedTasks::Entity, ArchivedTasks::Column::GroupId))
                .eq(Expr::col((Group::Entity, Group::Column::Id))),
        )
        .and_where(Expr::col((ArchivedTasks::Entity, ArchivedTasks::Column::Uuid)).eq(uuid))
        .limit(1)
        .to_owned();
    let builder = pool.db.get_database_backend();
    let info = match TaskQueryInfo::find_by_statement(builder.build(&active_task_stmt))
        .one(&pool.db)
        .await?
    {
        Some(task) => Some(task),
        None => {
            TaskQueryInfo::find_by_statement(builder.build(&archive_task_stmt))
                .one(&pool.db)
                .await?
        }
    }
    .ok_or(Error::ApiError(crate::error::ApiError::NotFound(format!(
        "Task with uuid {uuid}"
    ))))?;
    let artifacts: Vec<ArtifactQueryResp> = Artifact::Entity::find()
        .filter(Artifact::Column::TaskId.eq(uuid))
        .all(&pool.db)
        .await?
        .into_iter()
        .map(Into::into)
        .collect();
    let info = ParsedTaskQueryInfo {
        uuid: info.uuid,
        creator_username: info.creator_username,
        group_name: info.group_name,
        task_id: info.task_id,
        tags: info.tags,
        labels: info.labels,
        created_at: info.created_at,
        updated_at: info.updated_at,
        state: info.state,
        priority: info.priority,
        spec: serde_json::from_value(info.spec)?,
        exec_options: info.exec_options.map(serde_json::from_value).transpose()?,
        result: info.result.map(serde_json::from_value).transpose()?,
        upstream_task_uuid: info.upstream_task_uuid,
        downstream_task_uuid: info.downstream_task_uuid,
        runner_uuid: info.runner_uuid,
    };
    Ok(TaskQueryResp { info, artifacts })
}

/// Returns the name of `user_id`, read from the same row as the role check so
/// callers that want to log it pay no extra query.
pub(crate) async fn check_task_list_query(
    user_id: i64,
    pool: &InfraPool,
    query: &mut TasksQueryReq,
    role: UserGroupRole,
) -> crate::error::Result<String> {
    if let Some(ref tags) = query.tags {
        if tags.is_empty() {
            return Err(Error::ApiError(crate::error::ApiError::InvalidRequest(
                "Tags cannot be empty if specified".to_string(),
            )));
        }
    }
    if let Some(ref labels) = query.labels {
        if labels.is_empty() {
            return Err(Error::ApiError(crate::error::ApiError::InvalidRequest(
                "Labels cannot be empty if specified".to_string(),
            )));
        }
    }
    if let Some(ref creator_usernames) = query.creator_usernames {
        if creator_usernames.is_empty() {
            return Err(Error::ApiError(crate::error::ApiError::InvalidRequest(
                "Creator username cannot be empty if specified".to_string(),
            )));
        }
    }
    if let Some(ref states) = query.states {
        if states.is_empty() {
            return Err(Error::ApiError(crate::error::ApiError::InvalidRequest(
                "State cannot be empty if specified".to_string(),
            )));
        }
    }
    let group_name = match query.group_name {
        Some(ref group_name) => group_name.clone(),
        None => {
            let username = User::Entity::find()
                .filter(User::Column::Id.eq(user_id))
                .one(&pool.db)
                .await?
                .ok_or(Error::ApiError(crate::error::ApiError::NotFound(
                    "User".to_string(),
                )))?
                .username;
            tracing::debug!("No group name specified, use username {} instead", username);
            query.group_name = Some(username.clone());
            username
        }
    };
    let builder = pool.db.get_database_backend();
    let role_stmt = Query::select()
        .column((UserGroup::Entity, UserGroup::Column::Role))
        .column((User::Entity, User::Column::Username))
        .from(UserGroup::Entity)
        .join(
            sea_orm::JoinType::Join,
            Group::Entity,
            Expr::col((Group::Entity, Group::Column::Id))
                .eq(Expr::col((UserGroup::Entity, UserGroup::Column::GroupId))),
        )
        .join(
            sea_orm::JoinType::Join,
            User::Entity,
            Expr::col((User::Entity, User::Column::Id))
                .eq(Expr::col((UserGroup::Entity, UserGroup::Column::UserId))),
        )
        .and_where(Expr::col((UserGroup::Entity, UserGroup::Column::UserId)).eq(user_id))
        .and_where(Expr::col((Group::Entity, Group::Column::GroupName)).eq(group_name.clone()))
        .to_owned();
    let query_role = UserGroupRoleWithName::find_by_statement(builder.build(&role_stmt))
        .one(&pool.db)
        .await?;
    match query_role {
        Some(r) if r.role >= role => Ok(r.username),
        Some(_) => Err(Error::AuthError(crate::error::AuthError::PermissionDenied)),
        None => Err(Error::ApiError(crate::error::ApiError::InvalidRequest(
            format!("Group with name {group_name} not found or user is not in the group"),
        ))),
    }
}

#[derive(Debug, Copy, Clone)]
pub(crate) enum OperatorWithNumber {
    Eq(i32),
    Neq(i32),
    Gt(i32),
    Gte(i32),
    Lt(i32),
    Lte(i32),
}

// Parse operators with number
pub(crate) fn parse_operators_with_number(s: &str) -> crate::error::Result<OperatorWithNumber> {
    fn parse_i32(s: &str) -> crate::error::Result<i32> {
        s.parse::<i32>().map_err(|e| {
            Error::ApiError(crate::error::ApiError::InvalidRequest(format!(
                "Failed to parse number from {s}: {e}"
            )))
        })
    }
    match s {
        s if s.starts_with(">=") => Ok(OperatorWithNumber::Gte(parse_i32(&s[2..])?)),
        s if s.starts_with("<=") => Ok(OperatorWithNumber::Lte(parse_i32(&s[2..])?)),
        s if s.starts_with("!=") => Ok(OperatorWithNumber::Neq(parse_i32(&s[2..])?)),
        s if s.starts_with('>') => Ok(OperatorWithNumber::Gt(parse_i32(&s[1..])?)),
        s if s.starts_with('<') => Ok(OperatorWithNumber::Lt(parse_i32(&s[1..])?)),
        s if s.starts_with('=') => Ok(OperatorWithNumber::Eq(parse_i32(&s[1..])?)),
        s => Ok(OperatorWithNumber::Eq(parse_i32(s)?)),
    }
}

/// Apply task query filters to both active and archived task query statements.
/// This helper function is shared between query and batch cancel operations.
pub(crate) fn apply_task_filters(
    active_stmt: &mut sea_orm::sea_query::SelectStatement,
    archive_stmt: &mut sea_orm::sea_query::SelectStatement,
    query: &TasksQueryReq,
) -> crate::error::Result<()> {
    if let Some(runner_uuid) = query.runner_uuid {
        active_stmt.and_where(
            Expr::col((ActiveTasks::Entity, ActiveTasks::Column::RunnerUuid)).eq(runner_uuid),
        );
        archive_stmt.and_where(
            Expr::col((ArchivedTasks::Entity, ArchivedTasks::Column::RunnerUuid)).eq(runner_uuid),
        );
    }
    if let Some(ref creator_usernames) = query.creator_usernames {
        let creator_usernames = Vec::from_iter(creator_usernames.clone());
        active_stmt.and_where(
            Expr::col((User::Entity, User::Column::Username))
                .eq(PgFunc::any(creator_usernames.clone())),
        );
        archive_stmt.and_where(
            Expr::col((User::Entity, User::Column::Username)).eq(PgFunc::any(creator_usernames)),
        );
    }
    if let Some(ref group_name) = query.group_name {
        active_stmt
            .and_where(Expr::col((Group::Entity, Group::Column::GroupName)).eq(group_name.clone()));
        archive_stmt
            .and_where(Expr::col((Group::Entity, Group::Column::GroupName)).eq(group_name.clone()));
    }
    if let Some(ref tags) = query.tags {
        let tags = Vec::from_iter(tags.clone());
        active_stmt.and_where(
            Expr::col((ActiveTasks::Entity, ActiveTasks::Column::Tags)).contains(tags.clone()),
        );
        archive_stmt.and_where(
            Expr::col((ArchivedTasks::Entity, ArchivedTasks::Column::Tags)).contains(tags),
        );
    }
    if let Some(ref labels) = query.labels {
        let labels = Vec::from_iter(labels.clone());
        active_stmt.and_where(
            Expr::col((ActiveTasks::Entity, ActiveTasks::Column::Labels)).contains(labels.clone()),
        );
        archive_stmt.and_where(
            Expr::col((ArchivedTasks::Entity, ArchivedTasks::Column::Labels)).contains(labels),
        );
    }
    if let Some(ref states) = query.states {
        let states = Vec::from_iter(states.clone());
        active_stmt.and_where(
            Expr::col((ActiveTasks::Entity, ActiveTasks::Column::State))
                .eq(PgFunc::any(states.clone())),
        );
        archive_stmt.and_where(
            Expr::col((ArchivedTasks::Entity, ArchivedTasks::Column::State))
                .eq(PgFunc::any(states)),
        );
    }
    if let Some(ref exit_status) = query.exit_status {
        let op = parse_operators_with_number(exit_status)?;
        match op {
            OperatorWithNumber::Eq(e) => {
                active_stmt.and_where(
                    Expr::col((ActiveTasks::Entity, ActiveTasks::Column::Result))
                        .cast_json_field("exit_status")
                        .eq(e.to_string()),
                );
                archive_stmt.and_where(
                    Expr::col((ArchivedTasks::Entity, ArchivedTasks::Column::Result))
                        .cast_json_field("exit_status")
                        .eq(e.to_string()),
                );
            }
            OperatorWithNumber::Neq(e) => {
                active_stmt.and_where(
                    Expr::col((ActiveTasks::Entity, ActiveTasks::Column::Result))
                        .cast_json_field("exit_status")
                        .ne(e.to_string()),
                );
                archive_stmt.and_where(
                    Expr::col((ArchivedTasks::Entity, ArchivedTasks::Column::Result))
                        .cast_json_field("exit_status")
                        .ne(e.to_string()),
                );
            }
            OperatorWithNumber::Gt(e) => {
                active_stmt.and_where(
                    Expr::col((ActiveTasks::Entity, ActiveTasks::Column::Result))
                        .cast_json_field("exit_status")
                        .gt(e.to_string()),
                );
                archive_stmt.and_where(
                    Expr::col((ArchivedTasks::Entity, ArchivedTasks::Column::Result))
                        .cast_json_field("exit_status")
                        .gt(e.to_string()),
                );
            }
            OperatorWithNumber::Gte(e) => {
                active_stmt.and_where(
                    Expr::col((ActiveTasks::Entity, ActiveTasks::Column::Result))
                        .cast_json_field("exit_status")
                        .gte(e.to_string()),
                );
                archive_stmt.and_where(
                    Expr::col((ArchivedTasks::Entity, ArchivedTasks::Column::Result))
                        .cast_json_field("exit_status")
                        .gte(e.to_string()),
                );
            }
            OperatorWithNumber::Lt(e) => {
                active_stmt.and_where(
                    Expr::col((ActiveTasks::Entity, ActiveTasks::Column::Result))
                        .cast_json_field("exit_status")
                        .lt(e.to_string()),
                );
                archive_stmt.and_where(
                    Expr::col((ArchivedTasks::Entity, ArchivedTasks::Column::Result))
                        .cast_json_field("exit_status")
                        .lt(e.to_string()),
                );
            }
            OperatorWithNumber::Lte(e) => {
                active_stmt.and_where(
                    Expr::col((ActiveTasks::Entity, ActiveTasks::Column::Result))
                        .cast_json_field("exit_status")
                        .lte(e.to_string()),
                );
                archive_stmt.and_where(
                    Expr::col((ArchivedTasks::Entity, ArchivedTasks::Column::Result))
                        .cast_json_field("exit_status")
                        .lte(e.to_string()),
                );
            }
        }
    }
    if let Some(ref priority) = query.priority {
        let op = parse_operators_with_number(priority)?;
        match op {
            OperatorWithNumber::Eq(p) => {
                active_stmt.and_where(
                    Expr::col((ActiveTasks::Entity, ActiveTasks::Column::Priority)).eq(p),
                );
                archive_stmt.and_where(
                    Expr::col((ArchivedTasks::Entity, ArchivedTasks::Column::Priority)).eq(p),
                );
            }
            OperatorWithNumber::Neq(p) => {
                active_stmt.and_where(
                    Expr::col((ActiveTasks::Entity, ActiveTasks::Column::Priority)).ne(p),
                );
                archive_stmt.and_where(
                    Expr::col((ArchivedTasks::Entity, ArchivedTasks::Column::Priority)).ne(p),
                );
            }
            OperatorWithNumber::Gt(p) => {
                active_stmt.and_where(
                    Expr::col((ActiveTasks::Entity, ActiveTasks::Column::Priority)).gt(p),
                );
                archive_stmt.and_where(
                    Expr::col((ArchivedTasks::Entity, ArchivedTasks::Column::Priority)).gt(p),
                );
            }
            OperatorWithNumber::Gte(p) => {
                active_stmt.and_where(
                    Expr::col((ActiveTasks::Entity, ActiveTasks::Column::Priority)).gte(p),
                );
                archive_stmt.and_where(
                    Expr::col((ArchivedTasks::Entity, ArchivedTasks::Column::Priority)).gte(p),
                );
            }
            OperatorWithNumber::Lt(p) => {
                active_stmt.and_where(
                    Expr::col((ActiveTasks::Entity, ActiveTasks::Column::Priority)).lt(p),
                );
                archive_stmt.and_where(
                    Expr::col((ArchivedTasks::Entity, ArchivedTasks::Column::Priority)).lt(p),
                );
            }
            OperatorWithNumber::Lte(p) => {
                active_stmt.and_where(
                    Expr::col((ActiveTasks::Entity, ActiveTasks::Column::Priority)).lte(p),
                );
                archive_stmt.and_where(
                    Expr::col((ArchivedTasks::Entity, ArchivedTasks::Column::Priority)).lte(p),
                );
            }
        }
    }
    if let Some(limit) = query.limit {
        active_stmt.limit(limit);
        archive_stmt.limit(limit);
    }
    if let Some(offset) = query.offset {
        active_stmt.offset(offset);
        archive_stmt.offset(offset);
    }
    Ok(())
}

pub async fn query_tasks_by_filter(
    user_id: i64,
    pool: &InfraPool,
    mut query: TasksQueryReq,
) -> crate::error::Result<TasksQueryResp> {
    check_task_list_query(user_id, pool, &mut query, UserGroupRole::Read).await?;
    let mut active_stmt = Query::select();
    if query.count {
        active_stmt.expr(Expr::col((ActiveTasks::Entity, ActiveTasks::Column::Uuid)).count());
    } else {
        active_stmt
            .columns([
                (ActiveTasks::Entity, ActiveTasks::Column::Uuid),
                (ActiveTasks::Entity, ActiveTasks::Column::TaskId),
                (ActiveTasks::Entity, ActiveTasks::Column::Tags),
                (ActiveTasks::Entity, ActiveTasks::Column::Labels),
                (ActiveTasks::Entity, ActiveTasks::Column::CreatedAt),
                (ActiveTasks::Entity, ActiveTasks::Column::UpdatedAt),
                (ActiveTasks::Entity, ActiveTasks::Column::State),
                (ActiveTasks::Entity, ActiveTasks::Column::Priority),
                (ActiveTasks::Entity, ActiveTasks::Column::Spec),
                (ActiveTasks::Entity, ActiveTasks::Column::ExecOptions),
                (ActiveTasks::Entity, ActiveTasks::Column::Result),
                (ActiveTasks::Entity, ActiveTasks::Column::UpstreamTaskUuid),
                (ActiveTasks::Entity, ActiveTasks::Column::DownstreamTaskUuid),
                (ActiveTasks::Entity, ActiveTasks::Column::RunnerUuid),
            ])
            .expr_as(
                Expr::col((User::Entity, User::Column::Username)),
                Alias::new("creator_username"),
            )
            .expr_as(
                Expr::col((Group::Entity, Group::Column::GroupName)),
                Alias::new("group_name"),
            );
    }
    active_stmt
        .from(ActiveTasks::Entity)
        .join(
            sea_orm::JoinType::Join,
            User::Entity,
            Expr::col((User::Entity, User::Column::Id)).eq(Expr::col((
                ActiveTasks::Entity,
                ActiveTasks::Column::CreatorId,
            ))),
        )
        .join(
            sea_orm::JoinType::Join,
            Group::Entity,
            Expr::col((ActiveTasks::Entity, ActiveTasks::Column::GroupId))
                .eq(Expr::col((Group::Entity, Group::Column::Id))),
        );
    let mut archive_stmt = Query::select();
    if query.count {
        archive_stmt.expr(Expr::col((ArchivedTasks::Entity, ArchivedTasks::Column::Uuid)).count());
    } else {
        archive_stmt
            .columns([
                (ArchivedTasks::Entity, ArchivedTasks::Column::Uuid),
                (ArchivedTasks::Entity, ArchivedTasks::Column::TaskId),
                (ArchivedTasks::Entity, ArchivedTasks::Column::Tags),
                (ArchivedTasks::Entity, ArchivedTasks::Column::Labels),
                (ArchivedTasks::Entity, ArchivedTasks::Column::CreatedAt),
                (ArchivedTasks::Entity, ArchivedTasks::Column::UpdatedAt),
                (ArchivedTasks::Entity, ArchivedTasks::Column::State),
                (ArchivedTasks::Entity, ArchivedTasks::Column::Priority),
                (ArchivedTasks::Entity, ArchivedTasks::Column::Spec),
                (ArchivedTasks::Entity, ArchivedTasks::Column::ExecOptions),
                (ArchivedTasks::Entity, ArchivedTasks::Column::Result),
                (
                    ArchivedTasks::Entity,
                    ArchivedTasks::Column::UpstreamTaskUuid,
                ),
                (
                    ArchivedTasks::Entity,
                    ArchivedTasks::Column::DownstreamTaskUuid,
                ),
                (ArchivedTasks::Entity, ArchivedTasks::Column::RunnerUuid),
            ])
            .expr_as(
                Expr::col((User::Entity, User::Column::Username)),
                Alias::new("creator_username"),
            )
            .expr_as(
                Expr::col((Group::Entity, Group::Column::GroupName)),
                Alias::new("group_name"),
            );
    }
    archive_stmt
        .from(ArchivedTasks::Entity)
        .join(
            sea_orm::JoinType::Join,
            User::Entity,
            Expr::col((User::Entity, User::Column::Id)).eq(Expr::col((
                ArchivedTasks::Entity,
                ArchivedTasks::Column::CreatorId,
            ))),
        )
        .join(
            sea_orm::JoinType::Join,
            Group::Entity,
            Expr::col((ArchivedTasks::Entity, ArchivedTasks::Column::GroupId))
                .eq(Expr::col((Group::Entity, Group::Column::Id))),
        );

    // Apply filters using the shared helper function
    apply_task_filters(&mut active_stmt, &mut archive_stmt, &query)?;
    let builder = pool.db.get_database_backend();
    let resp = if query.count {
        let active_count = CountQuery::find_by_statement(builder.build(&active_stmt))
            .one(&pool.db)
            .await?
            .map(|c| c.count)
            .unwrap_or(0) as u64;
        let archive_count = CountQuery::find_by_statement(builder.build(&archive_stmt))
            .one(&pool.db)
            .await?
            .map(|c| c.count)
            .unwrap_or(0) as u64;
        TasksQueryResp {
            count: active_count + archive_count,
            tasks: vec![],
            group_name: query.group_name.unwrap_or_default(),
        }
    } else {
        let mut active_info = TaskQueryInfo::find_by_statement(builder.build(&active_stmt))
            .all(&pool.db)
            .await?;
        let mut archive_info = TaskQueryInfo::find_by_statement(builder.build(&archive_stmt))
            .all(&pool.db)
            .await?;
        active_info.append(&mut archive_info);
        TasksQueryResp {
            count: active_info.len() as u64,
            tasks: active_info,
            group_name: query.group_name.unwrap_or_default(),
        }
    };
    Ok(resp)
}

#[derive(FromQueryResult)]
struct IdResult {
    id: i64,
    task_suite_id: Option<i64>,
}

/// Tick `incomplete_tasks` down once per cancelled task, for every suite a
/// batch cancel touched.
///
/// The rows come straight from the archive's `RETURNING`, so this counts what
/// was actually cancelled rather than what was asked for.
async fn decrement_incomplete_tasks_by_suite<C: ConnectionTrait>(
    txn: &C,
    suite_ids: impl IntoIterator<Item = Option<i64>>,
    now: TimeDateTimeWithTimeZone,
) -> crate::error::Result<()> {
    let mut per_suite: HashMap<i64, i32> = HashMap::new();
    for suite_id in suite_ids.into_iter().flatten() {
        *per_suite.entry(suite_id).or_default() += 1;
    }
    for (suite_id, count) in per_suite {
        crate::service::suite::decrement_incomplete_tasks(txn, suite_id, count, now).await?;
    }
    Ok(())
}

/// Cancel multiple tasks by filter criteria.
/// Only tasks in Ready or Pending state will be cancelled.
/// User must have Admin or Write role in the task's group (validated by check_task_list_query).
pub async fn cancel_tasks_by_filter(
    user_id: i64,
    pool: &InfraPool,
    req: TasksCancelByFilterReq,
) -> crate::error::Result<TasksCancelByFilterResp> {
    // Convert request to TasksQueryReq for validation and filtering
    let mut query = TasksQueryReq {
        creator_usernames: req.creator_usernames.clone(),
        group_name: req.group_name.clone(),
        tags: req.tags.clone(),
        labels: req.labels.clone(),
        runner_uuid: None,
        // Filter for Ready and Pending tasks (cancellable states)
        states: {
            let mut states = HashSet::new();
            // If user specified states, intersect with Ready and Pending
            if let Some(ref user_states) = req.states {
                if user_states.contains(&TaskState::Ready) {
                    states.insert(TaskState::Ready);
                }
                if user_states.contains(&TaskState::Pending) {
                    states.insert(TaskState::Pending);
                }
            }
            if states.is_empty() {
                // Default: both Ready and Pending are cancellable
                states.insert(TaskState::Ready);
                states.insert(TaskState::Pending);
            }
            Some(states)
        },
        exit_status: req.exit_status.clone(),
        priority: req.priority.clone(),
        limit: None,
        offset: None,
        count: false,
    };

    // Validate query and fill in defaults (also checks Write permission)
    let username = check_task_list_query(user_id, pool, &mut query, UserGroupRole::Write).await?;
    let group_name = query.group_name.clone().unwrap_or_default();

    // Build task ID subquery with the same filters (avoids parameter limits)
    let mut task_id_subquery = Query::select();
    task_id_subquery
        .column((ActiveTasks::Entity, ActiveTasks::Column::Id))
        .from(ActiveTasks::Entity)
        .join(
            sea_orm::JoinType::Join,
            User::Entity,
            Expr::col((User::Entity, User::Column::Id)).eq(Expr::col((
                ActiveTasks::Entity,
                ActiveTasks::Column::CreatorId,
            ))),
        )
        .join(
            sea_orm::JoinType::Join,
            Group::Entity,
            Expr::col((ActiveTasks::Entity, ActiveTasks::Column::GroupId))
                .eq(Expr::col((Group::Entity, Group::Column::Id))),
        );

    // Apply the same filters to the subquery
    let mut dummy_archive_stmt = Query::select().from(ArchivedTasks::Entity).to_owned();
    apply_task_filters(&mut task_id_subquery, &mut dummy_archive_stmt, &query)?;

    // Build the complete CTE statement before the transaction to avoid lifetime issues

    let delete_stmt = DeleteStatement::new()
        .from_table(ActiveTasks::Entity)
        .and_where(Expr::col(ActiveTasks::Column::Id).in_subquery(task_id_subquery))
        .returning_all()
        .to_owned();

    let cte = CommonTableExpression::new()
        .query(delete_stmt)
        .table_name(Alias::new("deleted"))
        .to_owned();

    // Get the database backend before the transaction
    let builder = pool.db.get_database_backend();

    // Execute delete and insert in a single transaction
    // Total: 1 query using CTE (DELETE RETURNING + INSERT SELECT) regardless of task count or column count
    let task_ids = pool
        .db
        .transaction::<_, Vec<i64>, crate::error::Error>(|txn| {
            let cte = cte.clone();
            Box::pin(async move {
                let now = TimeDateTimeWithTimeZone::now_utc();
                let res = TaskResultSpec {
                    exit_status: 0,
                    msg: Some(crate::schema::TaskResultMessage::UserCancellation),
                };
                let result = serde_json::to_value(res).inspect_err(|e| tracing::error!("{}", e))?;

                // Build SELECT from the CTE
                let select_from_cte = Query::select()
                    .expr(Expr::col(Alias::new("id")))
                    .expr(Expr::col(Alias::new("creator_id")))
                    .expr(Expr::col(Alias::new("group_id")))
                    .expr(Expr::col(Alias::new("task_id")))
                    .expr(Expr::col(Alias::new("uuid")))
                    .expr(Expr::col(Alias::new("tags")))
                    .expr(Expr::col(Alias::new("labels")))
                    .expr(Expr::col(Alias::new("created_at")))
                    .expr(Expr::value(now))
                    .expr(Expr::value(TaskState::Cancelled))
                    .expr(Expr::col(Alias::new("runner_uuid")))
                    .expr(Expr::col(Alias::new("exec_options")))
                    .expr(Expr::col(Alias::new("priority")))
                    .expr(Expr::col(Alias::new("spec")))
                    .expr(Expr::value(result.clone()))
                    .expr(Expr::col(Alias::new("upstream_task_uuid")))
                    .expr(Expr::col(Alias::new("downstream_task_uuid")))
                    .expr(Expr::col(Alias::new("task_suite_id")))
                    .from(Alias::new("deleted"))
                    .to_owned();

                // Build the INSERT SELECT statement with CTE
                let mut insert_stmt = InsertStatement::new()
                    .into_table(ArchivedTasks::Entity)
                    .columns([
                        ArchivedTasks::Column::Id,
                        ArchivedTasks::Column::CreatorId,
                        ArchivedTasks::Column::GroupId,
                        ArchivedTasks::Column::TaskId,
                        ArchivedTasks::Column::Uuid,
                        ArchivedTasks::Column::Tags,
                        ArchivedTasks::Column::Labels,
                        ArchivedTasks::Column::CreatedAt,
                        ArchivedTasks::Column::UpdatedAt,
                        ArchivedTasks::Column::State,
                        ArchivedTasks::Column::RunnerUuid,
                        ArchivedTasks::Column::ExecOptions,
                        ArchivedTasks::Column::Priority,
                        ArchivedTasks::Column::Spec,
                        ArchivedTasks::Column::Result,
                        ArchivedTasks::Column::UpstreamTaskUuid,
                        ArchivedTasks::Column::DownstreamTaskUuid,
                        // Without this the archived copy of a suite task loses
                        // its suite, and the count below has nothing to key on.
                        ArchivedTasks::Column::TaskSuiteId,
                    ])
                    .to_owned();

                insert_stmt.select_from(select_from_cte).unwrap();
                insert_stmt.returning_all();
                let insert_with_cte = insert_stmt.with(cte.into());

                let stmt = builder.build(&insert_with_cte);

                let cancelled: Vec<IdResult> = IdResult::find_by_statement(stmt).all(txn).await?;

                decrement_incomplete_tasks_by_suite(
                    txn,
                    cancelled.iter().map(|r| r.task_suite_id),
                    now,
                )
                .await?;

                let task_ids: Vec<i64> = cancelled.into_iter().map(|r| r.id).collect();

                Ok(task_ids)
            })
        })
        .await?;

    // Remove tasks from dispatch queues
    for task_id in &task_ids {
        let _ = remove_task(*task_id, pool)
            .inspect_err(|e| tracing::warn!("Failed to remove task {}: {:?}", task_id, e));
    }

    tracing::info!("User {} cancelled tasks by filter {:?}", username, req);

    Ok(TasksCancelByFilterResp {
        cancelled_count: task_ids.len() as u64,
        group_name,
    })
}

#[derive(FromQueryResult)]
struct IdUuidResult {
    id: i64,
    uuid: Uuid,
    task_suite_id: Option<i64>,
}

pub async fn cancel_tasks_by_uuids(
    user_id: i64,
    pool: &InfraPool,
    req: TasksCancelByUuidsReq,
) -> crate::error::Result<TasksCancelByUuidsResp> {
    // Validate UUIDs list is not empty
    if req.uuids.is_empty() {
        return Err(Error::ApiError(crate::error::ApiError::InvalidRequest(
            "UUIDs list cannot be empty".to_string(),
        )));
    }

    // Permission is checked per task inside the delete below, so this lookup is
    // only for the log line.
    let username = User::Entity::find()
        .filter(User::Column::Id.eq(user_id))
        .one(&pool.db)
        .await?
        .ok_or(Error::ApiError(crate::error::ApiError::NotFound(
            "User".to_string(),
        )))?
        .username;

    // Chunk UUIDs to avoid hitting the parameter limit
    let uuid_chunks: Vec<Vec<Uuid>> = req.uuids.chunks(1024).map(|chunk| chunk.to_vec()).collect();

    let mut all_task_ids = Vec::new();
    let mut all_found_uuids = HashSet::new();

    // Process each chunk
    for uuid_chunk in uuid_chunks {
        // Query all matching tasks in a single query with permission checks
        // Join with user_group to check Write permission
        let builder = pool.db.get_database_backend();

        // Build a single-column subquery that selects only task IDs with permission checks.
        let id_subquery = Query::select()
            .column((ActiveTasks::Entity, ActiveTasks::Column::Id))
            .from(ActiveTasks::Entity)
            // Join with user_group to verify user has Write permission on the group
            .join(
                sea_orm::JoinType::Join,
                UserGroup::Entity,
                sea_orm::sea_query::Expr::col((UserGroup::Entity, UserGroup::Column::GroupId)).eq(
                    sea_orm::sea_query::Expr::col((
                        ActiveTasks::Entity,
                        ActiveTasks::Column::GroupId,
                    )),
                ),
            )
            .and_where(
                sea_orm::sea_query::Expr::col((ActiveTasks::Entity, ActiveTasks::Column::Uuid))
                    .is_in(uuid_chunk),
            )
            .and_where(
                sea_orm::sea_query::Expr::col((UserGroup::Entity, UserGroup::Column::UserId))
                    .eq(user_id),
            )
            // Only allow users with Write permission or higher
            .and_where(
                sea_orm::sea_query::Expr::col((UserGroup::Entity, UserGroup::Column::Role))
                    .is_in(vec![UserGroupRole::Write, UserGroupRole::Admin]),
            )
            // Only cancel tasks in Ready or Pending states
            .and_where(
                sea_orm::sea_query::Expr::col((ActiveTasks::Entity, ActiveTasks::Column::State))
                    .is_in(vec![TaskState::Ready, TaskState::Pending]),
            )
            .to_owned();

        // Build CTE for DELETE RETURNING + INSERT SELECT to avoid parameter limits
        // Convert the SELECT statement into a subquery for the DELETE
        let delete_stmt = DeleteStatement::new()
            .from_table(ActiveTasks::Entity)
            .and_where(Expr::col(ActiveTasks::Column::Id).in_subquery(id_subquery))
            .returning_all()
            .to_owned();

        let cte = CommonTableExpression::new()
            .query(delete_stmt)
            .table_name(Alias::new("deleted"))
            .to_owned();

        // Execute delete and insert in a single transaction
        // Total: 1 query using CTE (DELETE RETURNING + INSERT SELECT) per chunk
        let (task_ids, found_uuids) = pool
            .db
            .transaction::<_, (Vec<i64>, HashSet<Uuid>), crate::error::Error>(|txn| {
                let cte = cte.clone();
                Box::pin(async move {
                    let now = TimeDateTimeWithTimeZone::now_utc();
                    let res = TaskResultSpec {
                        exit_status: 0,
                        msg: Some(crate::schema::TaskResultMessage::UserCancellation),
                    };
                    let result =
                        serde_json::to_value(res).inspect_err(|e| tracing::error!("{}", e))?;

                    // Build SELECT from the CTE
                    let select_from_cte = Query::select()
                        .expr(Expr::col(Alias::new("id")))
                        .expr(Expr::col(Alias::new("creator_id")))
                        .expr(Expr::col(Alias::new("group_id")))
                        .expr(Expr::col(Alias::new("task_id")))
                        .expr(Expr::col(Alias::new("uuid")))
                        .expr(Expr::col(Alias::new("tags")))
                        .expr(Expr::col(Alias::new("labels")))
                        .expr(Expr::col(Alias::new("created_at")))
                        .expr(Expr::value(now))
                        .expr(Expr::value(TaskState::Cancelled))
                        .expr(Expr::col(Alias::new("runner_uuid")))
                        .expr(Expr::col(Alias::new("exec_options")))
                        .expr(Expr::col(Alias::new("priority")))
                        .expr(Expr::col(Alias::new("spec")))
                        .expr(Expr::value(result.clone()))
                        .expr(Expr::col(Alias::new("upstream_task_uuid")))
                        .expr(Expr::col(Alias::new("downstream_task_uuid")))
                        .expr(Expr::col(Alias::new("task_suite_id")))
                        .from(Alias::new("deleted"))
                        .to_owned();

                    // Build the INSERT SELECT statement with CTE
                    let mut insert_stmt = InsertStatement::new()
                        .into_table(ArchivedTasks::Entity)
                        .columns([
                            ArchivedTasks::Column::Id,
                            ArchivedTasks::Column::CreatorId,
                            ArchivedTasks::Column::GroupId,
                            ArchivedTasks::Column::TaskId,
                            ArchivedTasks::Column::Uuid,
                            ArchivedTasks::Column::Tags,
                            ArchivedTasks::Column::Labels,
                            ArchivedTasks::Column::CreatedAt,
                            ArchivedTasks::Column::UpdatedAt,
                            ArchivedTasks::Column::State,
                            ArchivedTasks::Column::RunnerUuid,
                            ArchivedTasks::Column::ExecOptions,
                            ArchivedTasks::Column::Priority,
                            ArchivedTasks::Column::Spec,
                            ArchivedTasks::Column::Result,
                            ArchivedTasks::Column::UpstreamTaskUuid,
                            ArchivedTasks::Column::DownstreamTaskUuid,
                            // Without this the archived copy of a suite task
                            // loses its suite, and the count below has nothing
                            // to key on.
                            ArchivedTasks::Column::TaskSuiteId,
                        ])
                        .to_owned();

                    insert_stmt.select_from(select_from_cte).unwrap();
                    insert_stmt.returning_all();
                    let insert_with_cte = insert_stmt.with(cte.into());

                    let stmt = builder.build(&insert_with_cte);

                    let results: Vec<IdUuidResult> =
                        IdUuidResult::find_by_statement(stmt).all(txn).await?;

                    decrement_incomplete_tasks_by_suite(
                        txn,
                        results.iter().map(|r| r.task_suite_id),
                        now,
                    )
                    .await?;

                    let task_ids: Vec<i64> = results.iter().map(|r| r.id).collect();
                    let found_uuids: HashSet<Uuid> = results.into_iter().map(|r| r.uuid).collect();

                    Ok((task_ids, found_uuids))
                })
            })
            .await?;

        // Accumulate results from this chunk
        all_task_ids.extend(task_ids);
        all_found_uuids.extend(found_uuids);
    }

    // Remove tasks from dispatch queues
    for task_id in &all_task_ids {
        let _ = remove_task(*task_id, pool)
            .inspect_err(|e| tracing::warn!("Failed to remove task {}: {:?}", task_id, e));
    }

    tracing::info!("User {} cancelled tasks by uuids {:?}", username, req);

    // Determine which UUIDs failed (not found or no permission)
    let failed_uuids: Vec<Uuid> = req
        .uuids
        .into_iter()
        .filter(|uuid| !all_found_uuids.contains(uuid))
        .collect();

    Ok(TasksCancelByUuidsResp {
        cancelled_count: all_task_ids.len() as u64,
        failed_uuids,
    })
}

#[derive(Debug, Clone, FromQueryResult)]
pub(crate) struct PartialWorkerId {
    pub(crate) id: i64,
}

impl From<PartialWorkerId> for i64 {
    fn from(p: PartialWorkerId) -> Self {
        p.id
    }
}

/// Batch submit multiple tasks.
/// Submits each task in the list and returns individual results for each (including failures).
pub async fn user_batch_submit_tasks(
    pool: &InfraPool,
    user_id: i64,
    req: crate::schema::TasksSubmitReq,
) -> crate::error::Result<crate::schema::TasksSubmitResp> {
    if req.tasks.is_empty() {
        return Err(Error::ApiError(crate::error::ApiError::InvalidRequest(
            "Tasks list cannot be empty".to_string(),
        )));
    }

    let mut results = Vec::with_capacity(req.tasks.len());

    for task_req in req.tasks {
        let result = user_submit_task(pool, user_id, task_req)
            .await
            .map_err(|e| match e {
                crate::error::Error::AuthError(err) => ApiError::AuthError(err),
                crate::error::Error::ApiError(e) => e,
                _ => {
                    tracing::error!("{}", e);
                    ApiError::InternalServerError
                }
            })
            .map_err(ErrorMsg::from);
        results.push(result);
    }

    Ok(crate::schema::TasksSubmitResp { results })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::sea_query::PostgresQueryBuilder;
    use sea_orm::QueryTrait;

    fn accept_sql() -> String {
        accept_task_into_suite_query(7, TimeDateTimeWithTimeZone::now_utc())
            .into_query()
            .to_string(PostgresQueryBuilder)
    }

    #[test]
    fn accepting_a_task_moves_both_counters_by_column_expression() {
        let sql = accept_sql();
        for column in ["total_tasks", "incomplete_tasks"] {
            assert!(
                sql.contains(&format!(r#""{column}" = "{column}" + 1"#)),
                "{column} must be incremented in SQL: {sql}"
            );
        }
    }

    #[test]
    fn accepting_a_task_never_reopens_a_cancelled_suite() {
        use sea_orm::ActiveEnum;
        let sql = accept_sql();
        let cancelled = TaskSuiteState::Cancelled.into_value().to_string();
        assert!(
            sql.contains(&format!(r#""state" <> {cancelled}"#)),
            "a cancelled suite must not match: {sql}"
        );
    }
}
