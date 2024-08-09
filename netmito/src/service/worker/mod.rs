pub mod heartbeat;
pub mod queue;

use std::collections::{HashMap, HashSet};

pub use heartbeat::{HeartbeatOp, HeartbeatQueue};
pub use queue::{TaskDispatcher, TaskDispatcherOp};

use sea_orm::{
    prelude::*,
    sea_query::{extension::postgres::PgExpr, Alias, Nullable, OnConflict, PgFunc, Query},
    FromQueryResult, QuerySelect, Set, TransactionTrait,
};

use crate::{
    config::InfraPool,
    entity::{
        active_tasks as ActiveTask, archived_tasks as ArchivedTask, artifacts as Artifact,
        group_worker as GroupWorker, groups as Group,
        role::{GroupWorkerRole, UserGroupRole},
        state::{GroupState, TaskState, WorkerState},
        user_group as UserGroup, users as User, workers as Worker,
    },
    error::{ApiError, Error},
    schema::{
        RawWorkerQueryInfo, ReportTaskOp, TaskSpec, WorkerQueryInfo, WorkerQueryResp,
        WorkerShutdownOp, WorkerTaskResp, WorkersQueryReq, WorkersQueryResp,
    },
};

use super::s3::get_presigned_upload_link;

pub async fn register_worker(
    creator_id: i64,
    tags: Vec<String>,
    groups: Vec<String>,
    pool: &InfraPool,
) -> crate::error::Result<Uuid> {
    let uuid = uuid::Uuid::new_v4();
    let now = TimeDateTimeWithTimeZone::now_utc();
    let worker = Worker::ActiveModel {
        worker_id: Set(uuid),
        creator_id: Set(creator_id),
        tags: Set(tags),
        created_at: Set(now),
        updated_at: Set(now),
        last_heartbeat: Set(now),
        ..Default::default()
    };
    let worker = worker.insert(&pool.db).await?;
    if !groups.is_empty() {
        let write_groups: Vec<i64> = Group::Entity::find()
            .filter(Expr::col(Group::Column::GroupName).eq(sea_orm::sea_query::PgFunc::any(groups)))
            .select_only()
            .column(Group::Column::Id)
            .into_tuple()
            .all(&pool.db)
            .await?;
        let write_group_relations =
            write_groups
                .into_iter()
                .map(|group_id| GroupWorker::ActiveModel {
                    group_id: Set(group_id),
                    worker_id: Set(worker.id),
                    role: Set(GroupWorkerRole::Write),
                    ..Default::default()
                });
        GroupWorker::Entity::insert_many(write_group_relations)
            .exec(&pool.db)
            .await?;
    }

    let builder = pool.db.get_database_backend();
    let group_id_stmt = Query::select()
        .column((Group::Entity, Group::Column::Id))
        .from(Group::Entity)
        .join(
            sea_orm::JoinType::Join,
            User::Entity,
            Expr::col((User::Entity, User::Column::Username))
                .eq(Expr::col((Group::Entity, Group::Column::GroupName))),
        )
        .and_where(User::Column::Id.eq(creator_id))
        .to_owned();
    let group_id: Option<PartialGroupId> =
        PartialGroupId::find_by_statement(builder.build(&group_id_stmt))
            .one(&pool.db)
            .await?;
    if let Some(PartialGroupId { id }) = group_id {
        let admin_group_relation = GroupWorker::ActiveModel {
            group_id: Set(id),
            worker_id: Set(worker.id),
            role: Set(GroupWorkerRole::Admin),
            ..Default::default()
        };
        GroupWorker::Entity::insert(admin_group_relation)
            .on_conflict(
                OnConflict::columns([GroupWorker::Column::GroupId, GroupWorker::Column::WorkerId])
                    .update_column(GroupWorker::Column::Role)
                    .to_owned(),
            )
            .exec(&pool.db)
            .await?;
    }
    setup_worker_queues(pool, worker.id, worker.tags).await?;
    Ok(worker.worker_id)
}

pub async fn restore_workers(pool: &InfraPool) -> crate::error::Result<()> {
    let workers: Vec<(i64, Vec<String>)> = Worker::Entity::find()
        .select_only()
        .columns([Worker::Column::Id, Worker::Column::Tags])
        .into_tuple()
        .all(&pool.db)
        .await?;
    for (id, tags) in workers {
        setup_worker_queues(pool, id, tags).await?;
    }
    Ok(())
}

