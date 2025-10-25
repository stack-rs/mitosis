use sea_orm::sea_query::extension::postgres::PgExpr;
use sea_orm::sea_query::{Alias, ExprTrait, PgFunc, Query};
use sea_orm::{prelude::*, FromQueryResult, Set, TransactionTrait};
use std::collections::HashSet;
use uuid::Uuid;

use crate::entity::role::{GroupWorkerRole, UserGroupRole};
use crate::entity::state::TaskState;
use crate::schema::{
    ArtifactQueryResp, ChangeTaskReq, CountQuery, ParsedTaskQueryInfo, SubmitTaskReq,
    SubmitTaskResp, TaskQueryInfo, TaskQueryResp, TaskResultSpec, TasksCancelByFilterReq,
    TasksCancelByFilterResp, TasksQueryReq, TasksQueryResp, UpdateTaskLabelsReq,
};
use crate::{config::InfraPool, schema::TaskSpec};

use crate::{
    entity::{
        active_tasks as ActiveTasks, archived_tasks as ArchivedTasks, artifacts as Artifact,
        group_worker as GroupWorker, groups as Group, user_group as UserGroup, users as User,
        workers as Worker,
    },
    error::Error,
};

use super::worker::{remove_task, TaskDispatcherOp};

// XXX: Not sure if we can relax the constrains on local path checking.
// We currently only check if the path is absolute or contains `..` and not check for `.`.
fn check_task_spec(spec: &TaskSpec) -> crate::error::Result<()> {
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

pub async fn user_submit_task(
    pool: &InfraPool,
    creator_id: i64,
    SubmitTaskReq {
        group_name,
        tags,
        labels,
        timeout,
        priority,
        task_spec,
    }: SubmitTaskReq,
) -> crate::error::Result<SubmitTaskResp> {
    let tags = Vec::from_iter(tags);
    let labels = Vec::from_iter(labels);
    let now = TimeDateTimeWithTimeZone::now_utc();
    let group = Group::Entity::update_many()
        .col_expr(
            Group::Column::TaskCount,
            Expr::col(Group::Column::TaskCount).add(1),
        )
        .col_expr(Group::Column::UpdatedAt, Expr::value(now))
        .filter(Group::Column::GroupName.eq(&group_name))
        .exec_with_returning(&pool.db)
        .await?;
    if group.is_empty() {
        return Err(Error::ApiError(crate::error::ApiError::NotFound(format!(
            "User or group with name {group_name}"
        ))));
    }
    let group = group.into_iter().next().unwrap();
    let group_id = group.id;
    let task_id = group.task_count;
    let uuid = Uuid::new_v4();
    check_task_spec(&task_spec)?;
    let spec = serde_json::to_value(task_spec)?;
    let task = ActiveTasks::ActiveModel {
        creator_id: Set(creator_id),
        group_id: Set(group_id),
        task_id: Set(task_id),
        uuid: Set(uuid),
        tags: Set(tags),
        labels: Set(labels),
        created_at: Set(now),
        updated_at: Set(now),
        state: Set(crate::entity::state::TaskState::Ready),
        assigned_worker: Set(None),
        timeout: Set(timeout.as_secs() as i64),
        priority: Set(priority),
        spec: Set(spec),
        result: Set(None),
        ..Default::default()
    };
    let task = task.insert(&pool.db).await?;
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
        Ok(SubmitTaskResp {
            task_id: task.task_id,
            uuid: task.uuid,
        })
    }
}

