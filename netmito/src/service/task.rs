use sea_orm::sea_query::extension::postgres::PgExpr;
use sea_orm::sea_query::{Alias, ExprTrait, PgFunc, Query};
use sea_orm::{prelude::*, FromQueryResult, Set, TransactionTrait};
use uuid::Uuid;

use crate::entity::role::{GroupWorkerRole, UserGroupRole};
use crate::entity::state::TaskState;
use crate::schema::{
    ArtifactQueryResp, ChangeTaskReq, CountQuery, ParsedTaskQueryInfo, SubmitTaskReq,
    SubmitTaskResp, TaskQueryInfo, TaskQueryResp, TaskResultSpec, TasksQueryReq, TasksQueryResp,
    UpdateTaskLabelsReq,
};
use crate::{config::InfraPool, schema::TaskSpec};

use crate::{
    entity::{
        active_tasks as ActiveTask, archived_tasks as ArchivedTasks, artifacts as Artifact,
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
    let task = ActiveTask::ActiveModel {
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
        .column((Worker::Entity, ActiveTask::Column::Id))
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
        .transaction::<_, ActiveTask::Model, crate::error::Error>(|txn| {
            Box::pin(async move {
                let task = ActiveTask::Entity::find()
                    .filter(ActiveTask::Column::Uuid.eq(uuid))
                    .filter(ActiveTask::Column::State.eq(TaskState::Ready))
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
                let mut task: ActiveTask::ActiveModel = task.into();
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
        .column((Worker::Entity, ActiveTask::Column::Id))
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
                let task = ActiveTask::Entity::find()
                    .filter(ActiveTask::Column::Uuid.eq(uuid))
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
                    let mut task: ActiveTask::ActiveModel = task.into();
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
                let task = ActiveTask::Entity::find()
                    .filter(ActiveTask::Column::Uuid.eq(uuid))
                    .filter(ActiveTask::Column::State.eq(TaskState::Ready))
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
                };
                archived_task.insert(txn).await?;
                ActiveTask::Entity::delete_by_id(task.id).exec(txn).await?;
                Ok(task.id)
            })
        })
        .await?;
    let _ = remove_task(task_id, pool)
        .inspect_err(|e| tracing::warn!("Failed to remove task {}: {:?}", task_id, e));
    Ok(())
}

pub async fn get_task(pool: &InfraPool, uuid: Uuid) -> crate::error::Result<TaskQueryResp> {
    let active_task_stmt = Query::select()
        .columns([
            (ActiveTask::Entity, ActiveTask::Column::Uuid),
            (ActiveTask::Entity, ActiveTask::Column::TaskId),
            (ActiveTask::Entity, ActiveTask::Column::Tags),
            (ActiveTask::Entity, ActiveTask::Column::Labels),
            (ActiveTask::Entity, ActiveTask::Column::CreatedAt),
            (ActiveTask::Entity, ActiveTask::Column::UpdatedAt),
            (ActiveTask::Entity, ActiveTask::Column::State),
            (ActiveTask::Entity, ActiveTask::Column::Timeout),
            (ActiveTask::Entity, ActiveTask::Column::Priority),
            (ActiveTask::Entity, ActiveTask::Column::Spec),
            (ActiveTask::Entity, ActiveTask::Column::Result),
        ])
        .expr_as(
            Expr::col((User::Entity, User::Column::Username)),
            Alias::new("creator_username"),
        )
        .expr_as(
            Expr::col((Group::Entity, Group::Column::GroupName)),
            Alias::new("group_name"),
        )
        .from(ActiveTask::Entity)
        .join(
            sea_orm::JoinType::Join,
            User::Entity,
            Expr::col((User::Entity, User::Column::Id)).eq(Expr::col((
                ActiveTask::Entity,
                ActiveTask::Column::CreatorId,
            ))),
        )
        .join(
            sea_orm::JoinType::Join,
            Group::Entity,
            Expr::col((ActiveTask::Entity, ActiveTask::Column::GroupId))
                .eq(Expr::col((Group::Entity, Group::Column::Id))),
        )
        .and_where(Expr::col((ActiveTask::Entity, ActiveTask::Column::Uuid)).eq(uuid))
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
    match query.group_name {
        Some(ref group_name) => {
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
                .and_where(
                    Expr::col((Group::Entity, Group::Column::GroupName)).eq(group_name.clone()),
                )
                .to_owned();
            let role = UserGroupRoleQueryRes::find_by_statement(builder.build(&role_stmt))
                .one(&pool.db)
                .await?
                .map(|r| r.role);
            if role.is_none() {
                return Err(Error::ApiError(crate::error::ApiError::InvalidRequest(
                    format!("Group with name {group_name} not found or user is not in the group"),
                )));
            }
        }
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
            query.group_name = Some(username);
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

pub async fn query_task_list(
    user_id: i64,
    pool: &InfraPool,
    mut query: TasksQueryReq,
) -> crate::error::Result<TasksQueryResp> {
    check_task_list_query(user_id, pool, &mut query).await?;
    let mut active_stmt = Query::select();
    if query.count {
        active_stmt.expr(Expr::col((ActiveTask::Entity, ActiveTask::Column::Uuid)).count());
    } else {
        active_stmt
            .columns([
                (ActiveTask::Entity, ActiveTask::Column::Uuid),
                (ActiveTask::Entity, ActiveTask::Column::TaskId),
                (ActiveTask::Entity, ActiveTask::Column::Tags),
                (ActiveTask::Entity, ActiveTask::Column::Labels),
                (ActiveTask::Entity, ActiveTask::Column::CreatedAt),
                (ActiveTask::Entity, ActiveTask::Column::UpdatedAt),
                (ActiveTask::Entity, ActiveTask::Column::State),
                (ActiveTask::Entity, ActiveTask::Column::Timeout),
                (ActiveTask::Entity, ActiveTask::Column::Priority),
                (ActiveTask::Entity, ActiveTask::Column::Spec),
                (ActiveTask::Entity, ActiveTask::Column::Result),
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
        .from(ActiveTask::Entity)
        .join(
            sea_orm::JoinType::Join,
            User::Entity,
            Expr::col((User::Entity, User::Column::Id)).eq(Expr::col((
                ActiveTask::Entity,
                ActiveTask::Column::CreatorId,
            ))),
        )
        .join(
            sea_orm::JoinType::Join,
            Group::Entity,
            Expr::col((ActiveTask::Entity, ActiveTask::Column::GroupId))
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
    if let Some(creator_usernames) = query.creator_usernames {
        let creator_usernames = Vec::from_iter(creator_usernames);
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
    if let Some(tags) = query.tags {
        let tags = Vec::from_iter(tags);
        active_stmt.and_where(
            Expr::col((ActiveTask::Entity, ActiveTask::Column::Tags)).contains(tags.clone()),
        );
        archive_stmt.and_where(
            Expr::col((ArchivedTasks::Entity, ArchivedTasks::Column::Tags)).contains(tags),
        );
    }
    if let Some(labels) = query.labels {
        let labels = Vec::from_iter(labels);
        active_stmt.and_where(
            Expr::col((ActiveTask::Entity, ActiveTask::Column::Labels)).contains(labels.clone()),
        );
        archive_stmt.and_where(
            Expr::col((ArchivedTasks::Entity, ArchivedTasks::Column::Labels)).contains(labels),
        );
    }
    if let Some(states) = query.states {
        let states = Vec::from_iter(states);
        active_stmt.and_where(
            Expr::col((ActiveTask::Entity, ActiveTask::Column::State))
                .eq(PgFunc::any(states.clone())),
        );
        archive_stmt.and_where(
            Expr::col((ArchivedTasks::Entity, ArchivedTasks::Column::State))
                .eq(PgFunc::any(states)),
        );
    }
    if let Some(exit_status) = query.exit_status {
        let op = parse_operators_with_number(&exit_status)?;
        match op {
            OperatorWithNumber::Eq(e) => {
                active_stmt.and_where(
                    Expr::col((ActiveTask::Entity, ActiveTask::Column::Result))
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
                    Expr::col((ActiveTask::Entity, ActiveTask::Column::Result))
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
                    Expr::col((ActiveTask::Entity, ActiveTask::Column::Result))
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
                    Expr::col((ActiveTask::Entity, ActiveTask::Column::Result))
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
                    Expr::col((ActiveTask::Entity, ActiveTask::Column::Result))
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
                    Expr::col((ActiveTask::Entity, ActiveTask::Column::Result))
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
    if let Some(priority) = query.priority {
        let op = parse_operators_with_number(&priority)?;
        match op {
            OperatorWithNumber::Eq(p) => {
                active_stmt
                    .and_where(Expr::col((ActiveTask::Entity, ActiveTask::Column::Priority)).eq(p));
                archive_stmt.and_where(
                    Expr::col((ArchivedTasks::Entity, ArchivedTasks::Column::Priority)).eq(p),
                );
            }
            OperatorWithNumber::Neq(p) => {
                active_stmt
                    .and_where(Expr::col((ActiveTask::Entity, ActiveTask::Column::Priority)).ne(p));
                archive_stmt.and_where(
                    Expr::col((ArchivedTasks::Entity, ArchivedTasks::Column::Priority)).ne(p),
                );
            }
            OperatorWithNumber::Gt(p) => {
                active_stmt
                    .and_where(Expr::col((ActiveTask::Entity, ActiveTask::Column::Priority)).gt(p));
                archive_stmt.and_where(
                    Expr::col((ArchivedTasks::Entity, ArchivedTasks::Column::Priority)).gt(p),
                );
            }
            OperatorWithNumber::Gte(p) => {
                active_stmt.and_where(
                    Expr::col((ActiveTask::Entity, ActiveTask::Column::Priority)).gte(p),
                );
                archive_stmt.and_where(
                    Expr::col((ArchivedTasks::Entity, ArchivedTasks::Column::Priority)).gte(p),
                );
            }
            OperatorWithNumber::Lt(p) => {
                active_stmt
                    .and_where(Expr::col((ActiveTask::Entity, ActiveTask::Column::Priority)).lt(p));
                archive_stmt.and_where(
                    Expr::col((ArchivedTasks::Entity, ArchivedTasks::Column::Priority)).lt(p),
                );
            }
            OperatorWithNumber::Lte(p) => {
                active_stmt.and_where(
                    Expr::col((ActiveTask::Entity, ActiveTask::Column::Priority)).lte(p),
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

#[derive(Debug, Clone, FromQueryResult)]
pub(crate) struct PartialWorkerId {
    pub(crate) id: i64,
}

impl From<PartialWorkerId> for i64 {
    fn from(p: PartialWorkerId) -> Self {
        p.id
    }
}