async fn setup_worker_queues(
    pool: &InfraPool,
    id: i64,
    tags: Vec<String>,
) -> crate::error::Result<()> {
    if pool
        .worker_heartbeat_queue_tx
        .send(HeartbeatOp::Heartbeat(id))
        .is_err()
        || pool
            .worker_task_queue_tx
            .send(TaskDispatcherOp::RegisterWorker(id))
            .is_err()
    {
        Err(Error::Custom("send register worker failed".to_string()))
    } else {
        // Add tasks from active_tasks to worker_task_queue
        let builder = pool.db.get_database_backend();
        let tasks_stmt = Query::select()
            .columns([
                (ActiveTask::Entity, ActiveTask::Column::Id),
                (ActiveTask::Entity, ActiveTask::Column::Priority),
            ])
            .from(ActiveTask::Entity)
            .join(
                sea_orm::JoinType::Join,
                GroupWorker::Entity,
                Expr::col((ActiveTask::Entity, ActiveTask::Column::GroupId)).eq(Expr::col((
                    GroupWorker::Entity,
                    GroupWorker::Column::GroupId,
                ))),
            )
            .join(
                sea_orm::JoinType::Join,
                Worker::Entity,
                Expr::col((GroupWorker::Entity, GroupWorker::Column::WorkerId))
                    .eq(Expr::col((Worker::Entity, Worker::Column::Id))),
            )
            .and_where(Expr::col((GroupWorker::Entity, GroupWorker::Column::WorkerId)).eq(id))
            .and_where(Expr::col((GroupWorker::Entity, GroupWorker::Column::Role)).gt(0))
            .and_where(Expr::col((ActiveTask::Entity, ActiveTask::Column::Tags)).contained(tags))
            .to_owned();
        let tasks: Vec<PartialActiveTask> =
            PartialActiveTask::find_by_statement(builder.build(&tasks_stmt))
                .all(&pool.db)
                .await?;
        let op = TaskDispatcherOp::AddTasks(id, tasks.into_iter().map(Into::into).collect());
        if pool.worker_task_queue_tx.send(op).is_err() {
            Err(Error::Custom("send add tasks failed".to_string()))
        } else {
            Ok(())
        }
    }
}

#[derive(Debug, Clone, FromQueryResult)]
struct PartialGroupId {
    id: i64,
}

#[derive(Debug, Clone, FromQueryResult)]
struct PartialActiveTask {
    id: i64,
    priority: i32,
}

impl From<PartialActiveTask> for (i64, i32) {
    fn from(task: PartialActiveTask) -> Self {
        (task.id, task.priority)
    }
}

async fn remove_worker(worker: Worker::Model, pool: &InfraPool) -> crate::error::Result<()> {
    // push back the assigned task (if have)
    if let Some(task_id) = worker.assigned_task_id {
        let task = ActiveTask::ActiveModel {
            id: Set(task_id),
            assigned_worker: Set(None),
            state: Set(TaskState::Ready),
            updated_at: Set(TimeDateTimeWithTimeZone::now_utc()),
            ..Default::default()
        };
        let task = task.update(&pool.db).await?;
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
            .and_where(
                Expr::col((GroupWorker::Entity, GroupWorker::Column::GroupId)).eq(task.group_id),
            )
            .and_where(Expr::col((GroupWorker::Entity, GroupWorker::Column::Role)).gt(0))
            .and_where(Expr::col((Worker::Entity, Worker::Column::Tags)).contains(task.tags))
            .to_owned();
        let workers: Vec<super::task::PartialWorkerId> =
            super::task::PartialWorkerId::find_by_statement(builder.build(&tasks_stmt))
                .all(&pool.db)
                .await?;
        let op = TaskDispatcherOp::BatchAddTask(
            workers.into_iter().map(i64::from).collect(),
            task.id,
            task.priority,
        );
        if pool.worker_task_queue_tx.send(op).is_err() {
            return Err(Error::Custom("send batch add task op failed".to_string()));
        }
    }
    pool.db
        .transaction(|txn| {
            Box::pin(async move {
                GroupWorker::Entity::delete_many()
                    .filter(GroupWorker::Column::WorkerId.eq(worker.id))
                    .exec(txn)
                    .await?;
                Worker::Entity::delete_by_id(worker.id).exec(txn).await?;
                Ok(())
            })
        })
        .await?;

    if pool
        .worker_task_queue_tx
        .send(TaskDispatcherOp::UnregisterWorker(worker.id))
        .is_err()
        || pool
            .worker_heartbeat_queue_tx
            .send(HeartbeatOp::UnregisterWorker(worker.id))
            .is_err()
    {
        Err(Error::Custom("send remove worker op failed".to_string()))
    } else {
        Ok(())
    }
}

pub async fn unregister_worker(worker_id: i64, pool: &InfraPool) -> crate::error::Result<()> {
    let worker = Worker::Entity::find_by_id(worker_id)
        .one(&pool.db)
        .await?
        .ok_or(ApiError::NotFound(format!(
            "Worker {} not found",
            worker_id
        )))?;
    remove_worker(worker, pool).await
}

pub async fn remove_worker_by_uuid(
    worker_uuid: Uuid,
    op: WorkerShutdownOp,
    pool: &InfraPool,
) -> crate::error::Result<()> {
    let worker = Worker::Entity::find()
        .filter(Worker::Column::WorkerId.eq(worker_uuid))
        .one(&pool.db)
        .await?
        .ok_or(ApiError::NotFound(format!(
            "Worker {} not found",
            worker_uuid
        )))?;
    match op {
        WorkerShutdownOp::Force => remove_worker(worker, pool).await?,
        WorkerShutdownOp::Graceful => {
            if worker.assigned_task_id.is_none() {
                remove_worker(worker, pool).await?;
            } else {
                let worker = Worker::ActiveModel {
                    id: Set(worker.id),
                    state: Set(WorkerState::GracefulShutdown),
                    ..Default::default()
                };
                worker.update(&pool.db).await?;
            }
        }
    }
    Ok(())
}