pub async fn worker_submit_pending_task(
    pool: &InfraPool,
    creator_id: i64,
    upstream_task_uuid: Uuid,
    SubmitTaskReq {
        group_name,
        tags,
        labels,
        timeout,
        priority,
        task_spec,
    }: SubmitTaskReq,
) -> crate::error::Result<SubmitTaskResp> {
    let tags = Vec::from_iter(tags);
    let labels = Vec::from_iter(labels);
    let now = TimeDateTimeWithTimeZone::now_utc();
    let group = Group::Entity::update_many()
        .col_expr(
            Group::Column::TaskCount,
            Expr::col(Group::Column::TaskCount).add(1),
        )
        .col_expr(Group::Column::UpdatedAt, Expr::value(now))
        .filter(Group::Column::GroupName.eq(&group_name))
        .exec_with_returning(&pool.db)
        .await?;
    if group.is_empty() {
        return Err(Error::ApiError(crate::error::ApiError::NotFound(format!(
            "User or group with name {group_name}"
        ))));
    }
    let group = group.into_iter().next().unwrap();
    let group_id = group.id;
    let task_id = group.task_count;
    let uuid = Uuid::new_v4();
    check_task_spec(&task_spec)?;
    let spec = serde_json::to_value(task_spec)?;
    let task = ActiveTasks::ActiveModel {
        creator_id: Set(creator_id),
        group_id: Set(group_id),
        task_id: Set(task_id),
        uuid: Set(uuid),
        tags: Set(tags),
        labels: Set(labels),
        created_at: Set(now),
        updated_at: Set(now),
        state: Set(crate::entity::state::TaskState::Pending),
        assigned_worker: Set(None),
        timeout: Set(timeout.as_secs() as i64),
        priority: Set(priority),
        spec: Set(spec),
        result: Set(None),
        upstream_task_uuid: Set(Some(upstream_task_uuid)),
        ..Default::default()
    };
    let task = task.insert(&pool.db).await?;
    Ok(SubmitTaskResp {
        task_id: task.task_id,
        uuid: task.uuid,
    })
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
        timeout,
        priority,
        task_spec,
    }: ChangeTaskReq,
) -> crate::error::Result<()> {
    if tags.is_none() && timeout.is_none() && priority.is_none() && task_spec.is_none() {
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
                if let Some(task_spec) = task_spec {
                    check_task_spec(&task_spec)?;
                    let spec = serde_json::to_value(task_spec)?;
                    task.spec = Set(spec);
                }
                if let Some(timeout) = timeout {
                    task.timeout = Set(timeout.as_secs() as i64);
                }
                if let Some(priority) = priority {
                    task.priority = Set(priority);
                }
                let task = task.update(txn).await?;

                Ok(task)
            })
        })
        .await?;
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

pub async fn user_cancel_task(
    pool: &InfraPool,
    user_id: i64,
    uuid: Uuid,
) -> crate::error::Result<()> {
    let task_id = pool
        .db
        .transaction::<_, i64, crate::error::Error>(|txn| {
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
                let now = TimeDateTimeWithTimeZone::now_utc();
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
                    assigned_worker: Set(task.assigned_worker),
                    timeout: Set(task.timeout),
                    priority: Set(task.priority),
                    spec: Set(task.spec),
                    result: Set(Some(result)),
                    upstream_task_uuid: Set(task.upstream_task_uuid),
                    downstream_task_uuid: Set(task.downstream_task_uuid),
                };
                archived_task.insert(txn).await?;
                ActiveTasks::Entity::delete_by_id(task.id).exec(txn).await?;
                Ok(task.id)
            })
        })
        .await?;
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
            (ActiveTasks::Entity, ActiveTasks::Column::Timeout),
            (ActiveTasks::Entity, ActiveTasks::Column::Priority),
            (ActiveTasks::Entity, ActiveTasks::Column::Spec),
            (ActiveTasks::Entity, ActiveTasks::Column::Result),
            (ActiveTasks::Entity, ActiveTasks::Column::UpstreamTaskUuid),
            (ActiveTasks::Entity, ActiveTasks::Column::DownstreamTaskUuid),
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
            (ArchivedTasks::Entity, ArchivedTasks::Column::Timeout),
            (ArchivedTasks::Entity, ArchivedTasks::Column::Priority),
            (ArchivedTasks::Entity, ArchivedTasks::Column::Spec),
            (ArchivedTasks::Entity, ArchivedTasks::Column::Result),
            (
                ArchivedTasks::Entity,
                ArchivedTasks::Column::UpstreamTaskUuid,
            ),
            (
                ArchivedTasks::Entity,
                ArchivedTasks::Column::DownstreamTaskUuid,
            ),
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
        timeout: info.timeout,
        priority: info.priority,
        spec: serde_json::from_value(info.spec)?,
        result: info.result.map(serde_json::from_value).transpose()?,
        upstream_task_uuid: info.upstream_task_uuid,
        downstream_task_uuid: info.downstream_task_uuid,
    };
    Ok(TaskQueryResp { info, artifacts })
}

