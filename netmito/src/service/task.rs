use sea_orm::sea_query::extension::postgres::PgExpr;
use sea_orm::sea_query::{Alias, Query};
use sea_orm::{prelude::*, FromQueryResult, Set};
use uuid::Uuid;

use crate::schema::{
    ArtifactQueryResp, ParsedTaskQueryInfo, SubmitTaskReq, SubmitTaskResp, TaskQueryInfo,
    TaskQueryResp, TasksQueryReq,
};
use crate::{config::InfraPool, schema::TaskSpec};

use crate::{
    entity::{
        active_tasks as ActiveTask, archived_tasks as ArchivedTasks, artifacts as Artifact,
        group_worker as GroupWorker, groups as Group, users as User, workers as Worker,
    },
    error::Error,
};

use super::worker::TaskDispatcherOp;

fn check_task_spec(spec: &TaskSpec) -> crate::error::Result<()> {
    if spec.resources.iter().any(|r| {
        r.local_path.is_absolute()
            || r.local_path.components().any(|c| {
                matches!(c, std::path::Component::ParentDir)
                    || matches!(c, std::path::Component::CurDir)
            })
    }) {
        return Err(Error::ApiError(crate::error::ApiError::InvalidRequest(
            "Resource local path is absolute or contains `..` or `.`".to_string(),
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
            "User or group with name {}",
            group_name
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
        .and_where(Expr::col((GroupWorker::Entity, GroupWorker::Column::Role)).gt(0))
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
        "Task with uuid {}",
        uuid
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

pub async fn query_task_list(
    pool: &InfraPool,
    query: TasksQueryReq,
) -> crate::error::Result<Vec<TaskQueryInfo>> {
    let mut active_stmt = Query::select();
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
        );
    let mut archive_stmt = Query::select();
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
        );
    if let Some(creator_username) = query.creator_username {
        active_stmt.and_where(
            Expr::col((User::Entity, User::Column::Username)).eq(creator_username.clone()),
        );
        archive_stmt
            .and_where(Expr::col((User::Entity, User::Column::Username)).eq(creator_username));
    }
    if let Some(group_name) = query.group_name {
        active_stmt
            .and_where(Expr::col((Group::Entity, Group::Column::GroupName)).eq(group_name.clone()));
        archive_stmt.and_where(Expr::col((Group::Entity, Group::Column::GroupName)).eq(group_name));
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
    if let Some(state) = query.state {
        active_stmt.and_where(
            Expr::col((ActiveTask::Entity, ActiveTask::Column::State)).eq(state),
        );
        archive_stmt
            .and_where(Expr::col((ArchivedTasks::Entity, ArchivedTasks::Column::State)).eq(state));
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
    let mut active_info = TaskQueryInfo::find_by_statement(builder.build(&active_stmt))
        .all(&pool.db)
        .await?;
    let mut archive_info = TaskQueryInfo::find_by_statement(builder.build(&archive_stmt))
        .all(&pool.db)
        .await?;
    active_info.append(&mut archive_info);
    Ok(active_info)
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