pub async fn user_remove_worker_by_uuid(
    user_id: i64,
    worker_uuid: Uuid,
    op: WorkerShutdownOp,
    pool: &InfraPool,
) -> crate::error::Result<()> {
    let builder = pool.db.get_database_backend();
    let worker = Worker::Entity::find()
        .filter(Worker::Column::WorkerId.eq(worker_uuid))
        .one(&pool.db)
        .await?
        .ok_or(ApiError::NotFound(format!(
            "Worker {} not found",
            worker_uuid
        )))?;
    let stmt = Query::select()
        .column((Group::Entity, Group::Column::GroupName))
        .column((GroupWorker::Entity, GroupWorker::Column::Role))
        .from(GroupWorker::Entity)
        .join(
            sea_orm::JoinType::Join,
            Group::Entity,
            Expr::col((Group::Entity, Group::Column::Id)).eq(Expr::col((
                GroupWorker::Entity,
                GroupWorker::Column::GroupId,
            ))),
        )
        .join(
            sea_orm::JoinType::Join,
            UserGroup::Entity,
            Expr::col((UserGroup::Entity, UserGroup::Column::GroupId))
                .eq(Expr::col((Group::Entity, Group::Column::Id))),
        )
        .and_where(Expr::col((GroupWorker::Entity, GroupWorker::Column::WorkerId)).eq(worker.id))
        .and_where(
            Expr::col((GroupWorker::Entity, GroupWorker::Column::Role)).eq(GroupWorkerRole::Admin),
        )
        .and_where(Expr::col((UserGroup::Entity, UserGroup::Column::UserId)).eq(user_id))
        .and_where(Expr::col((UserGroup::Entity, UserGroup::Column::Role)).eq(UserGroupRole::Admin))
        .limit(1)
        .to_owned();
    let _group = PartialGroupWorkerRole::find_by_statement(builder.build(&stmt))
        .one(&pool.db)
        .await?
        .ok_or(ApiError::AuthError(
            crate::error::AuthError::PermissionDenied,
        ))?;
    match op {
        WorkerShutdownOp::Force => remove_worker(worker, pool).await?,
        WorkerShutdownOp::Graceful => {
            if worker.assigned_task_id.is_none() {
                remove_worker(worker, pool).await?;
            } else {
                let worker = Worker::ActiveModel {
                    id: Set(worker.id),
                    state: Set(WorkerState::GracefulShutdown),
                    ..Default::default()
                };
                worker.update(&pool.db).await?;
            }
        }
    }
    Ok(())
}

pub async fn heartbeat(worker_id: i64, pool: &InfraPool) -> crate::error::Result<()> {
    let worker = Worker::ActiveModel {
        id: Set(worker_id),
        last_heartbeat: Set(TimeDateTimeWithTimeZone::now_utc()),
        ..Default::default()
    };
    // TODO: check if the task is already expired
    let _worker = worker.update(&pool.db).await?;
    if pool
        .worker_heartbeat_queue_tx
        .send(HeartbeatOp::Heartbeat(worker_id))
        .is_err()
    {
        Err(Error::Custom("send worker heartbeat failed".to_string()))
    } else {
        Ok(())
    }
}

pub fn remove_task(task_id: i64, pool: &InfraPool) -> crate::error::Result<()> {
    if pool
        .worker_task_queue_tx
        .send(TaskDispatcherOp::RemoveTask(task_id))
        .is_err()
    {
        Err(Error::Custom("send remove task failed".to_string()))
    } else {
        Ok(())
    }
}

pub async fn fetch_task(
    worker_id: i64,
    pool: &InfraPool,
) -> crate::error::Result<Option<WorkerTaskResp>> {
    loop {
        let (tx, rx) = tokio::sync::oneshot::channel();
        if pool
            .worker_task_queue_tx
            .send(TaskDispatcherOp::FetchTask(worker_id, tx))
            .is_err()
        {
            break Err(Error::Custom("send fetch task failed".to_string()));
        } else {
            match rx
                .await
                .map_err(|e| Error::Custom(format!("recv fetch task failed: {:?}", e)))?
            {
                Some(id) => {
                    let now = TimeDateTimeWithTimeZone::now_utc();
                    let tasks = ActiveTask::Entity::update_many()
                        .col_expr(ActiveTask::Column::AssignedWorker, Expr::value(worker_id))
                        .col_expr(ActiveTask::Column::UpdatedAt, Expr::value(now))
                        .col_expr(ActiveTask::Column::State, Expr::value(TaskState::Running))
                        .filter(ActiveTask::Column::Id.eq(id))
                        .filter(ActiveTask::Column::AssignedWorker.is_null())
                        .filter(ActiveTask::Column::State.eq(TaskState::Ready))
                        .exec_with_returning(&pool.db)
                        .await?;
                    if !tasks.is_empty() {
                        let task = tasks.into_iter().next().unwrap();
                        let worker: Worker::ActiveModel = Worker::ActiveModel {
                            id: Set(worker_id),
                            updated_at: Set(now),
                            assigned_task_id: Set(Some(task.id)),
                            ..Default::default()
                        };
                        worker.update(&pool.db).await?;
                        let spec: TaskSpec = serde_json::from_value(task.spec)?;
                        let task_resp = WorkerTaskResp {
                            id: task.id,
                            uuid: task.uuid,
                            timeout: std::time::Duration::from_secs(task.timeout as u64),
                            spec,
                        };
                        break Ok(Some(task_resp));
                    }
                }
                None => break Ok(None),
            }
        }
    }
}