#[derive(FromQueryResult)]
struct UserGroupRoleQueryRes {
    role: UserGroupRole,
}

async fn check_task_list_query(
    user_id: i64,
    pool: &InfraPool,
    query: &mut TasksQueryReq,
    role: UserGroupRole,
) -> crate::error::Result<()> {
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
    if query.group_name.is_none() {
        let username = User::Entity::find()
            .filter(User::Column::Id.eq(user_id))
            .one(&pool.db)
            .await?
            .ok_or(Error::ApiError(crate::error::ApiError::NotFound(
                "User".to_string(),
            )))?
            .username;
        tracing::debug!("No group name specified, use username {} instead", username);
        query.group_name = Some(username);
    }
    if let Some(ref group_name) = query.group_name {
        let builder = pool.db.get_database_backend();
        let role_stmt = Query::select()
            .column((UserGroup::Entity, UserGroup::Column::Role))
            .from(UserGroup::Entity)
            .join(
                sea_orm::JoinType::Join,
                Group::Entity,
                Expr::col((Group::Entity, Group::Column::Id))
                    .eq(Expr::col((UserGroup::Entity, UserGroup::Column::GroupId))),
            )
            .and_where(Expr::col((UserGroup::Entity, UserGroup::Column::UserId)).eq(user_id))
            .and_where(Expr::col((Group::Entity, Group::Column::GroupName)).eq(group_name.clone()))
            .to_owned();
        let query_role = UserGroupRoleQueryRes::find_by_statement(builder.build(&role_stmt))
            .one(&pool.db)
            .await?
            .map(|r| r.role);
        match query_role {
            Some(r) if r >= role => {}
            Some(_) => {
                return Err(Error::AuthError(crate::error::AuthError::PermissionDenied));
            }
            None => {
                return Err(Error::ApiError(crate::error::ApiError::InvalidRequest(
                    format!("Group with name {group_name} not found or user is not in the group"),
                )));
            }
        }
    }
    Ok(())
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
fn apply_task_filters(
    active_stmt: &mut sea_orm::sea_query::SelectStatement,
    archive_stmt: &mut sea_orm::sea_query::SelectStatement,
    query: &TasksQueryReq,
) -> crate::error::Result<()> {
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
                (ActiveTasks::Entity, ActiveTasks::Column::Timeout),
                (ActiveTasks::Entity, ActiveTasks::Column::Priority),
                (ActiveTasks::Entity, ActiveTasks::Column::Spec),
                (ActiveTasks::Entity, ActiveTasks::Column::Result),
                (ActiveTasks::Entity, ActiveTasks::Column::UpstreamTaskUuid),
                (ActiveTasks::Entity, ActiveTasks::Column::DownstreamTaskUuid),
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
                (ArchivedTasks::Entity, ArchivedTasks::Column::Timeout),
                (ArchivedTasks::Entity, ArchivedTasks::Column::Priority),
                (ArchivedTasks::Entity, ArchivedTasks::Column::Spec),
                (ArchivedTasks::Entity, ArchivedTasks::Column::Result),
                (
                    ArchivedTasks::Entity,
                    ArchivedTasks::Column::UpstreamTaskUuid,
                ),
                (
                    ArchivedTasks::Entity,
                    ArchivedTasks::Column::DownstreamTaskUuid,
                ),
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
        creator_usernames: req.creator_usernames,
        group_name: req.group_name,
        tags: req.tags,
        labels: req.labels,
        // Filter for Ready and Pending tasks (cancellable states)
        states: {
            let mut states = HashSet::new();
            // If user specified states, intersect with Ready and Pending
            if let Some(user_states) = req.states {
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
        exit_status: req.exit_status,
        priority: req.priority,
        limit: None,
        offset: None,
        count: false,
    };

    // Validate query and fill in defaults (also checks Write permission)
    check_task_list_query(user_id, pool, &mut query, UserGroupRole::Write).await?;
    let group_name = query.group_name.clone().unwrap_or_default();

    // Build query statement for matching tasks
    let mut active_stmt = Query::select();
    active_stmt
        .columns([
            (ActiveTasks::Entity, ActiveTasks::Column::Id),
            (ActiveTasks::Entity, ActiveTasks::Column::Uuid),
            (ActiveTasks::Entity, ActiveTasks::Column::TaskId),
            (ActiveTasks::Entity, ActiveTasks::Column::CreatorId),
            (ActiveTasks::Entity, ActiveTasks::Column::GroupId),
            (ActiveTasks::Entity, ActiveTasks::Column::Tags),
            (ActiveTasks::Entity, ActiveTasks::Column::Labels),
            (ActiveTasks::Entity, ActiveTasks::Column::CreatedAt),
            (ActiveTasks::Entity, ActiveTasks::Column::UpdatedAt),
            (ActiveTasks::Entity, ActiveTasks::Column::State),
            (ActiveTasks::Entity, ActiveTasks::Column::AssignedWorker),
            (ActiveTasks::Entity, ActiveTasks::Column::Timeout),
            (ActiveTasks::Entity, ActiveTasks::Column::Priority),
            (ActiveTasks::Entity, ActiveTasks::Column::Spec),
            (ActiveTasks::Entity, ActiveTasks::Column::UpstreamTaskUuid),
            (ActiveTasks::Entity, ActiveTasks::Column::DownstreamTaskUuid),
        ])
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

    // Apply filters (note: we don't need archive_stmt for cancellation)
    let mut archive_stmt = Query::select().from(ArchivedTasks::Entity).to_owned();
    apply_task_filters(&mut active_stmt, &mut archive_stmt, &query)?;

    let builder = pool.db.get_database_backend();
    let stmt = builder.build(&active_stmt);

    // Execute query, delete, and insert in a single transaction
    // Total: 3 queries (SELECT + DELETE + INSERT_MANY) regardless of task count
    let task_ids = pool
        .db
        .transaction::<_, Vec<i64>, crate::error::Error>(|txn| {
            Box::pin(async move {
                // Query matching tasks within transaction (Query #1)
                let tasks = ActiveTasks::Model::find_by_statement(stmt.clone())
                    .all(txn)
                    .await?;

                if tasks.is_empty() {
                    return Ok(vec![]);
                }

                let now = TimeDateTimeWithTimeZone::now_utc();
                let res = TaskResultSpec {
                    exit_status: 0,
                    msg: Some(crate::schema::TaskResultMessage::UserCancellation),
                };
                let result = serde_json::to_value(res).inspect_err(|e| tracing::error!("{}", e))?;

                // Collect task IDs for deletion
                let task_ids: Vec<i64> = tasks.iter().map(|t| t.id).collect();

                // Prepare archived tasks
                let archived_tasks: Vec<ArchivedTasks::ActiveModel> = tasks
                    .into_iter()
                    .map(|task| ArchivedTasks::ActiveModel {
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
                        assigned_worker: Set(task.assigned_worker),
                        timeout: Set(task.timeout),
                        priority: Set(task.priority),
                        spec: Set(task.spec),
                        result: Set(Some(result.clone())),
                        upstream_task_uuid: Set(task.upstream_task_uuid),
                        downstream_task_uuid: Set(task.downstream_task_uuid),
                    })
                    .collect();

                // Batch delete from active_tasks (Query #2)
                ActiveTasks::Entity::delete_many()
                    .filter(ActiveTasks::Column::Id.is_in(task_ids.clone()))
                    .exec(txn)
                    .await?;

                // Batch insert into archived_tasks (Query #3)
                ArchivedTasks::Entity::insert_many(archived_tasks)
                    .exec(txn)
                    .await?;

                Ok(task_ids)
            })
        })
        .await?;

    // Remove tasks from dispatch queues
    for task_id in &task_ids {
        let _ = remove_task(*task_id, pool)
            .inspect_err(|e| tracing::warn!("Failed to remove task {}: {:?}", task_id, e));
    }

    Ok(TasksCancelByFilterResp {
        cancelled_count: task_ids.len() as u64,
        group_name,
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
