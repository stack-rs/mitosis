use std::{cmp::Reverse, collections::HashMap, time::Duration};

use priority_queue::PriorityQueue;
use sea_orm::{
    prelude::*,
    sea_query::{extension::postgres::PgExpr, OnConflict, Query},
    FromQueryResult, QuerySelect, Set, TransactionTrait,
};
use tokio::{
    sync::{mpsc::UnboundedReceiver, oneshot::Sender},
    time::Instant,
};
use tokio_util::sync::CancellationToken;

use crate::{
    config::InfraPool,
    entity::{
        active_tasks as ActiveTask, archived_tasks as ArchivedTask, artifacts as Artifact,
        group_worker as GroupWorker, groups as Group,
        role::GroupWorkerRole,
        state::{GroupState, TaskState},
        users as User, workers as Worker,
    },
    error::{ApiError, Error},
    schema::{ReportTaskOp, TaskSpec, WorkerTaskResp},
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
    let write_groups: Vec<i64> = Group::Entity::find()
        .filter(Expr::col(Group::Column::GroupName).eq(sea_orm::sea_query::PgFunc::any(groups)))
        .select_only()
        .column(Group::Column::Id)
        .into_tuple()
        .all(&pool.db)
        .await?;
    let write_group_relations = write_groups
        .into_iter()
        .map(|group_id| GroupWorker::ActiveModel {
            group_id: Set(group_id),
            worker_id: Set(worker.id),
            role: Set(GroupWorkerRole::Write),
            ..Default::default()
        })
        .collect::<Vec<_>>();
    GroupWorker::Entity::insert_many(write_group_relations)
        .exec(&pool.db)
        .await?;

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
        .send(WorkerHeartbeatQueueOp::Heartbeat(id))
        .is_err()
        || pool
            .worker_task_queue_tx
            .send(WorkerTaskQueueOp::RegisterWorker(id))
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
        let op = WorkerTaskQueueOp::AddTasks(id, tasks.into_iter().map(Into::into).collect());
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

pub async fn unregister_worker(worker_id: i64, pool: &InfraPool) -> crate::error::Result<()> {
    let worker = Worker::Entity::find_by_id(worker_id)
        .one(&pool.db)
        .await?
        .ok_or(ApiError::NotFound(format!(
            "Worker {} not found",
            worker_id
        )))?;
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
        let op = WorkerTaskQueueOp::BatchAddTask(
            workers.into_iter().map(i64::from).collect(),
            task.id,
            task.priority,
        );
        if pool.worker_task_queue_tx.send(op).is_err() {
            return Err(Error::Custom("send batch add task failed".to_string()));
        }
    }
    GroupWorker::Entity::delete_many()
        .filter(GroupWorker::Column::WorkerId.eq(worker_id))
        .exec(&pool.db)
        .await?;
    Worker::Entity::delete_by_id(worker_id)
        .exec(&pool.db)
        .await?;
    if pool
        .worker_task_queue_tx
        .send(WorkerTaskQueueOp::UnregisterWorker(worker_id))
        .is_err()
        || pool
            .worker_heartbeat_queue_tx
            .send(WorkerHeartbeatQueueOp::UnregisterWorker(worker_id))
            .is_err()
    {
        Err(Error::Custom("send unregister worker failed".to_string()))
    } else {
        Ok(())
    }
}

pub async fn remove_worker_by_uuid(
    worker_uuid: Uuid,
    pool: &InfraPool,
) -> crate::error::Result<()> {
    match Worker::Entity::find()
        .filter(Worker::Column::WorkerId.eq(worker_uuid))
        .select_only()
        .column(Worker::Column::Id)
        .into_tuple()
        .one(&pool.db)
        .await?
    {
        Some(worker_id) => unregister_worker(worker_id, pool).await,
        None => Ok(()),
    }
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
        .send(WorkerHeartbeatQueueOp::Heartbeat(worker_id))
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
        .send(WorkerTaskQueueOp::RemoveTask(task_id))
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
            .send(WorkerTaskQueueOp::FetchTask(worker_id, tx))
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
            pool.db
                .transaction(|txn| {
                    Box::pin(async move {
                        archived_task.insert(txn).await?;
                        ActiveTask::Entity::delete_by_id(task_id).exec(txn).await?;
                        worker.update(txn).await?;
                        Ok(())
                    })
                })
                .await?;
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
                            ApiError::InvalidRequest("Group fot the task not found".to_string()),
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
                        match artifact {
                            Some(artifact) => {
                                let new_storage_used =
                                    group.storage_used + content_length - artifact.size;
                                if new_storage_used > group.storage_quota {
                                    return Err(ApiError::QuotaExceeded.into());
                                }
                                let artifact = Artifact::ActiveModel {
                                    id: Set(artifact.id),
                                    size: Set(content_length),
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
                        let key = format!("{}/{}", task.uuid, content_type);
                        Ok(get_presigned_upload_link(
                            &s3_client,
                            "mitosis-artifacts",
                            key,
                            content_length,
                        )
                        .await?)
                    })
                })
                .await?;
            return Ok(Some(uri));
        }
    }
    Ok(None)
}