pub async fn report_task(
    worker_id: i64,
    task_id: i64,
    op: ReportTaskOp,
    pool: &InfraPool,
) -> crate::error::Result<Option<String>> {
    let now = TimeDateTimeWithTimeZone::now_utc();
    let task = ActiveTask::Entity::find_by_id(task_id)
        .one(&pool.db)
        .await?
        .ok_or(ApiError::NotFound(format!("Task {} not found", task_id)))?;
    if let Some(w_id) = task.assigned_worker {
        if w_id != worker_id {
            return Err(ApiError::NotFound(format!("Task {} not found", task_id)).into());
        }
    } else {
        return Err(ApiError::NotFound(format!("Task {} not found", task_id)).into());
    }
    match op {
        ReportTaskOp::Finish => {
            tracing::debug!("Worker {} finish task {}", worker_id, task_id);
            let task = ActiveTask::ActiveModel {
                id: Set(task_id),
                state: Set(TaskState::Finished),
                updated_at: Set(now),
                ..Default::default()
            };
            task.update(&pool.db).await?;
        }
        ReportTaskOp::Cancel => {
            tracing::debug!("Worker {} cancel task {}", worker_id, task_id);
            let task = ActiveTask::ActiveModel {
                id: Set(task_id),
                state: Set(TaskState::Cancelled),
                updated_at: Set(now),
                ..Default::default()
            };
            task.update(&pool.db).await?;
        }
        ReportTaskOp::Commit(res) => {
            tracing::debug!("Worker {} commit task {}", worker_id, task_id);
            if task.state != TaskState::Finished && task.state != TaskState::Cancelled {
                return Err(
                    ApiError::InvalidRequest("Task can not be marked as done".to_string()).into(),
                );
            }
            let result =
                serde_json::to_value(res).map_err(|e| ApiError::InvalidRequest(e.to_string()))?;
            let archived_task = ArchivedTask::ActiveModel {
                id: Set(task.id),
                creator_id: Set(task.creator_id),
                group_id: Set(task.group_id),
                task_id: Set(task.task_id),
                uuid: Set(task.uuid),
                tags: Set(task.tags),
                labels: Set(task.labels),
                created_at: Set(task.created_at),
                updated_at: Set(now),
                state: Set(task.state),
                assigned_worker: Set(task.assigned_worker),
                timeout: Set(task.timeout),
                priority: Set(task.priority),
                spec: Set(task.spec),
                result: Set(Some(result)),
            };
            let worker = Worker::ActiveModel {
                id: Set(worker_id),
                updated_at: Set(now),
                assigned_task_id: Set(None),
                ..Default::default()
            };
            let (worker_id, worker_state) = pool
                .db
                .transaction(|txn| {
                    Box::pin(async move {
                        archived_task.insert(txn).await?;
                        ActiveTask::Entity::delete_by_id(task_id).exec(txn).await?;
                        let worker = worker.update(txn).await?;
                        let worker_id = worker.id;
                        let worker_state = worker.state;
                        // Worker was requested to gracefully shutdown
                        if matches!(worker.state, WorkerState::GracefulShutdown) {
                            GroupWorker::Entity::delete_many()
                                .filter(GroupWorker::Column::WorkerId.eq(worker.id))
                                .exec(txn)
                                .await?;
                            Worker::Entity::delete_by_id(worker.id).exec(txn).await?;
                        }
                        Ok((worker_id, worker_state))
                    })
                })
                .await?;
            let _ = remove_task(task_id, pool)
                .inspect_err(|e| tracing::warn!("Failed to remove task {}: {:?}", task_id, e));
            // Worker was requested to gracefully shutdown
            if matches!(worker_state, WorkerState::GracefulShutdown)
                && (pool
                    .worker_task_queue_tx
                    .send(TaskDispatcherOp::UnregisterWorker(worker_id))
                    .is_err()
                    || pool
                        .worker_heartbeat_queue_tx
                        .send(HeartbeatOp::UnregisterWorker(worker_id))
                        .is_err())
            {
                return Err(Error::Custom("Worker was requested to gracefully shutdown but failed to send op through channels".to_string()));
            }
        }
        ReportTaskOp::Upload {
            content_type,
            content_length,
        } => {
            tracing::debug!(
                "Worker {} request upload task {} artifact {} of size {}",
                worker_id,
                task_id,
                content_type,
                content_length
            );
            let content_length = content_length as i64;
            let updated_task = ActiveTask::ActiveModel {
                id: Set(task_id),
                updated_at: Set(now),
                ..Default::default()
            };
            updated_task.update(&pool.db).await?;
            // Check group storage quota and allocate storage for the artifact
            let group_id = task.group_id;
            let s3_client = pool.s3.clone();
            let uri = pool
                .db
                .transaction::<_, String, Error>(|txn| {
                    Box::pin(async move {
                        let group = Group::Entity::find_by_id(group_id).one(txn).await?.ok_or(
                            ApiError::InvalidRequest("Group for the task not found".to_string()),
                        )?;
                        if group.state != GroupState::Active {
                            return Err(ApiError::InvalidRequest(
                                "Group is not active".to_string(),
                            )
                            .into());
                        }
                        let artifact = Artifact::Entity::find()
                            .filter(Artifact::Column::TaskId.eq(task.uuid))
                            .filter(Artifact::Column::ContentType.eq(content_type))
                            .one(txn)
                            .await?;
                        let s3_object_key = format!("{}/{}", task.uuid, content_type);
                        let url: String;
                        match artifact {
                            Some(artifact) => {
                                let recorded_content_length = content_length.max(artifact.size);
                                let new_storage_used =
                                    group.storage_used + (recorded_content_length - artifact.size);
                                if new_storage_used > group.storage_quota {
                                    return Err(ApiError::QuotaExceeded.into());
                                }
                                url = get_presigned_upload_link(
                                    &s3_client,
                                    "mitosis-artifacts",
                                    s3_object_key,
                                    content_length,
                                )
                                .await
                                .map_err(ApiError::from)?;
                                let artifact = Artifact::ActiveModel {
                                    id: Set(artifact.id),
                                    size: Set(recorded_content_length),
                                    updated_at: Set(now),
                                    ..Default::default()
                                };
                                artifact.update(txn).await?;
                                let group = Group::ActiveModel {
                                    id: Set(group_id),
                                    storage_used: Set(new_storage_used),
                                    updated_at: Set(now),
                                    ..Default::default()
                                };
                                group.update(txn).await?;
                            }
                            None => {
                                let new_storage_used = group.storage_used + content_length;
                                if new_storage_used > group.storage_quota {
                                    return Err(ApiError::QuotaExceeded.into());
                                }
                                url = get_presigned_upload_link(
                                    &s3_client,
                                    "mitosis-artifacts",
                                    s3_object_key,
                                    content_length,
                                )
                                .await
                                .map_err(ApiError::from)?;
                                let artifact = Artifact::ActiveModel {
                                    task_id: Set(task.uuid),
                                    content_type: Set(content_type),
                                    size: Set(content_length),
                                    created_at: Set(now),
                                    updated_at: Set(now),
                                    ..Default::default()
                                };
                                artifact.insert(txn).await?;
                                let group = Group::ActiveModel {
                                    id: Set(group_id),
                                    storage_used: Set(new_storage_used),
                                    updated_at: Set(now),
                                    ..Default::default()
                                };
                                group.update(txn).await?;
                            }
                        }
                        Ok(url)
                    })
                })
                .await?;
            return Ok(Some(uri));
        }
    }
    Ok(None)
}

