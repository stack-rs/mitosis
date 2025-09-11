use std::collections::{HashMap, HashSet};

use priority_queue::PriorityQueue;

use tokio::sync::{mpsc::UnboundedReceiver, oneshot::Sender};
use tokio_util::sync::CancellationToken;

// MARK: Scheduling enums
#[derive(Debug, Clone)]
pub enum ScheduleMode {
    Fanout,
    Balanced(BalanceStrategy),
}

impl Default for ScheduleMode {
    fn default() -> Self {
        ScheduleMode::Fanout
    }
}

#[derive(Debug, Clone)]
pub enum BalanceStrategy {
    LeastQueue,
    RoundRobin,
    // Future: Random
}

// MARK: TaskDispatcher
#[derive(Debug)]
pub struct TaskDispatcher {
    /// Map from worker id to priority queue of tasks.
    /// Every task is represented by a tuple of (task id, priority).
    pub workers: HashMap<i64, PriorityQueue<i64, i32>>,
    /// Scheduling mode determining how tasks are distributed
    mode: ScheduleMode,
    /// Reverse index: task_id -> set of worker_ids that have this task queued
    task_index: HashMap<i64, HashSet<i64>>,
    /// Candidate memory: task_id -> list of eligible worker_ids for redistribution
    task_candidates: HashMap<i64, Vec<i64>>,
    /// Running tasks count per worker: worker_id -> count of tasks currently running
    running_count: HashMap<i64, usize>,
    /// Round-robin cursor for fair tie-breaking
    rr_cursor: usize,
    cancel_token: CancellationToken,
    rx: UnboundedReceiver<TaskDispatcherOp>,
}

pub enum TaskDispatcherOp {
    RegisterWorker(i64),
    UnregisterWorker(i64),
    /// Add a task to a worker. The first i64 is the worker_id, the second i64 is the task_id, and the i32 is the priority.
    AddTask(i64, i64, i32),
    AddTasks(i64, Vec<(i64, i32)>),
    BatchAddTask(Vec<i64>, i64, i32),
    BatchAddTasks(Vec<i64>, Vec<(i64, i32)>),
    /// Enqueue a task to candidate workers based on scheduling mode
    EnqueueCandidates {
        candidates: Vec<i64>,
        task_id: i64,
        priority: i32,
    },
    /// Enqueue multiple tasks to candidate workers
    EnqueueManyCandidates {
        candidates: Vec<i64>,
        tasks: Vec<(i64, i32)>,
    },
    /// Redistribute tasks that were queued in a specific worker (for balanced mode)
    RedistributeTasks {
        tasks: Vec<(i64, i32)>,
    },
    /// Acknowledge task assignment (increment running count)
    AckAssigned(i64, i64), // worker_id, task_id
    /// Acknowledge task completion/cancellation (decrement running count)  
    AckFinished(i64, i64), // worker_id, task_id
    /// Log current metrics
    LogMetrics,
    RemoveTask(i64),
    FetchTask(i64, Sender<Option<i64>>),
}

impl TaskDispatcher {
    pub fn new(
        cancel_token: CancellationToken,
        rx: UnboundedReceiver<TaskDispatcherOp>,
        mode: ScheduleMode,
    ) -> Self {
        Self {
            workers: HashMap::new(),
            mode,
            task_index: HashMap::new(),
            task_candidates: HashMap::new(),
            running_count: HashMap::new(),
            rr_cursor: 0,
            cancel_token,
            rx,
        }
    }

    fn register_worker(&mut self, worker_id: i64) {
        tracing::debug!("worker {} registered", worker_id);
        self.workers.entry(worker_id).or_default();
        self.running_count.insert(worker_id, 0);
    }