// MARK: WorkerTaskQueue
#[derive(Debug)]
pub struct WorkerTaskQueue {
    /// Map from worker id to priority queue of tasks.
    /// Every task is represented by a tuple of (task id, priority).
    pub workers: HashMap<i64, PriorityQueue<i64, i32>>,
    cancel_token: CancellationToken,
    rx: UnboundedReceiver<WorkerTaskQueueOp>,
}

pub enum WorkerTaskQueueOp {
    RegisterWorker(i64),
    UnregisterWorker(i64),
    /// Add a task to a worker. The first i64 is the worker_id, the second i64 is the task_id, and the i32 is the priority.
    AddTask(i64, i64, i32),
    AddTasks(i64, Vec<(i64, i32)>),
    BatchAddTask(Vec<i64>, i64, i32),
    BatchAddTasks(Vec<i64>, Vec<(i64, i32)>),
    RemoveTask(i64),
    FetchTask(i64, Sender<Option<i64>>),
}

impl WorkerTaskQueue {
    pub fn new(cancel_token: CancellationToken, rx: UnboundedReceiver<WorkerTaskQueueOp>) -> Self {
        Self {
            workers: HashMap::new(),
            cancel_token,
            rx,
        }
    }

    fn register_worker(&mut self, worker_id: i64) {
        tracing::debug!("worker {} registered", worker_id);
        self.workers.entry(worker_id).or_default();
    }

    fn unregister_worker(&mut self, worker_id: i64) {
        tracing::debug!("worker {} unregistered", worker_id);
        self.workers.remove(&worker_id);
    }

    fn add_task(&mut self, worker_id: i64, task_id: i64, priority: i32) {
        tracing::debug!("Add task {} to worker {}", task_id, worker_id);
        if let Some(worker) = self.workers.get_mut(&worker_id) {
            worker.push(task_id, priority);
        }
    }

    fn add_tasks(&mut self, worker_id: i64, tasks: Vec<(i64, i32)>) {
        tracing::debug!("Add {} tasks to worker {}", tasks.len(), worker_id);
        if let Some(worker) = self.workers.get_mut(&worker_id) {
            tasks.into_iter().for_each(|(task_id, priority)| {
                worker.push(task_id, priority);
            });
        }
    }

    fn batch_add_task(&mut self, worker_ids: Vec<i64>, task_id: i64, priority: i32) {
        tracing::debug!("Batch add task {} to {} workers", task_id, worker_ids.len());
        worker_ids.into_iter().for_each(|worker_id| {
            self.add_task(worker_id, task_id, priority);
        });
    }

    fn batch_add_tasks(&mut self, worker_ids: Vec<i64>, tasks: Vec<(i64, i32)>) {
        tracing::debug!(
            "Batch add {} tasks to {} workers",
            tasks.len(),
            worker_ids.len()
        );
        for worker_id in worker_ids {
            self.add_tasks(worker_id, tasks.clone());
        }
    }

    fn remove_task(&mut self, task_id: i64) {
        tracing::debug!("Remove task {}", task_id);
        self.workers.iter_mut().for_each(|(_, worker)| {
            worker.remove(&task_id);
        });
    }

    fn fetch_task(&mut self, worker_id: i64) -> Option<i64> {
        tracing::debug!("Fetch task from worker {}", worker_id);
        if let Some(worker) = self.workers.get_mut(&worker_id) {
            worker.pop().map(|(task_id, _)| task_id)
        } else {
            None
        }
    }