#[derive(Debug, Clone, FromQueryResult)]
struct PartialGroupWorkerRole {
    role: GroupWorkerRole,
    group_name: String,
}

pub async fn get_worker(
    pool: &InfraPool,
    worker_id: Uuid,
) -> crate::error::Result<WorkerQueryResp> {
    let builder = pool.db.get_database_backend();
    let stmt = Query::select()
        .columns([
            (Worker::Entity, Worker::Column::Id),
            (Worker::Entity, Worker::Column::WorkerId),
            (Worker::Entity, Worker::Column::Tags),
            (Worker::Entity, Worker::Column::CreatedAt),
            (Worker::Entity, Worker::Column::UpdatedAt),
            (Worker::Entity, Worker::Column::State),
            (Worker::Entity, Worker::Column::LastHeartbeat),
        ])
        .expr_as(
            Expr::col((User::Entity, User::Column::Username)),
            Alias::new("creator_username"),
        )
        .expr_as(
            Expr::case(
                Expr::col((Worker::Entity, Worker::Column::AssignedTaskId)).is_null(),
                Uuid::null(),
            )
            .finally(Expr::col((ActiveTask::Entity, ActiveTask::Column::Uuid))),
            Alias::new("assigned_task_id"),
        )
        .from(Worker::Entity)
        .join(
            sea_orm::JoinType::LeftJoin,
            ActiveTask::Entity,
            Expr::col((ActiveTask::Entity, ActiveTask::Column::Id))
                .eq(Expr::col((Worker::Entity, Worker::Column::AssignedTaskId))),
        )
        .join(
            sea_orm::JoinType::Join,
            User::Entity,
            Expr::col((User::Entity, User::Column::Id))
                .eq(Expr::col((Worker::Entity, Worker::Column::CreatorId))),
        )
        .and_where(Expr::col((Worker::Entity, Worker::Column::WorkerId)).eq(worker_id))
        .limit(1)
        .to_owned();
    let worker = RawWorkerQueryInfo::find_by_statement(builder.build(&stmt))
        .one(&pool.db)
        .await?
        .ok_or(ApiError::NotFound(format!("Worker {}", worker_id)))?;
    let id = worker.id;
    let info = worker.into();
    let group_stmt = Query::select()
        .column((Group::Entity, Group::Column::GroupName))
        .column((GroupWorker::Entity, GroupWorker::Column::Role))
        .from(Group::Entity)
        .join(
            sea_orm::JoinType::Join,
            GroupWorker::Entity,
            Expr::col((GroupWorker::Entity, GroupWorker::Column::GroupId))
                .eq(Expr::col((Group::Entity, Group::Column::Id))),
        )
        .and_where(Expr::col((GroupWorker::Entity, GroupWorker::Column::WorkerId)).eq(id))
        .to_owned();
    let groups: HashMap<String, GroupWorkerRole> =
        PartialGroupWorkerRole::find_by_statement(builder.build(&group_stmt))
            .all(&pool.db)
            .await?
            .into_iter()
            .map(|g| (g.group_name, g.role))
            .collect();
    let resp = WorkerQueryResp { info, groups };
    Ok(resp)
}

