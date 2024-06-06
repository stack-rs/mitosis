use sea_orm::sea_query::extension::postgres::PgExpr;
use sea_orm::sea_query::Query;
use sea_orm::{prelude::*, FromQueryResult, Set};
use uuid::Uuid;

use crate::schema::{ArtifactQueryResp, TaskQueryResp};
use crate::{config::InfraPool, schema::TaskSpec};

use crate::{
    entity::{
        active_tasks as ActiveTask, archived_tasks as ArchivedTasks, artifacts as Artifact,
        group_worker as GroupWorker, groups as Group, users as User, workers as Worker,
    },
    error::Error,
};

use super::worker::TaskDispatcherOp;

pub async fn user_submit_task(
    pool: &InfraPool,
    creator_id: i64,
    group_name: String,
    tags: Vec<String>,
    timeout: std::time::Duration,
    priority: i32,
    spec: TaskSpec,
) -> crate::error::Result<(i64, Uuid)> {
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
    let spec = serde_json::to_value(spec)?;
    let task = ActiveTask::ActiveModel {
        creator_id: Set(creator_id),
        group_id: Set(group_id),
        task_id: Set(task_id),
        uuid: Set(uuid),
        tags: Set(tags),
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
        Ok((task.task_id, task.uuid))
    }
}

pub async fn get_task(pool: &InfraPool, uuid: Uuid) -> crate::error::Result<TaskQueryResp> {
    let task = match ActiveTask::Entity::find()
        .filter(ActiveTask::Column::Uuid.eq(uuid))
        .one(&pool.db)
        .await?
    {
        Some(task) => Some(task.into()),
        None => {
            ArchivedTasks::Entity::find()
                .filter(ArchivedTasks::Column::Uuid.eq(uuid))
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
    let creator_username = User::Entity::find_by_id(task.creator_id)
        .one(&pool.db)
        .await?
        .ok_or(Error::ApiError(crate::error::ApiError::NotFound(format!(
            "Task with uuid {}",
            uuid
        ))))?
        .username;
    let group_name = Group::Entity::find_by_id(task.group_id)
        .one(&pool.db)
        .await?
        .ok_or(Error::ApiError(crate::error::ApiError::NotFound(format!(
            "Task with uuid {}",
            uuid
        ))))?
        .group_name;
    Ok(TaskQueryResp {
        uuid: task.uuid,
        creator_username,
        group_name,
        task_id: task.task_id,
        tags: task.tags,
        created_at: task.created_at,
        updated_at: task.updated_at,
        state: task.state,
        timeout: task.timeout,
        priority: task.priority,
        spec: serde_json::from_value(task.spec)?,
        result: task.result.map(serde_json::from_value).transpose()?,
        artifacts,
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