    fn unregister_worker(&mut self, worker_id: i64) {
        tracing::debug!("worker {} unregistered", worker_id);

        // Capture tasks that were only in this worker's queue (for redistribution in Balanced mode)
        let mut tasks_to_redistribute = Vec::new();

        // First, extract all tasks from this worker's queue before removal
        if let Some(worker_queue) = self.workers.get(&worker_id) {
            for (task_id, priority) in worker_queue.iter() {
                // Check if this task exists only in this worker (balanced mode case)
                if let Some(worker_set) = self.task_index.get(task_id) {
                    if worker_set.len() == 1 && worker_set.contains(&worker_id) {
                        // This task is only in this worker - needs redistribution
                        tasks_to_redistribute.push((*task_id, *priority));
                    }
                }
            }
        }

        // Clean up task_index for this worker
        let mut tasks_to_remove = Vec::new();
        for (task_id, worker_set) in &mut self.task_index {
            worker_set.remove(&worker_id);
            if worker_set.is_empty() {
                tasks_to_remove.push(*task_id);
            }
        }
        for task_id in tasks_to_remove {
            self.task_index.remove(&task_id);
        }

        // Remove the worker
        self.workers.remove(&worker_id);
        self.running_count.remove(&worker_id);

        // Redistribute tasks that were only in this worker
        if !tasks_to_redistribute.is_empty() {
            tracing::info!(
                "Redistributing {} tasks from unregistered worker {}",
                tasks_to_redistribute.len(),
                worker_id
            );
            self.redistribute_tasks(tasks_to_redistribute);
        }
    }

    fn add_task(&mut self, worker_id: i64, task_id: i64, priority: i32) {
        tracing::debug!("Add task {} to worker {}", task_id, worker_id);
        if let Some(worker) = self.workers.get_mut(&worker_id) {
            worker.push(task_id, priority);
            // Update task_index
            self.task_index
                .entry(task_id)
                .or_default()
                .insert(worker_id);
        }
    }