#[derive(Debug, Clone, FromQueryResult)]
struct PartialUserGroupRole {
    group_id: i64,
    // role: UserGroupRole,
    group_name: String,
}

pub async fn query_worker_list(
    user_id: i64,
    pool: &InfraPool,
    query: WorkersQueryReq,
) -> crate::error::Result<WorkersQueryResp> {
    let builder = pool.db.get_database_backend();
    let mut group_stmt = Query::select()
        .columns([
            (UserGroup::Entity, UserGroup::Column::GroupId),
            // (UserGroup::Entity, UserGroup::Column::Role),
        ])
        .column((Group::Entity, Group::Column::GroupName))
        .from(UserGroup::Entity)
        .and_where(UserGroup::Column::UserId.eq(user_id))
        .limit(1)
        .join(
            sea_orm::JoinType::Join,
            Group::Entity,
            Expr::col((Group::Entity, Group::Column::Id))
                .eq(Expr::col((UserGroup::Entity, UserGroup::Column::GroupId))),
        )
        .to_owned();
    match query.group_name {
        Some(group_name) => {
            group_stmt.and_where(
                Expr::col((Group::Entity, Group::Column::GroupName)).eq(group_name.clone()),
            );
        }
        None => {
            group_stmt
                .join(
                    sea_orm::JoinType::Join,
                    User::Entity,
                    Expr::col((User::Entity, User::Column::Username))
                        .eq(Expr::col((Group::Entity, Group::Column::GroupName))),
                )
                .and_where(Expr::col((User::Entity, User::Column::Id)).eq(user_id));
        }
    }
    let user_group_role = PartialUserGroupRole::find_by_statement(builder.build(&group_stmt))
        .one(&pool.db)
        .await?
        .ok_or(crate::error::AuthError::PermissionDenied)?;
    let mut stmt = Query::select()
        .columns([
            (Worker::Entity, Worker::Column::WorkerId),
            (Worker::Entity, Worker::Column::Tags),
            (Worker::Entity, Worker::Column::CreatedAt),
            (Worker::Entity, Worker::Column::UpdatedAt),
            (Worker::Entity, Worker::Column::State),
            (Worker::Entity, Worker::Column::LastHeartbeat),
        ])
        .expr_as(
            Expr::col((User::Entity, User::Column::Username)),
            Alias::new("creator_username"),
        )
        .expr_as(
            Expr::case(
                Expr::col((Worker::Entity, Worker::Column::AssignedTaskId)).is_null(),
                Uuid::null(),
            )
            .finally(Expr::col((ActiveTask::Entity, ActiveTask::Column::Uuid))),
            Alias::new("assigned_task_id"),
        )
        .from(Worker::Entity)
        .join(
            sea_orm::JoinType::LeftJoin,
            ActiveTask::Entity,
            Expr::col((ActiveTask::Entity, ActiveTask::Column::Id))
                .eq(Expr::col((Worker::Entity, Worker::Column::AssignedTaskId))),
        )
        .join(
            sea_orm::JoinType::Join,
            GroupWorker::Entity,
            Expr::col((GroupWorker::Entity, GroupWorker::Column::WorkerId))
                .eq(Expr::col((Worker::Entity, Worker::Column::Id))),
        )
        .join(
            sea_orm::JoinType::Join,
            User::Entity,
            Expr::col((User::Entity, User::Column::Id))
                .eq(Expr::col((Worker::Entity, Worker::Column::CreatorId))),
        )
        .and_where(
            Expr::col((GroupWorker::Entity, GroupWorker::Column::GroupId))
                .eq(user_group_role.group_id),
        )
        .to_owned();
    if let Some(tags) = query.tags {
        let tags: Vec<String> = tags.into_iter().collect();
        stmt.and_where(Expr::col((Worker::Entity, Worker::Column::Tags)).contains(tags));
    }
    if let Some(creator_username) = query.creator_username {
        stmt.and_where(
            Expr::col((User::Entity, User::Column::Username)).eq(creator_username.clone()),
        );
    }
    if let Some(role) = query.role {
        let role: Vec<GroupWorkerRole> = role.into_iter().collect();
        stmt.and_where(
            Expr::col((GroupWorker::Entity, GroupWorker::Column::Role))
                .eq(sea_orm::sea_query::PgFunc::any(role)),
        );
    }
    let workers = WorkerQueryInfo::find_by_statement(builder.build(&stmt))
        .all(&pool.db)
        .await?;
    let resp = WorkersQueryResp {
        workers,
        group_name: user_group_role.group_name,
    };
    Ok(resp)
}

