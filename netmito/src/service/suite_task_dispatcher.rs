//! Suite task dispatcher for managing per-suite in-memory task queues.
//!
//! This actor manages priority queues for each active task suite, providing
//! O(active_suites × buffer_size) memory instead of the O(workers × tasks)
//! overhead of per-worker queues.
//!
//! ## Design
//!
//! The dispatcher holds a `HashMap<suite_id, SuiteTaskBuffer>`. Each buffer is
//! a priority queue of task IDs ordered by task priority (max-heap). Agents
//! ask for a batch of task IDs; the dispatcher pops them and signals whether a
//! DB refill is needed. The caller (HTTP handler) is responsible for:
//!
//! 1. Atomically claiming each task in the DB (`state=Ready → Running`).
//! 2. Triggering a background DB refill when `needs_refill == true`.
//! 3. Sending `AddRefillTasks` + `RefillDone` when the refill query completes.
//!
//! Stale entries (tasks cancelled by users while sitting in the buffer) are
//! discarded naturally: the atomic DB update will find `state ≠ Ready` and
//! return `rows_affected = 0`, so the caller simply skips them.

use std::collections::HashMap;

use priority_queue::PriorityQueue;
use tokio::sync::oneshot::Sender;
use tokio_util::sync::CancellationToken;

/// Number of tasks remaining in buffer that triggers a proactive refill.
const DEFAULT_REFILL_THRESHOLD: u32 = 16;

/// Maximum number of tasks fetched per refill query.
const DEFAULT_MAX_CAPACITY: u32 = 64;

struct SuiteTaskBuffer {
    buffer: PriorityQueue<i64, i32>,
    max_capacity: u32,
    refill_threshold: u32,
    /// True while a DB refill is in flight; prevents duplicate refills.
    is_refilling: bool,
}

impl SuiteTaskBuffer {
    fn new() -> Self {
        Self {
            buffer: PriorityQueue::new(),
            max_capacity: DEFAULT_MAX_CAPACITY,
            refill_threshold: DEFAULT_REFILL_THRESHOLD,
            is_refilling: false,
        }
    }

    /// Grow capacity based on an agent's batch size. Only grows, never shrinks.
    fn update_capacity(&mut self, batch_size: u32) {
        if batch_size > self.refill_threshold {
            self.refill_threshold = batch_size;
            self.max_capacity = batch_size * 3;
        }
    }
}

/// Operations that can be sent to the [`SuiteTaskDispatcher`] actor.
pub enum SuiteDispatcherOp {
    /// Add one task to a suite's buffer.
    /// Lazily creates the buffer if it does not yet exist.
    AddTask {
        suite_id: i64,
        task_id: i64,
        priority: i32,
    },

    /// Bulk-add tasks returned by a DB refill query.
    /// Lazily creates the buffer if it does not yet exist.
    AddRefillTasks {
        suite_id: i64,
        /// `(task_id, priority)` pairs, typically from
        /// `SELECT id, priority … ORDER BY priority DESC LIMIT max_capacity`.
        tasks: Vec<(i64, i32)>,
    },

    /// Signal that the in-flight refill for this suite has finished.
    /// Clears the `is_refilling` flag so the next fetch can trigger another.
    RefillDone { suite_id: i64 },

    /// Pop up to `max_count` task IDs from the suite's buffer.
    /// Sends a [`FetchResult`] back via the oneshot channel.
    FetchTasks {
        suite_id: i64,
        max_count: u32,
        tx: Sender<FetchResult>,
    },

    /// Expand the buffer capacity based on an agent's `batch_size`.
    /// Called when an agent accepts a suite so the buffer stays large enough.
    UpdateCapacity { suite_id: i64, batch_size: u32 },

    /// Release the buffer for a completed or cancelled suite.
    DropBuffer { suite_id: i64 },
}

/// Returned by a [`SuiteDispatcherOp::FetchTasks`] operation.
pub struct FetchResult {
    /// Task IDs popped from the buffer. May be fewer than `max_count`.
    pub task_ids: Vec<i64>,
    /// If `true`, the caller should spawn an async DB refill and then send
    /// `AddRefillTasks` + `RefillDone` back to the dispatcher.
    pub needs_refill: bool,
    /// How many rows to `LIMIT` in the refill query.
    pub max_capacity: u32,
}

/// Actor that manages in-memory task buffers for all active suites.
pub struct SuiteTaskDispatcher {
    suites: HashMap<i64, SuiteTaskBuffer>,
    cancel_token: CancellationToken,
    rx: crossfire::AsyncRx<SuiteDispatcherOp>,
}