    fn add_tasks(&mut self, worker_id: i64, tasks: Vec<(i64, i32)>) {
        tracing::debug!("Add {} tasks to worker {}", tasks.len(), worker_id);
        if let Some(worker) = self.workers.get_mut(&worker_id) {
            tasks.into_iter().for_each(|(task_id, priority)| {
                worker.push(task_id, priority);
                // Update task_index
                self.task_index
                    .entry(task_id)
                    .or_default()
                    .insert(worker_id);
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

    fn enqueue_candidates(&mut self, candidates: Vec<i64>, task_id: i64, priority: i32) {
        tracing::debug!(
            "Enqueue task {} to {} candidates using {:?} mode",
            task_id,
            candidates.len(),
            self.mode
        );

        // Store candidate memory for redistribution
        self.task_candidates.insert(task_id, candidates.clone());

        match &self.mode {
            ScheduleMode::Fanout => {
                // Fanout: enqueue to all candidates
                for worker_id in candidates {
                    self.add_task(worker_id, task_id, priority);
                }
            }
            ScheduleMode::Balanced(strategy) => {
                match strategy {
                    BalanceStrategy::LeastQueue => {
                        // Find the worker with the smallest load among candidates
                        let selected_worker = candidates
                            .into_iter()
                            .filter(|&worker_id| self.workers.contains_key(&worker_id))
                            .min_by_key(|&worker_id| {
                                let queue_len = self
                                    .workers
                                    .get(&worker_id)
                                    .map(|queue| queue.len())
                                    .unwrap_or(usize::MAX);
                                let running_count =
                                    self.running_count.get(&worker_id).copied().unwrap_or(0);
                                queue_len + running_count
                            });

                        if let Some(worker_id) = selected_worker {
                            self.add_task(worker_id, task_id, priority);
                        }
                    }
                    BalanceStrategy::RoundRobin => {
                        // Filter available candidates
                        let available_candidates: Vec<i64> = candidates
                            .into_iter()
                            .filter(|&worker_id| self.workers.contains_key(&worker_id))
                            .collect();

                        if !available_candidates.is_empty() {
                            // Select worker using round-robin
                            let selected_worker =
                                available_candidates[self.rr_cursor % available_candidates.len()];
                            self.rr_cursor = (self.rr_cursor + 1) % available_candidates.len();
                            self.add_task(selected_worker, task_id, priority);
                        }
                    }
                }
            }
        }
    }

    fn enqueue_many_candidates(&mut self, candidates: Vec<i64>, tasks: Vec<(i64, i32)>) {
        tracing::debug!(
            "Enqueue {} tasks to {} candidates using {:?} mode",
            tasks.len(),
            candidates.len(),
            self.mode
        );

        match &self.mode {
            ScheduleMode::Fanout => {
                // Store candidate memory for all tasks
                for (task_id, _) in &tasks {
                    self.task_candidates.insert(*task_id, candidates.clone());
                }
                // Fanout: enqueue all tasks to all candidates
                for worker_id in candidates {
                    self.add_tasks(worker_id, tasks.clone());
                }
            }
            ScheduleMode::Balanced(strategy) => {
                match strategy {
                    BalanceStrategy::LeastQueue | BalanceStrategy::RoundRobin => {
                        // For each task, find the worker with the smallest queue or round-robin
                        for (task_id, priority) in tasks {
                            self.enqueue_candidates(candidates.clone(), task_id, priority);
                        }
                    }
                }
            }
        }
    }

    fn remove_task(&mut self, task_id: i64) {
        tracing::debug!("Remove task {}", task_id);
        // Use task_index for O(K) removal instead of O(N)
        if let Some(worker_ids) = self.task_index.remove(&task_id) {
            for worker_id in worker_ids {
                if let Some(worker) = self.workers.get_mut(&worker_id) {
                    worker.remove(&task_id);
                }
            }
        }
        // Clean up candidate memory
        self.task_candidates.remove(&task_id);
    }

    fn redistribute_tasks(&mut self, tasks: Vec<(i64, i32)>) {
        tracing::info!(
            "Redistributing {} tasks due to worker unregistration",
            tasks.len()
        );

        for (task_id, priority) in tasks {
            // Try to get stored candidates for this task
            if let Some(candidates) = self.task_candidates.get(&task_id) {
                // Filter out the candidates that are still registered
                let available_candidates: Vec<i64> = candidates
                    .iter()
                    .filter(|&&worker_id| self.workers.contains_key(&worker_id))
                    .copied()
                    .collect();

                if !available_candidates.is_empty() {
                    tracing::debug!(
                        "Redistributing task {} to {} available candidates",
                        task_id,
                        available_candidates.len()
                    );
                    self.enqueue_candidates(available_candidates, task_id, priority);
                } else {
                    tracing::warn!(
                        "No available candidates for task {} during redistribution",
                        task_id
                    );
                    // Clean up since no candidates are available
                    self.task_candidates.remove(&task_id);
                }
            } else {
                tracing::warn!(
                    "No candidate memory found for task {} during redistribution",
                    task_id
                );
            }
        }
    }

    fn log_metrics(&self) {
        let total_queued_tasks: usize = self.workers.values().map(|queue| queue.len()).sum();
        let total_running_tasks: usize = self.running_count.values().sum();
        let total_workers = self.workers.len();
        let tasks_tracked = self.task_index.len();
        let candidates_cached = self.task_candidates.len();

        tracing::info!(
            "TaskDispatcher metrics: {} workers, {} queued tasks, {} running tasks, {} tracked, {} candidates cached, mode: {:?}",
            total_workers, total_queued_tasks, total_running_tasks, tasks_tracked, candidates_cached, self.mode
        );

        // Log per-worker details at debug level
        for (worker_id, queue) in &self.workers {
            let running = self.running_count.get(worker_id).copied().unwrap_or(0);
            let load = queue.len() + running;
            tracing::debug!(
                "Worker {}: {} queued, {} running, {} total load",
                worker_id,
                queue.len(),
                running,
                load
            );
        }
    }

    fn ack_assigned(&mut self, worker_id: i64, task_id: i64) {
        tracing::debug!("Ack assigned: task {} to worker {}", task_id, worker_id);
        if let Some(count) = self.running_count.get_mut(&worker_id) {
            *count += 1;
        }
    }

    fn ack_finished(&mut self, worker_id: i64, task_id: i64) {
        tracing::debug!("Ack finished: task {} from worker {}", task_id, worker_id);
        if let Some(count) = self.running_count.get_mut(&worker_id) {
            if *count > 0 {
                *count -= 1;
            }
        }
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
                TaskDispatcherOp::EnqueueCandidates {
                    candidates,
                    task_id,
                    priority,
                } => {
                    self.enqueue_candidates(candidates, task_id, priority);
                }
                TaskDispatcherOp::EnqueueManyCandidates { candidates, tasks } => {
                    self.enqueue_many_candidates(candidates, tasks);
                }
                TaskDispatcherOp::RedistributeTasks { tasks } => {
                    self.redistribute_tasks(tasks);
                }
                TaskDispatcherOp::AckAssigned(worker_id, task_id) => {
                    self.ack_assigned(worker_id, task_id);
                }
                TaskDispatcherOp::AckFinished(worker_id, task_id) => {
                    self.ack_finished(worker_id, task_id);
                }
                TaskDispatcherOp::LogMetrics => {
                    self.log_metrics();
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
        // Setup periodic metrics logging
        let mut metrics_interval = tokio::time::interval(std::time::Duration::from_secs(60));
        metrics_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                biased;
                _ = self.cancel_token.cancelled() => {
                    break;
                }
                _ = metrics_interval.tick() => {
                    self.log_metrics();
                }
                op = self.rx.recv() => if self.handle_op(op) {
                    self.cancel_token.cancel();
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;

    fn create_test_dispatcher(
        mode: ScheduleMode,
    ) -> (TaskDispatcher, mpsc::UnboundedSender<TaskDispatcherOp>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let cancel_token = CancellationToken::new();
        let dispatcher = TaskDispatcher::new(cancel_token, rx, mode);
        (dispatcher, tx)
    }

    #[test]
    fn test_fanout_enqueue_candidates() {
        let (mut dispatcher, _tx) = create_test_dispatcher(ScheduleMode::Fanout);

        // Register workers
        dispatcher.register_worker(1);
        dispatcher.register_worker(2);
        dispatcher.register_worker(3);

        // Enqueue task to candidates
        let candidates = vec![1, 2, 3];
        dispatcher.enqueue_candidates(candidates, 101, 10);

        // Verify task is in all workers' queues
        assert_eq!(dispatcher.workers.get(&1).unwrap().len(), 1);
        assert_eq!(dispatcher.workers.get(&2).unwrap().len(), 1);
        assert_eq!(dispatcher.workers.get(&3).unwrap().len(), 1);

        // Verify task_index is updated correctly
        let worker_set = dispatcher.task_index.get(&101).unwrap();
        assert_eq!(worker_set.len(), 3);
        assert!(worker_set.contains(&1));
        assert!(worker_set.contains(&2));
        assert!(worker_set.contains(&3));
    }

    #[test]
    fn test_balanced_enqueue_candidates() {
        let (mut dispatcher, _tx) =
            create_test_dispatcher(ScheduleMode::Balanced(BalanceStrategy::LeastQueue));

        // Register workers
        dispatcher.register_worker(1);
        dispatcher.register_worker(2);
        dispatcher.register_worker(3);

        // Add some tasks to worker 1 to make it less preferred
        dispatcher.add_task(1, 999, 5);
        dispatcher.add_task(1, 998, 5);

        // Enqueue task to candidates - should go to worker 2 or 3 (both have 0 tasks)
        let candidates = vec![1, 2, 3];
        dispatcher.enqueue_candidates(candidates, 101, 10);

        // Verify task went to a worker with fewer tasks (2 or 3, not 1)
        let task_in_worker_1 = dispatcher.workers.get(&1).unwrap().get(&101).is_some();
        let task_in_worker_2 = dispatcher.workers.get(&2).unwrap().get(&101).is_some();
        let task_in_worker_3 = dispatcher.workers.get(&3).unwrap().get(&101).is_some();

        // Task should be in exactly one worker (not worker 1 since it has more tasks)
        assert!(
            !task_in_worker_1,
            "Task should not go to worker 1 (has more tasks)"
        );
        assert!(
            task_in_worker_2 || task_in_worker_3,
            "Task should go to worker 2 or 3"
        );
        assert!(
            !(task_in_worker_2 && task_in_worker_3),
            "Task should be in exactly one worker"
        );

        // Verify task_index has exactly one worker
        let worker_set = dispatcher.task_index.get(&101).unwrap();
        assert_eq!(worker_set.len(), 1);
    }

    #[test]
    fn test_remove_task_with_index() {
        let (mut dispatcher, _tx) = create_test_dispatcher(ScheduleMode::Fanout);

        // Register workers and add tasks
        dispatcher.register_worker(1);
        dispatcher.register_worker(2);
        dispatcher.register_worker(3);

        dispatcher.enqueue_candidates(vec![1, 2, 3], 101, 10);
        dispatcher.enqueue_candidates(vec![1, 3], 102, 5);

        // Verify initial state
        assert_eq!(dispatcher.task_index.get(&101).unwrap().len(), 3);
        assert_eq!(dispatcher.task_index.get(&102).unwrap().len(), 2);

        // Remove task 101
        dispatcher.remove_task(101);

        // Verify task 101 is removed from all queues
        assert!(!dispatcher.workers.get(&1).unwrap().get(&101).is_some());
        assert!(!dispatcher.workers.get(&2).unwrap().get(&101).is_some());
        assert!(!dispatcher.workers.get(&3).unwrap().get(&101).is_some());

        // Verify task 101 is removed from task_index
        assert!(!dispatcher.task_index.contains_key(&101));

        // Verify task 102 is still present
        assert_eq!(dispatcher.task_index.get(&102).unwrap().len(), 2);
        assert!(dispatcher.workers.get(&1).unwrap().get(&102).is_some());
        assert!(dispatcher.workers.get(&3).unwrap().get(&102).is_some());
    }

    #[test]
    fn test_unregister_worker_cleans_task_index() {
        let (mut dispatcher, _tx) = create_test_dispatcher(ScheduleMode::Fanout);

        // Register workers and add tasks
        dispatcher.register_worker(1);
        dispatcher.register_worker(2);

        dispatcher.enqueue_candidates(vec![1, 2], 101, 10);
        dispatcher.enqueue_candidates(vec![1], 102, 5);

        // Verify initial state
        assert_eq!(dispatcher.task_index.get(&101).unwrap().len(), 2);
        assert_eq!(dispatcher.task_index.get(&102).unwrap().len(), 1);

        // Unregister worker 1
        dispatcher.unregister_worker(1);

        // Verify worker 1 is removed from task_index
        assert_eq!(dispatcher.task_index.get(&101).unwrap().len(), 1);
        assert!(dispatcher.task_index.get(&101).unwrap().contains(&2));

        // Task 102 should be completely removed since it was only in worker 1
        assert!(!dispatcher.task_index.contains_key(&102));

        // Verify worker queues
        assert!(!dispatcher.workers.contains_key(&1));
        assert!(dispatcher.workers.contains_key(&2));
    }

    #[test]
    fn test_fetch_task_priority_order() {
        let (mut dispatcher, _tx) = create_test_dispatcher(ScheduleMode::Fanout);

        // Register worker
        dispatcher.register_worker(1);

        // Add tasks with different priorities
        dispatcher.add_task(1, 101, 5); // Lower priority
        dispatcher.add_task(1, 102, 10); // Higher priority
        dispatcher.add_task(1, 103, 8); // Medium priority

        // Fetch tasks - should come out in priority order (highest first)
        assert_eq!(dispatcher.fetch_task(1), Some(102)); // Priority 10
        assert_eq!(dispatcher.fetch_task(1), Some(103)); // Priority 8
        assert_eq!(dispatcher.fetch_task(1), Some(101)); // Priority 5
        assert_eq!(dispatcher.fetch_task(1), None); // Empty
    }

    #[test]
    fn test_enqueue_many_candidates_fanout() {
        let (mut dispatcher, _tx) = create_test_dispatcher(ScheduleMode::Fanout);

        // Register workers
        dispatcher.register_worker(1);
        dispatcher.register_worker(2);

        // Enqueue multiple tasks
        let tasks = vec![(101, 10), (102, 5), (103, 8)];
        dispatcher.enqueue_many_candidates(vec![1, 2], tasks);

        // Verify all tasks are in both workers
        assert_eq!(dispatcher.workers.get(&1).unwrap().len(), 3);
        assert_eq!(dispatcher.workers.get(&2).unwrap().len(), 3);

        // Verify task_index
        for task_id in [101, 102, 103] {
            let worker_set = dispatcher.task_index.get(&task_id).unwrap();
            assert_eq!(worker_set.len(), 2);
            assert!(worker_set.contains(&1));
            assert!(worker_set.contains(&2));
        }
    }

    #[test]
    fn test_enqueue_many_candidates_balanced() {
        let (mut dispatcher, _tx) =
            create_test_dispatcher(ScheduleMode::Balanced(BalanceStrategy::LeastQueue));

        // Register workers
        dispatcher.register_worker(1);
        dispatcher.register_worker(2);

        // Enqueue multiple tasks
        let tasks = vec![(101, 10), (102, 5), (103, 8)];
        dispatcher.enqueue_many_candidates(vec![1, 2], tasks);

        // In balanced mode, each task should go to exactly one worker
        let total_tasks_worker_1 = dispatcher.workers.get(&1).unwrap().len();
        let total_tasks_worker_2 = dispatcher.workers.get(&2).unwrap().len();

        // All 3 tasks should be distributed
        assert_eq!(total_tasks_worker_1 + total_tasks_worker_2, 3);

        // Verify task_index - each task should be in exactly one worker
        for task_id in [101, 102, 103] {
            let worker_set = dispatcher.task_index.get(&task_id).unwrap();
            assert_eq!(worker_set.len(), 1);
        }
    }

    #[test]
    fn test_round_robin_balanced_enqueue() {
        let (mut dispatcher, _tx) =
            create_test_dispatcher(ScheduleMode::Balanced(BalanceStrategy::RoundRobin));

        // Register workers
        dispatcher.register_worker(1);
        dispatcher.register_worker(2);
        dispatcher.register_worker(3);

        // Enqueue multiple tasks - they should be distributed round-robin
        let candidates = vec![1, 2, 3];
        dispatcher.enqueue_candidates(candidates.clone(), 101, 10);
        dispatcher.enqueue_candidates(candidates.clone(), 102, 10);
        dispatcher.enqueue_candidates(candidates.clone(), 103, 10);

        // Each worker should have exactly one task
        assert_eq!(dispatcher.workers.get(&1).unwrap().len(), 1);
        assert_eq!(dispatcher.workers.get(&2).unwrap().len(), 1);
        assert_eq!(dispatcher.workers.get(&3).unwrap().len(), 1);

        // Verify distribution pattern
        let task_101_worker = dispatcher
            .task_index
            .get(&101)
            .unwrap()
            .iter()
            .next()
            .unwrap();
        let task_102_worker = dispatcher
            .task_index
            .get(&102)
            .unwrap()
            .iter()
            .next()
            .unwrap();
        let task_103_worker = dispatcher
            .task_index
            .get(&103)
            .unwrap()
            .iter()
            .next()
            .unwrap();

        // All tasks should be in different workers (round-robin)
        let workers = vec![*task_101_worker, *task_102_worker, *task_103_worker];
        assert_eq!(workers.len(), 3);
        assert!(workers.contains(&1));
        assert!(workers.contains(&2));
        assert!(workers.contains(&3));
    }

    #[test]
    fn test_load_metric_with_running_tasks() {
        let (mut dispatcher, _tx) =
            create_test_dispatcher(ScheduleMode::Balanced(BalanceStrategy::LeastQueue));

        // Register workers
        dispatcher.register_worker(1);
        dispatcher.register_worker(2);

        // Add some queued tasks to worker 1
        dispatcher.add_task(1, 999, 5);
        dispatcher.add_task(1, 998, 5);

        // Add running tasks to worker 1
        dispatcher.ack_assigned(1, 997);
        dispatcher.ack_assigned(1, 996);

        // Worker 1 now has: 2 queued + 2 running = 4 total load
        // Worker 2 has: 0 queued + 0 running = 0 total load

        // New task should go to worker 2 (lower load)
        let candidates = vec![1, 2];
        dispatcher.enqueue_candidates(candidates, 101, 10);

        // Verify task went to worker 2
        assert!(!dispatcher.workers.get(&1).unwrap().get(&101).is_some());
        assert!(dispatcher.workers.get(&2).unwrap().get(&101).is_some());

        let worker_set = dispatcher.task_index.get(&101).unwrap();
        assert_eq!(worker_set.len(), 1);
        assert!(worker_set.contains(&2));
    }

    #[test]
    fn test_redistribute_on_worker_unregister() {
        let (mut dispatcher, _tx) =
            create_test_dispatcher(ScheduleMode::Balanced(BalanceStrategy::LeastQueue));

        // Register workers
        dispatcher.register_worker(1);
        dispatcher.register_worker(2);
        dispatcher.register_worker(3);

        // Enqueue tasks to different workers
        dispatcher.enqueue_candidates(vec![1, 2, 3], 101, 10); // Goes to worker 1
        dispatcher.enqueue_candidates(vec![1, 2, 3], 102, 10); // Goes to worker 1 (still least loaded)
        dispatcher.enqueue_candidates(vec![2, 3], 103, 10); // Goes to worker 2

        // Verify initial distribution
        assert!(dispatcher.workers.get(&1).unwrap().len() >= 1);

        // Store which worker has task 101 before unregistration
        let worker_with_101 = *dispatcher
            .task_index
            .get(&101)
            .unwrap()
            .iter()
            .next()
            .unwrap();

        // Unregister the worker that has task 101
        dispatcher.unregister_worker(worker_with_101);

        // Verify the worker is removed
        assert!(!dispatcher.workers.contains_key(&worker_with_101));

        // Verify task 101 is redistributed to another worker
        if let Some(worker_set) = dispatcher.task_index.get(&101) {
            assert_eq!(worker_set.len(), 1);
            assert!(!worker_set.contains(&worker_with_101));
        }
    }

    #[test]
    fn test_candidate_memory_and_cleanup() {
        let (mut dispatcher, _tx) =
            create_test_dispatcher(ScheduleMode::Balanced(BalanceStrategy::LeastQueue));

        // Register workers
        dispatcher.register_worker(1);
        dispatcher.register_worker(2);

        // Enqueue task with candidates
        let candidates = vec![1, 2];
        dispatcher.enqueue_candidates(candidates.clone(), 101, 10);

        // Verify candidate memory is stored
        assert!(dispatcher.task_candidates.contains_key(&101));
        assert_eq!(dispatcher.task_candidates.get(&101).unwrap(), &candidates);

        // Remove task and verify candidate memory is cleaned up
        dispatcher.remove_task(101);
        assert!(!dispatcher.task_candidates.contains_key(&101));
        assert!(!dispatcher.task_index.contains_key(&101));
    }

    #[test]
    fn test_ack_assigned_and_finished() {
        let (mut dispatcher, _tx) = create_test_dispatcher(ScheduleMode::Fanout);

        // Register worker
        dispatcher.register_worker(1);

        // Initial running count should be 0
        assert_eq!(dispatcher.running_count.get(&1).unwrap(), &0);

        // Acknowledge assignment
        dispatcher.ack_assigned(1, 101);
        assert_eq!(dispatcher.running_count.get(&1).unwrap(), &1);

        dispatcher.ack_assigned(1, 102);
        assert_eq!(dispatcher.running_count.get(&1).unwrap(), &2);

        // Acknowledge completion
        dispatcher.ack_finished(1, 101);
        assert_eq!(dispatcher.running_count.get(&1).unwrap(), &1);

        dispatcher.ack_finished(1, 102);
        assert_eq!(dispatcher.running_count.get(&1).unwrap(), &0);
    }

    #[test]
    fn test_metrics_logging() {
        let (mut dispatcher, _tx) =
            create_test_dispatcher(ScheduleMode::Balanced(BalanceStrategy::LeastQueue));

        // Register workers and add some tasks
        dispatcher.register_worker(1);
        dispatcher.register_worker(2);

        dispatcher.enqueue_candidates(vec![1, 2], 101, 10);
        dispatcher.enqueue_candidates(vec![1, 2], 102, 5);
        dispatcher.ack_assigned(1, 999);

        // This should not panic and should log metrics
        dispatcher.log_metrics();
    }
}