pub async fn user_replace_worker_tags(
    user_id: i64,
    worker_uuid: Uuid,
    tags: HashSet<String>,
    pool: &InfraPool,
) -> crate::error::Result<()> {
    if tags.is_empty() {
        return Err(ApiError::InvalidRequest("Empty tags".to_string()).into());
    }
    let tags = tags.into_iter().collect::<Vec<_>>();
    let builder = pool.db.get_database_backend();
    let worker = Worker::Entity::find()
        .filter(Worker::Column::WorkerId.eq(worker_uuid))
        .one(&pool.db)
        .await?
        .ok_or(ApiError::NotFound(format!(
            "Worker {} not found",
            worker_uuid
        )))?;
    let stmt = Query::select()
        .column((Group::Entity, Group::Column::GroupName))
        .column((GroupWorker::Entity, GroupWorker::Column::Role))
        .from(GroupWorker::Entity)
        .join(
            sea_orm::JoinType::Join,
            Group::Entity,
            Expr::col((Group::Entity, Group::Column::Id)).eq(Expr::col((
                GroupWorker::Entity,
                GroupWorker::Column::GroupId,
            ))),
        )
        .join(
            sea_orm::JoinType::Join,
            UserGroup::Entity,
            Expr::col((UserGroup::Entity, UserGroup::Column::GroupId))
                .eq(Expr::col((Group::Entity, Group::Column::Id))),
        )
        .and_where(Expr::col((GroupWorker::Entity, GroupWorker::Column::WorkerId)).eq(worker.id))
        .and_where(
            Expr::col((GroupWorker::Entity, GroupWorker::Column::Role)).eq(PgFunc::any(vec![
                GroupWorkerRole::Write,
                GroupWorkerRole::Admin,
            ])),
        )
        .and_where(Expr::col((UserGroup::Entity, UserGroup::Column::UserId)).eq(user_id))
        .and_where(
            Expr::col((UserGroup::Entity, UserGroup::Column::Role)).eq(PgFunc::any(vec![
                UserGroupRole::Write,
                UserGroupRole::Admin,
            ])),
        )
        .limit(1)
        .to_owned();
    let _group = PartialGroupWorkerRole::find_by_statement(builder.build(&stmt))
        .one(&pool.db)
        .await?
        .ok_or(ApiError::AuthError(
            crate::error::AuthError::PermissionDenied,
        ))?;
    let mut worker: Worker::ActiveModel = worker.into();
    worker.tags = Set(tags);
    worker.updated_at = Set(TimeDateTimeWithTimeZone::now_utc());
    worker.update(&pool.db).await?;
    Ok(())
}