impl SuiteTaskDispatcher {
    pub fn new(cancel_token: CancellationToken, rx: crossfire::AsyncRx<SuiteDispatcherOp>) -> Self {
        Self {
            suites: HashMap::new(),
            cancel_token,
            rx,
        }
    }

    // -------------------------------------------------------------------------
    // Operation handlers
    // -------------------------------------------------------------------------

    fn add_task(&mut self, suite_id: i64, task_id: i64, priority: i32) {
        let buffer = self.suites.entry(suite_id).or_insert_with(SuiteTaskBuffer::new);
        buffer.buffer.push(task_id, priority);
    }

    fn add_refill_tasks(&mut self, suite_id: i64, tasks: Vec<(i64, i32)>) {
        let buffer = self.suites.entry(suite_id).or_insert_with(SuiteTaskBuffer::new);
        for (task_id, priority) in tasks {
            buffer.buffer.push(task_id, priority);
        }
    }

    fn refill_done(&mut self, suite_id: i64) {
        if let Some(buffer) = self.suites.get_mut(&suite_id) {
            buffer.is_refilling = false;
        }
    }

    fn fetch_tasks(&mut self, suite_id: i64, max_count: u32, tx: Sender<FetchResult>) {
        let result = if let Some(buffer) = self.suites.get_mut(&suite_id) {
            let mut task_ids = Vec::with_capacity(max_count as usize);
            for _ in 0..max_count {
                match buffer.buffer.pop() {
                    Some((task_id, _priority)) => task_ids.push(task_id),
                    None => break,
                }
            }
            let needs_refill = !buffer.is_refilling
                && (buffer.buffer.len() as u32) < buffer.refill_threshold;
            if needs_refill {
                buffer.is_refilling = true;
            }
            FetchResult {
                task_ids,
                needs_refill,
                max_capacity: buffer.max_capacity,
            }
        } else {
            // Suite has no buffer (e.g., after coordinator restart).
            // Create an empty buffer and immediately request a refill.
            let mut buffer = SuiteTaskBuffer::new();
            buffer.is_refilling = true;
            self.suites.insert(suite_id, buffer);
            FetchResult {
                task_ids: vec![],
                needs_refill: true,
                max_capacity: DEFAULT_MAX_CAPACITY,
            }
        };

        if tx.send(result).is_err() && !self.cancel_token.is_cancelled() {
            tracing::warn!(
                suite_id = suite_id,
                "SuiteTaskDispatcher: FetchTasks receiver dropped"
            );
        }
    }

    fn update_capacity(&mut self, suite_id: i64, batch_size: u32) {
        let buffer = self.suites.entry(suite_id).or_insert_with(SuiteTaskBuffer::new);
        buffer.update_capacity(batch_size);
    }

    fn drop_buffer(&mut self, suite_id: i64) {
        self.suites.remove(&suite_id);
        tracing::debug!(suite_id = suite_id, "SuiteTaskDispatcher: dropped buffer");
    }

    fn handle_op(&mut self, op: SuiteDispatcherOp) {
        match op {
            SuiteDispatcherOp::AddTask {
                suite_id,
                task_id,
                priority,
            } => self.add_task(suite_id, task_id, priority),
            SuiteDispatcherOp::AddRefillTasks { suite_id, tasks } => {
                self.add_refill_tasks(suite_id, tasks)
            }
            SuiteDispatcherOp::RefillDone { suite_id } => self.refill_done(suite_id),
            SuiteDispatcherOp::FetchTasks {
                suite_id,
                max_count,
                tx,
            } => self.fetch_tasks(suite_id, max_count, tx),
            SuiteDispatcherOp::UpdateCapacity { suite_id, batch_size } => {
                self.update_capacity(suite_id, batch_size)
            }
            SuiteDispatcherOp::DropBuffer { suite_id } => self.drop_buffer(suite_id),
        }
    }

    // -------------------------------------------------------------------------
    // Actor run loop
    // -------------------------------------------------------------------------

    pub async fn run(&mut self) {
        tracing::info!("SuiteTaskDispatcher started");
        loop {
            tokio::select! {
                biased;
                _ = self.cancel_token.cancelled() => break,
                op = self.rx.recv() => match op.ok() {
                    None => break,
                    Some(op) => self.handle_op(op),
                }
            }
        }
        tracing::info!("SuiteTaskDispatcher stopped");
    }
}