    fn handle_op(&mut self, op: Option<WorkerTaskQueueOp>) -> bool {
        match op {
            None => {
                return true;
            }
            Some(op) => match op {
                WorkerTaskQueueOp::RegisterWorker(worker_id) => {
                    self.register_worker(worker_id);
                }
                WorkerTaskQueueOp::UnregisterWorker(worker_id) => {
                    self.unregister_worker(worker_id);
                }
                WorkerTaskQueueOp::AddTask(worker_id, task_id, priority) => {
                    self.add_task(worker_id, task_id, priority);
                }
                WorkerTaskQueueOp::AddTasks(worker_id, tasks) => {
                    self.add_tasks(worker_id, tasks);
                }
                WorkerTaskQueueOp::BatchAddTask(worker_ids, task_id, priority) => {
                    self.batch_add_task(worker_ids, task_id, priority);
                }
                WorkerTaskQueueOp::BatchAddTasks(worker_ids, tasks) => {
                    self.batch_add_tasks(worker_ids, tasks);
                }
                WorkerTaskQueueOp::RemoveTask(task_id) => {
                    self.remove_task(task_id);
                }
                WorkerTaskQueueOp::FetchTask(worker_id, tx) => {
                    let task_id = self.fetch_task(worker_id);
                    if let Err(e) = tx.send(task_id) {
                        if !self.cancel_token.is_cancelled() {
                            tracing::error!("send task id failed: {:?}", e);
                        }
                        return true;
                    }
                }
            },
        }
        false
    }

    pub async fn run(&mut self) {
        loop {
            tokio::select! {
                biased;
                _ = self.cancel_token.cancelled() => {
                    break;
                }
                op = self.rx.recv() => if self.handle_op(op) {
                    self.cancel_token.cancel();
                    break;
                }
            }
        }
    }
}

// MARK: WorkerHeartbeatQueue
#[derive(Debug)]
pub struct WorkerHeartbeatQueue {
    pub workers: PriorityQueue<i64, Reverse<Instant>>,
    cancel_token: CancellationToken,
    heartbeat_timeout: Duration,
    db: DatabaseConnection,
    rx: UnboundedReceiver<WorkerHeartbeatQueueOp>,
}

pub enum WorkerHeartbeatQueueOp {
    UnregisterWorker(i64),
    Heartbeat(i64),
}

impl WorkerHeartbeatQueue {
    pub fn new(
        cancel_token: CancellationToken,
        heartbeat_timeout: Duration,
        db: DatabaseConnection,
        rx: UnboundedReceiver<WorkerHeartbeatQueueOp>,
    ) -> Self {
        Self {
            workers: PriorityQueue::new(),
            cancel_token,
            heartbeat_timeout,
            db,
            rx,
        }
    }

    fn unregister_worker(&mut self, worker_id: i64) {
        tracing::debug!("worker {} unregistered", worker_id);
        self.workers.remove(&worker_id);
    }

    fn heartbeat(&mut self, worker_id: i64) {
        tracing::debug!("worker {} heartbeat", worker_id);
        self.workers
            .push(worker_id, Reverse(Instant::now() + self.heartbeat_timeout));
    }

    fn handle_op(&mut self, op: WorkerHeartbeatQueueOp) {
        match op {
            WorkerHeartbeatQueueOp::UnregisterWorker(worker_id) => {
                self.unregister_worker(worker_id);
            }
            WorkerHeartbeatQueueOp::Heartbeat(worker_id) => {
                self.heartbeat(worker_id);
            }
        }
    }

    async fn handle_timeout(&mut self) -> Result<(), DbErr> {
        if let Some(true) = self.workers.peek().map(|(_, r)| r.0 <= Instant::now()) {
            let (worker_id, _) = self.workers.pop().unwrap();
            GroupWorker::Entity::delete_many()
                .filter(GroupWorker::Column::WorkerId.eq(worker_id))
                .exec(&self.db)
                .await?;
            Worker::Entity::delete_by_id(worker_id)
                .exec(&self.db)
                .await?;
        }
        Ok(())
    }

    pub async fn run(&mut self) {
        let mut timeout_duration = self.heartbeat_timeout;
        loop {
            tokio::select! {
                biased;
                _ = self.cancel_token.cancelled() => {
                    break;
                }
                op = self.rx.recv() => match op {
                    None => {
                        break;
                    }
                    Some(op) => {
                        self.handle_op(op);
                        timeout_duration = self
                            .workers
                            .peek()
                            .map(|(_, r)| r.0 - Instant::now())
                            .unwrap_or(self.heartbeat_timeout);
                    }
                },
                _ = tokio::time::sleep(timeout_duration) => {
                    if let Err(e) = self.handle_timeout().await {
                        tracing::error!("handle timeout failed: {:?}", e);
                        self.cancel_token.cancel();
                        break;
                    }
                    timeout_duration = self
                            .workers
                            .peek()
                            .map(|(_, r)| r.0 - Instant::now())
                            .unwrap_or(self.heartbeat_timeout);
                }
            }
        }
    }
}