pub async fn user_update_worker_groups(
    user_id: i64,
    worker_uuid: Uuid,
    mut relations: HashMap<String, GroupWorkerRole>,
    pool: &InfraPool,
) -> crate::error::Result<()> {
    if relations.is_empty() {
        return Err(ApiError::InvalidRequest("Empty group relations".to_string()).into());
    }
    let builder = pool.db.get_database_backend();
    let worker = Worker::Entity::find()
        .filter(Worker::Column::WorkerId.eq(worker_uuid))
        .one(&pool.db)
        .await?
        .ok_or(ApiError::NotFound(format!(
            "Worker {} not found",
            worker_uuid
        )))?;
    let stmt = Query::select()
        .column((Group::Entity, Group::Column::GroupName))
        .column((GroupWorker::Entity, GroupWorker::Column::Role))
        .from(GroupWorker::Entity)
        .join(
            sea_orm::JoinType::Join,
            Group::Entity,
            Expr::col((Group::Entity, Group::Column::Id)).eq(Expr::col((
                GroupWorker::Entity,
                GroupWorker::Column::GroupId,
            ))),
        )
        .join(
            sea_orm::JoinType::Join,
            UserGroup::Entity,
            Expr::col((UserGroup::Entity, UserGroup::Column::GroupId))
                .eq(Expr::col((Group::Entity, Group::Column::Id))),
        )
        .and_where(Expr::col((GroupWorker::Entity, GroupWorker::Column::WorkerId)).eq(worker.id))
        .and_where(
            Expr::col((GroupWorker::Entity, GroupWorker::Column::Role)).eq(GroupWorkerRole::Admin),
        )
        .and_where(Expr::col((UserGroup::Entity, UserGroup::Column::UserId)).eq(user_id))
        .and_where(Expr::col((UserGroup::Entity, UserGroup::Column::Role)).eq(UserGroupRole::Admin))
        .limit(1)
        .to_owned();
    let _group = PartialGroupWorkerRole::find_by_statement(builder.build(&stmt))
        .one(&pool.db)
        .await?
        .ok_or(ApiError::AuthError(
            crate::error::AuthError::PermissionDenied,
        ))?;
    let group_names = relations.keys().cloned().collect::<Vec<_>>();
    let group_count = group_names.len();
    let groups: Vec<(i64, String)> = Group::Entity::find()
        .filter(Expr::col(Group::Column::GroupName).eq(PgFunc::any(group_names)))
        .select_only()
        .column(Group::Column::Id)
        .column(Group::Column::GroupName)
        .into_tuple()
        .all(&pool.db)
        .await?;
    if groups.len() != group_count {
        return Err(ApiError::InvalidRequest("Some groups do not exist".to_string()).into());
    }
    let group_worker_relations = groups.into_iter().filter_map(|(group_id, group_name)| {
        relations
            .remove(&group_name)
            .map(|role| GroupWorker::ActiveModel {
                group_id: Set(group_id),
                worker_id: Set(worker.id),
                role: Set(role),
                ..Default::default()
            })
    });
    GroupWorker::Entity::insert_many(group_worker_relations)
        .on_conflict(
            OnConflict::columns([GroupWorker::Column::GroupId, GroupWorker::Column::WorkerId])
                .update_column(GroupWorker::Column::Role)
                .to_owned(),
        )
        .exec(&pool.db)
        .await?;
    Ok(())
}

pub async fn user_remove_worker_groups(
    user_id: i64,
    worker_uuid: Uuid,
    groups: HashSet<String>,
    pool: &InfraPool,
) -> crate::error::Result<()> {
    if groups.is_empty() {
        return Err(ApiError::InvalidRequest("Empty group names".to_string()).into());
    }
    let builder = pool.db.get_database_backend();
    let worker = Worker::Entity::find()
        .filter(Worker::Column::WorkerId.eq(worker_uuid))
        .one(&pool.db)
        .await?
        .ok_or(ApiError::NotFound(format!(
            "Worker {} not found",
            worker_uuid
        )))?;
    let stmt = Query::select()
        .column((Group::Entity, Group::Column::GroupName))
        .column((GroupWorker::Entity, GroupWorker::Column::Role))
        .from(GroupWorker::Entity)
        .join(
            sea_orm::JoinType::Join,
            Group::Entity,
            Expr::col((Group::Entity, Group::Column::Id)).eq(Expr::col((
                GroupWorker::Entity,
                GroupWorker::Column::GroupId,
            ))),
        )
        .join(
            sea_orm::JoinType::Join,
            UserGroup::Entity,
            Expr::col((UserGroup::Entity, UserGroup::Column::GroupId))
                .eq(Expr::col((Group::Entity, Group::Column::Id))),
        )
        .and_where(Expr::col((GroupWorker::Entity, GroupWorker::Column::WorkerId)).eq(worker.id))
        .and_where(
            Expr::col((GroupWorker::Entity, GroupWorker::Column::Role)).eq(GroupWorkerRole::Admin),
        )
        .and_where(Expr::col((UserGroup::Entity, UserGroup::Column::UserId)).eq(user_id))
        .and_where(Expr::col((UserGroup::Entity, UserGroup::Column::Role)).eq(UserGroupRole::Admin))
        .limit(1)
        .to_owned();
    let _group = PartialGroupWorkerRole::find_by_statement(builder.build(&stmt))
        .one(&pool.db)
        .await?
        .ok_or(ApiError::AuthError(
            crate::error::AuthError::PermissionDenied,
        ))?;
    let group_names = groups.into_iter().collect::<Vec<_>>();
    let group_count = group_names.len();
    let groups: Vec<i64> = Group::Entity::find()
        .filter(Expr::col(Group::Column::GroupName).eq(PgFunc::any(group_names)))
        .select_only()
        .column(Group::Column::Id)
        .into_tuple()
        .all(&pool.db)
        .await?;
    if groups.len() != group_count {
        return Err(ApiError::InvalidRequest("Some groups do not exist".to_string()).into());
    }
    GroupWorker::Entity::delete_many()
        .filter(Expr::col(GroupWorker::Column::GroupId).eq(PgFunc::any(groups)))
        .filter(Expr::col(GroupWorker::Column::WorkerId).eq(worker.id))
        .exec(&pool.db)
        .await?;
    Ok(())
}
