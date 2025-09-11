use std::collections::HashMap;

use priority_queue::PriorityQueue;

#[cfg(not(feature = "crossfire-channel"))]
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::oneshot::Sender;
use tokio_util::sync::CancellationToken;

// MARK: TaskDispatcher
#[derive(Debug)]
pub struct TaskDispatcher {
    /// Map from worker id to priority queue of tasks.
    /// Every task is represented by a tuple of (task id, priority).
    pub workers: HashMap<i64, PriorityQueue<i64, i32>>,
    cancel_token: CancellationToken,
    #[cfg(not(feature = "crossfire-channel"))]
    rx: UnboundedReceiver<TaskDispatcherOp>,
    #[cfg(feature = "crossfire-channel")]
    rx: crossfire::AsyncRx<TaskDispatcherOp>,
}

pub enum TaskDispatcherOp {
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

impl TaskDispatcher {
    pub fn new(
        cancel_token: CancellationToken,
        #[cfg(not(feature = "crossfire-channel"))] rx: UnboundedReceiver<TaskDispatcherOp>,
        #[cfg(feature = "crossfire-channel")] rx: crossfire::AsyncRx<TaskDispatcherOp>,
    ) -> Self {
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
        tracing::trace!("Fetch task from worker {}", worker_id);
        if let Some(worker) = self.workers.get_mut(&worker_id) {
            worker.pop().map(|(task_id, _)| task_id)
        } else {
            None
        }
    }

    fn handle_op(&mut self, op: Option<TaskDispatcherOp>) -> bool {
        match op {
            None => {
                return true;
            }
            Some(op) => match op {
                TaskDispatcherOp::RegisterWorker(worker_id) => {
                    self.register_worker(worker_id);
                }
                TaskDispatcherOp::UnregisterWorker(worker_id) => {
                    self.unregister_worker(worker_id);
                }
                TaskDispatcherOp::AddTask(worker_id, task_id, priority) => {
                    self.add_task(worker_id, task_id, priority);
                }
                TaskDispatcherOp::AddTasks(worker_id, tasks) => {
                    self.add_tasks(worker_id, tasks);
                }
                TaskDispatcherOp::BatchAddTask(worker_ids, task_id, priority) => {
                    self.batch_add_task(worker_ids, task_id, priority);
                }
                TaskDispatcherOp::BatchAddTasks(worker_ids, tasks) => {
                    self.batch_add_tasks(worker_ids, tasks);
                }
                TaskDispatcherOp::RemoveTask(task_id) => {
                    self.remove_task(task_id);
                }
                TaskDispatcherOp::FetchTask(worker_id, tx) => {
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
        #[cfg(not(feature = "crossfire-channel"))]
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
        #[cfg(feature = "crossfire-channel")]
        loop {
            tokio::select! {
                biased;
                _ = self.cancel_token.cancelled() => {
                    break;
                }
                op = self.rx.recv() => if self.handle_op(op.ok()) {
                    self.cancel_token.cancel();
                    break;
                }
            }
        }
    }
}
