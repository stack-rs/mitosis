# Worker Manager Design Refinements - Analysis Document

**Version:** 0.5.0-draft
**Date:** 2025-11-05
**Purpose:** Detailed analysis of refinements before integrating into main design

---

## 1. Manager Selection for Task Groups

### Requirements

**Two Selection Methods:**
1. **User-Specified**: Explicit manager UUID list (add/remove operations)
2. **Tag-Based**: Automatic matching via task group tags + refresh operation

**Constraints:**
- Must respect group permissions (manager's groups ∩ task group's parent group)
- Task dispatcher per manager with priority queue of task groups
- Each task group has priority queue of tasks
- Manager can pre-fetch N tasks (defined in task group spec)

### Design

#### A. Manager Selection Storage

```sql
-- Explicit manager assignments
CREATE TABLE task_group_managers (
    id BIGSERIAL PRIMARY KEY,
    task_group_id BIGINT NOT NULL REFERENCES task_groups(id) ON DELETE CASCADE,
    manager_id BIGINT NOT NULL REFERENCES worker_managers(id) ON DELETE CASCADE,

    -- How this manager was selected
    selection_type INTEGER NOT NULL,  -- 0=User-specified, 1=Tag-matched

    -- For tag-matched, which tags matched
    matched_tags TEXT[],

    added_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    added_by_user_id BIGINT REFERENCES users(id),

    UNIQUE(task_group_id, manager_id)
);

CREATE INDEX idx_task_group_managers_task_group
    ON task_group_managers(task_group_id);

CREATE INDEX idx_task_group_managers_manager
    ON task_group_managers(manager_id);
```

#### B. Selection APIs

```rust
// 1. User-specified manager addition
POST /task-groups/{id}/managers
{
  "manager_ids": ["uuid1", "uuid2"],
  "selection_type": "user_specified"
}

// 2. Remove managers
DELETE /task-groups/{id}/managers
{
  "manager_ids": ["uuid1"]
}

// 3. Update tags (doesn't auto-refresh)
PUT /task-groups/{id}/tags
{
  "tags": ["gpu", "linux", "x86_64"]
}

// 4. Refresh tag-based matching
POST /task-groups/{id}/managers/refresh
Response: {
  "added_managers": [...],
  "removed_managers": [...],  // Tag-matched managers that no longer match
  "user_specified_kept": [...]  // User-specified managers are preserved
}
```

#### C. Selection Algorithm

```rust
impl Coordinator {
    async fn refresh_task_group_managers(
        &self,
        task_group_id: i64,
    ) -> Result<RefreshResult> {
        let task_group = self.db.get_task_group(task_group_id).await?;
        let parent_group_id = task_group.group_id;

        // Get all managers that:
        // 1. Have matching tags
        // 2. Belong to groups that task_group's parent group can access
        let eligible_managers = self.db.query_managers()
            .filter_tags_overlap(&task_group.tags)
            .filter_group_access(parent_group_id)
            .fetch_all()
            .await?;

        // Get current tag-matched managers
        let current_tag_matched = self.db
            .get_task_group_managers(task_group_id)
            .filter(|m| m.selection_type == SelectionType::TagMatched)
            .collect::<Vec<_>>();

        // Calculate diff
        let to_add = eligible_managers.iter()
            .filter(|m| !current_tag_matched.contains(m))
            .collect::<Vec<_>>();

        let to_remove = current_tag_matched.iter()
            .filter(|m| !eligible_managers.contains(m))
            .collect::<Vec<_>>();

        // Apply changes (preserve user-specified)
        for manager in to_add {
            self.db.add_task_group_manager(
                task_group_id,
                manager.id,
                SelectionType::TagMatched,
                &manager.tags.intersection(&task_group.tags).collect(),
            ).await?;
        }

        for manager in to_remove {
            self.db.remove_task_group_manager(
                task_group_id,
                manager.id,
                SelectionType::TagMatched,  // Only remove tag-matched
            ).await?;
        }

        Ok(RefreshResult { to_add, to_remove })
    }
}
```

#### D. Task Group Cancellation

```rust
// New API
POST /task-groups/{id}/cancel
{
  "reason": "User requested cancellation",
  "cancel_running_tasks": true  // New: for managed workers
}

impl Coordinator {
    async fn cancel_task_group(&self, task_group_id: i64, cancel_running: bool) -> Result<CancelStats> {
        let tasks = self.db.get_task_group_tasks(task_group_id).await?;

        let mut stats = CancelStats::default();

        for task in tasks {
            match task.state {
                TaskState::Ready | TaskState::Pending => {
                    // Standard cancellation (existing logic)
                    self.db.update_task_state(task.id, TaskState::Cancelled).await?;
                    stats.ready_cancelled += 1;
                }
                TaskState::Running if cancel_running => {
                    // NEW: Cancel running tasks for managed workers
                    if let Some(manager_id) = self.get_task_manager(task.id).await? {
                        // Send cancellation to manager via WebSocket
                        self.ws_manager.send_task_cancellation(
                            manager_id,
                            task.id,
                            "Task group cancelled",
                        ).await?;
                        stats.running_cancelled += 1;
                    } else {
                        // Independent worker - can't cancel running tasks
                        stats.running_not_cancellable += 1;
                    }
                }
                _ => {}
            }
        }

        // Mark task group as cancelled
        self.db.update_task_group_state(task_group_id, TaskGroupState::Cancelled).await?;

        Ok(stats)
    }
}

// Manager receives cancellation via WebSocket
impl Manager {
    async fn handle_task_cancellation(&mut self, task_id: i64, reason: &str) {
        // Find worker executing this task
        if let Some(worker) = self.find_worker_by_task(task_id) {
            // Send graceful shutdown to worker via IPC
            self.ipc_publisher.send(ControlMessage::CancelTask {
                task_id,
                graceful: true,
            }).await?;

            // Wait for worker to finish (with timeout)
            tokio::time::timeout(
                Duration::from_secs(30),
                worker.wait_for_completion(),
            ).await?;
        }

        // Report cancellation to coordinator
        self.coordinator_client.report_task_cancelled(task_id, reason).await?;
    }
}
```

#### E. Task Pre-fetching

```rust
pub struct WorkerSchedulePlan {
    pub worker_count: u32,
    pub cpu_binding: Option<CpuBinding>,
    pub task_prefetch_count: u32,  // NEW: How many tasks manager pre-fetches
}

// Manager maintains local task queue
struct Manager {
    local_task_queue: VecDeque<WorkerTaskResp>,
    prefetch_count: u32,
}

impl Manager {
    async fn maintain_task_buffer(&mut self) {
        // Keep buffer filled
        while self.local_task_queue.len() < self.prefetch_count as usize {
            match self.coordinator_client.fetch_task().await {
                Ok(Some(task)) => {
                    self.local_task_queue.push_back(task);
                }
                Ok(None) => break,  // No more tasks
                Err(e) => {
                    tracing::error!("Failed to prefetch task: {}", e);
                    break;
                }
            }
        }
    }

    async fn get_task_for_worker(&mut self, worker_id: u32) -> Option<WorkerTaskResp> {
        let task = self.local_task_queue.pop_front();

        // Async refill buffer
        let manager = self.clone();
        tokio::spawn(async move {
            manager.maintain_task_buffer().await;
        });

        task
    }
}
```

---

## 2. Communication Protocol Analysis

### Current Design
- User ↔ Coordinator: HTTP
- Coordinator ↔ Independent Worker: HTTP (polling)
- Coordinator ↔ Manager: HTTP (polling) **← PROPOSED: WebSocket**
- Manager ↔ Managed Worker: IPC (iceoryx2)

### Analysis: WebSocket vs HTTP for Manager-Coordinator

#### Comparison

| Aspect | HTTP (Polling) | WebSocket |
|--------|---------------|-----------|
| **Latency** | 50-200ms per request | 1-10ms (persistent) |
| **Overhead** | Full HTTP headers each request | Minimal frame overhead |
| **Server Push** | Not supported | Native support |
| **Complexity** | Simple | Moderate (reconnection, state) |
| **Scalability** | Poor (many connections) | Excellent (persistent) |
| **Firewall/Proxy** | Always works | May have issues |

#### Traffic Analysis

**Manager → Coordinator:**
- Heartbeat: Every 30s (low frequency)
- Task fetch: When worker needs task (high frequency with multiple workers)
- Task status: On completion (moderate frequency)
- Failure reports: On worker death (low frequency)

**Coordinator → Manager:**
- Task group assignment: When manager idle (low frequency, but latency-critical)
- Task cancellation: User-initiated (low frequency, latency-critical)
- Configuration updates: Rare

#### Recommendation: **WebSocket**

**Rationale:**
1. **Push-based task assignment**: Coordinator can immediately push task groups to idle managers (no polling delay)
2. **Lower latency for cancellations**: Immediate delivery when user cancels task group
3. **Reduced overhead**: With multiple workers (e.g., 16 workers), manager makes frequent task fetch requests - WebSocket eliminates HTTP overhead
4. **Better scalability**: 100 managers = 100 persistent connections vs thousands of HTTP requests/sec

**Trade-offs:**
- More complex implementation (need reconnection logic, heartbeat over WebSocket)
- Need WebSocket-aware load balancers (but most support it now)

### WebSocket Message Protocol

```rust
// Manager → Coordinator
enum ManagerMessage {
    Heartbeat {
        manager_id: Uuid,
        metrics: ManagerMetrics,
    },
    FetchTask {
        request_id: u64,
        worker_local_id: u32,
    },
    ReportTask {
        request_id: u64,
        task_id: i64,
        op: ReportTaskOp,
    },
    ReportFailure {
        task_id: i64,
        failure_count: u32,
        error_message: String,
    },
    AbortTask {
        task_id: i64,
        reason: String,
    },
}

// Coordinator → Manager
enum CoordinatorMessage {
    TaskGroupAssigned {
        task_group_id: i64,
        task_group_spec: TaskGroupSpec,
    },
    TaskAvailable {
        request_id: u64,
        task: Option<WorkerTaskResp>,
    },
    TaskReportAck {
        request_id: u64,
        success: bool,
        url: Option<String>,
    },
    CancelTask {
        task_id: i64,
        reason: String,
    },
    CancelTaskGroup {
        task_group_id: i64,
        reason: String,
    },
    ConfigUpdate {
        lease_duration: Option<Duration>,
        // ...
    },
}
```

### Communication Abstraction Layer

```rust
// Abstract trait for worker communication
#[async_trait]
trait WorkerComm: Send + Sync {
    async fn fetch_task(&mut self) -> Result<Option<WorkerTaskResp>>;
    async fn report_task(&mut self, task_id: i64, op: ReportTaskOp) -> Result<ReportTaskResp>;
    async fn send_heartbeat(&mut self) -> Result<()>;
}

// HTTP implementation (independent workers)
struct HttpWorkerComm {
    client: reqwest::Client,
    coordinator_url: String,
    token: String,
}

#[async_trait]
impl WorkerComm for HttpWorkerComm {
    async fn fetch_task(&mut self) -> Result<Option<WorkerTaskResp>> {
        let resp = self.client
            .get(format!("{}/workers/tasks", self.coordinator_url))
            .bearer_auth(&self.token)
            .send()
            .await?;

        if resp.status() == 204 {
            return Ok(None);
        }

        Ok(Some(resp.json().await?))
    }

    async fn report_task(&mut self, task_id: i64, op: ReportTaskOp) -> Result<ReportTaskResp> {
        let resp = self.client
            .post(format!("{}/workers/tasks", self.coordinator_url))
            .bearer_auth(&self.token)
            .json(&ReportTaskReq { id: task_id, op })
            .send()
            .await?;

        Ok(resp.json().await?)
    }

    async fn send_heartbeat(&mut self) -> Result<()> {
        self.client
            .post(format!("{}/workers/heartbeat", self.coordinator_url))
            .bearer_auth(&self.token)
            .send()
            .await?;
        Ok(())
    }
}

// IPC implementation (managed workers)
struct IpcWorkerComm {
    manager_id: Uuid,
    worker_local_id: u32,
    task_fetch_client: iceoryx2::Client<FetchTaskRequest, FetchTaskResponse>,
    task_report_client: iceoryx2::Client<ReportTaskRequest, ReportTaskResponse>,
    heartbeat_client: iceoryx2::Client<HeartbeatRequest, HeartbeatResponse>,
}

#[async_trait]
impl WorkerComm for IpcWorkerComm {
    async fn fetch_task(&mut self) -> Result<Option<WorkerTaskResp>> {
        let request = FetchTaskRequest {
            worker_local_id: self.worker_local_id,
            request_id: generate_request_id(),
        };

        let response = self.task_fetch_client
            .send_request(&request)
            .timeout(Duration::from_secs(5))?;

        Ok(response.task)
    }

    async fn report_task(&mut self, task_id: i64, op: ReportTaskOp) -> Result<ReportTaskResp> {
        let request = ReportTaskRequest {
            worker_local_id: self.worker_local_id,
            request_id: generate_request_id(),
            task_id,
            op,
        };

        let response = self.task_report_client
            .send_request(&request)
            .timeout(Duration::from_secs(5))?;

        Ok(ReportTaskResp {
            url: response.url,
        })
    }

    async fn send_heartbeat(&mut self) -> Result<()> {
        let request = HeartbeatRequest {
            worker_local_id: self.worker_local_id,
        };

        self.heartbeat_client
            .send_request(&request)
            .timeout(Duration::from_secs(5))?;

        Ok(())
    }
}

// Worker uses abstracted interface
struct Worker {
    comm: Box<dyn WorkerComm>,
    // ... other fields
}

impl Worker {
    async fn run(&mut self) -> Result<()> {
        loop {
            // Fetch task (same code for HTTP and IPC!)
            let task = self.comm.fetch_task().await?;

            if let Some(task) = task {
                // Execute task
                let result = self.execute_task(task).await?;

                // Report result (same code for HTTP and IPC!)
                self.comm.report_task(task.id, ReportTaskOp::Finish).await?;
                self.comm.report_task(task.id, ReportTaskOp::Commit(result)).await?;
            }

            // Heartbeat (same code for HTTP and IPC!)
            self.comm.send_heartbeat().await?;

            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    }
}
```

---

## 3. Worker Death Detection - Exit Status Analysis

### Requirements
- Distinguish between different failure types based on exit status/signal
- Use exit status to decide retry vs abort strategy

### Implementation

```rust
use nix::sys::wait::{waitpid, WaitStatus};
use nix::sys::signal::Signal;

#[derive(Debug, Clone)]
enum WorkerExitReason {
    // Clean exit
    Success,

    // Exit with non-zero code
    ExitCode(i32),

    // Killed by signal
    Signal(Signal),

    // Unknown/other
    Unknown,
}

impl WorkerExitReason {
    fn should_abort(&self, failure_count: u32) -> bool {
        match self {
            // Always abort on these signals (indicate serious issues)
            WorkerExitReason::Signal(Signal::SIGSEGV) |  // Segmentation fault
            WorkerExitReason::Signal(Signal::SIGILL) |   // Illegal instruction
            WorkerExitReason::Signal(Signal::SIGBUS) |   // Bus error
            WorkerExitReason::Signal(Signal::SIGFPE) => { // Floating point exception
                failure_count >= 2  // Abort faster for hardware issues
            }

            // Retry on resource issues but abort eventually
            WorkerExitReason::Signal(Signal::SIGKILL) |  // OOM killer
            WorkerExitReason::Signal(Signal::SIGABRT) => { // Abort signal
                failure_count >= 3
            }

            // Retry on exit codes (might be transient)
            WorkerExitReason::ExitCode(_) => {
                failure_count >= 3
            }

            // Never abort on success or user termination
            WorkerExitReason::Success |
            WorkerExitReason::Signal(Signal::SIGTERM) |
            WorkerExitReason::Signal(Signal::SIGINT) => false,

            _ => failure_count >= 3,
        }
    }

    fn error_message(&self) -> String {
        match self {
            WorkerExitReason::Success => "Worker exited successfully".to_string(),
            WorkerExitReason::ExitCode(code) => format!("Worker exited with code {}", code),
            WorkerExitReason::Signal(sig) => format!("Worker killed by signal: {:?}", sig),
            WorkerExitReason::Unknown => "Worker exit reason unknown".to_string(),
        }
    }
}

impl Manager {
    async fn monitor_worker(&mut self, worker: &mut WorkerProcess) -> Result<()> {
        // Wait for worker exit
        let pid = Pid::from_raw(worker.pid as i32);
        match waitpid(pid, None) {
            Ok(WaitStatus::Exited(_pid, exit_code)) => {
                let reason = if exit_code == 0 {
                    WorkerExitReason::Success
                } else {
                    WorkerExitReason::ExitCode(exit_code)
                };
                self.handle_worker_exit(worker, reason).await?;
            }
            Ok(WaitStatus::Signaled(_pid, signal, _core_dumped)) => {
                let reason = WorkerExitReason::Signal(signal);
                self.handle_worker_exit(worker, reason).await?;
            }
            _ => {
                let reason = WorkerExitReason::Unknown;
                self.handle_worker_exit(worker, reason).await?;
            }
        }

        Ok(())
    }

    async fn handle_worker_exit(
        &mut self,
        worker: &mut WorkerProcess,
        reason: WorkerExitReason,
    ) -> Result<()> {
        if let Some(task_id) = worker.current_task_id {
            // Record failure
            let failure_count = self.record_task_failure(
                task_id,
                worker.local_id,
                &reason.error_message(),
            ).await?;

            // Decide: abort or retry?
            if reason.should_abort(failure_count) {
                tracing::warn!(
                    task_id = task_id,
                    worker_id = worker.local_id,
                    reason = ?reason,
                    failure_count = failure_count,
                    "Aborting task due to repeated failures"
                );

                self.abort_task(
                    task_id,
                    &format!("Worker failed {} times: {}", failure_count, reason.error_message()),
                ).await?;
            } else {
                tracing::info!(
                    task_id = task_id,
                    worker_id = worker.local_id,
                    reason = ?reason,
                    failure_count = failure_count,
                    "Retrying task after worker failure"
                );

                // Respawn worker and retry task
                self.respawn_worker(worker.local_id).await?;
                // Task remains in queue for retry
            }
        }

        Ok(())
    }
}
```

### Heartbeat-Based Detection

For independent workers (no process monitoring):

```rust
impl Coordinator {
    async fn check_worker_heartbeats(&self) {
        let expired_workers = self.heartbeat_queue.get_expired_workers().await;

        for worker in expired_workers {
            if let Some(task_id) = worker.assigned_task_id {
                tracing::warn!(
                    worker_id = ?worker.id,
                    task_id = task_id,
                    "Worker heartbeat timeout - assuming dead"
                );

                // For independent workers, we can't know exit reason
                // Just mark for reassignment
                self.reassign_task(task_id).await?;
            }

            // Remove worker from active pool
            self.remove_worker(worker.id).await?;
        }
    }
}
```

---

## 4. Data Persistence Strategy

### Classification: DB vs Memory

#### Persistent (Database)

**Must survive restarts:**

| Data | Storage | Reason |
|------|---------|--------|
| Task groups | PostgreSQL | User-defined configuration |
| Tasks | PostgreSQL | Work queue |
| Task execution failures | PostgreSQL | Historical tracking, prevents retry loops |
| Manager registrations | PostgreSQL | Identity and authorization |
| Worker registrations (independent) | PostgreSQL | Active worker pool |
| Group/user permissions | PostgreSQL | Authorization |
| Lease assignments | PostgreSQL | Enables restart recovery |
| Task group state | PostgreSQL | Workflow state |

#### Volatile (Memory)

**Can be rebuilt on restart:**

| Data | Storage | Rebuild Strategy |
|------|---------|------------------|
| Manager WebSocket connections | Memory (HashMap) | Managers reconnect on startup |
| Local task queue (manager) | Memory (VecDeque) | Refetch from coordinator on startup |
| Worker process handles (managed) | Memory | Workers respawn on manager startup |
| IPC service handles | Memory | Recreate on manager startup |
| Metrics (fine-grained) | Memory → TimescaleDB | Batch flush every 30s, loss acceptable |
| In-flight requests | Memory | Retry on reconnection |
| Task dispatcher queues | Memory | Rebuilt from active_tasks table |
| Manager priority queues | Memory | Rebuilt from task_groups table + task_group_managers |

#### Hybrid (Memory + Periodic Flush)

| Data | Primary | Secondary | Sync Interval |
|------|---------|-----------|---------------|
| Metrics | Memory | TimescaleDB | 30 seconds |
| Failure tracker (manager-side) | Memory | PostgreSQL | On each failure |
| Lease expiry | PostgreSQL | Memory cache | On heartbeat |
| Task group pending count | PostgreSQL | Memory cache | On task state change |

### Rebuild Strategy

```rust
impl Manager {
    async fn recover_from_restart(&mut self) -> Result<()> {
        tracing::info!("Manager restarting, recovering state...");

        // 1. Reconnect to coordinator via WebSocket
        self.establish_websocket_connection().await?;

        // 2. Query assigned task group (if any)
        let assigned_group = self.coordinator_client
            .get_manager_assignment(self.manager_id)
            .await?;

        if let Some(task_group) = assigned_group {
            tracing::info!(
                task_group_id = task_group.id,
                "Recovering assignment to task group"
            );

            // 3. Check if environment still prepared
            if self.check_environment_prepared(&task_group).await? {
                // 4. Respawn workers
                self.spawn_workers(&task_group.worker_schedule).await?;

                // 5. Refill task buffer
                self.maintain_task_buffer().await;
            } else {
                // Environment lost, re-run preparation
                self.execute_env_preparation(&task_group).await?;
                self.spawn_workers(&task_group.worker_schedule).await?;
            }
        } else {
            tracing::info!("No task group assigned, entering idle state");
            self.state = ManagerState::Idle;
        }

        Ok(())
    }
}

impl Coordinator {
    async fn rebuild_runtime_state(&mut self) -> Result<()> {
        tracing::info!("Coordinator restarting, rebuilding runtime state...");

        // 1. Rebuild task dispatcher from database
        let active_tasks = self.db.get_all_active_tasks().await?;
        for task in active_tasks {
            if let Some(worker_id) = task.assigned_worker {
                self.task_dispatcher.assign_task(worker_id, task.id, task.priority);
            }
        }

        // 2. Rebuild manager queues
        let task_groups = self.db.get_all_task_groups().await?;
        for group in task_groups {
            let eligible_managers = self.db
                .get_task_group_managers(group.id)
                .await?;

            for manager_id in eligible_managers {
                self.manager_queue.assign_task_group(manager_id, group.id, group.priority);
            }
        }

        // 3. Rebuild heartbeat queue
        let workers = self.db.get_all_active_workers().await?;
        for worker in workers {
            self.heartbeat_queue.register(worker.id, worker.last_heartbeat);
        }

        // 4. Managers will reconnect via WebSocket automatically
        tracing::info!("Runtime state rebuilt, waiting for manager connections...");

        Ok(())
    }
}
```

---

## 5. Naming Review and Improvements

### Current Naming Issues

| Current Name | Issue | Suggested Alternative |
|--------------|-------|----------------------|
| `WorkerManager` | Ambiguous (managing workers or is it a worker that manages?) | `DeviceManager` or `ManagerNode` |
| `task_groups` | Generic | Keep as-is (clear in context) |
| `worker_schedule` | Could be clearer | `worker_allocation_plan` |
| `env_preparation` | Vague | `environment_setup_hook` |
| `env_cleanup` | Vague | `environment_teardown_hook` |
| `assigned_manager_id` | Unclear if manager or managed | `executing_manager_id` |
| `selection_type` | Too generic | `manager_assignment_method` |
| `ManagerMessage` vs `CoordinatorMessage` | Directional confusion | `ManagerToCoordinator` vs `CoordinatorToManager` |
| `WorkerComm` | Abbreviation | `WorkerCommunication` or `WorkerCommChannel` |
| `FetchTaskRequest` | Doesn't indicate direction | `WorkerTaskRequest` |
| `ReportTaskOp` | "Op" ambiguous | `TaskStatusUpdate` or `TaskReportOperation` |

### Proposed Naming Convention

**Entities:**
- `DeviceManager` / `ManagerNode`: The service managing workers on a device
- `ManagedWorker`: Worker launched by manager
- `IndependentWorker`: Worker registered directly with coordinator
- `TaskGroup`: Collection of tasks with shared lifecycle
- `WorkerAllocationPlan`: Spec for how many workers to spawn

**States:**
- `TaskGroupState`: OPEN / CLOSED / COMPLETE / CANCELLED
- `ManagerState`: IDLE / PREPARING / EXECUTING / CLEANUP
- `WorkerState`: IDLE / EXECUTING / CRASHED

**Messages:**
- Direction-explicit: `ManagerToCoordinator` / `CoordinatorToManager`
- Request/Response pairs: `TaskFetchRequest` / `TaskFetchResponse`

**Operations:**
- `TaskReportOperation`: Union of task report types
- `ManagerAssignmentMethod`: USER_SPECIFIED / TAG_MATCHED

---

## 6. Performance and Efficiency Analysis

### Bottleneck Analysis

#### Current Design Bottlenecks

1. **Task Fetching Latency**
   - **Issue**: HTTP polling every 5 seconds
   - **Impact**: Up to 5s delay before worker starts task
   - **Solution**: WebSocket push + pre-fetching

2. **Manager Assignment Delay**
   - **Issue**: Manager polls for task groups
   - **Impact**: Idle managers waste time polling
   - **Solution**: WebSocket push when task group becomes available

3. **Database Contention**
   - **Issue**: High-frequency task state updates
   - **Impact**: Lock contention on `active_tasks` table
   - **Solution**: Batch updates, use advisory locks

4. **Heartbeat Overhead**
   - **Issue**: Every heartbeat writes to DB
   - **Impact**: High DB write load with many managers/workers
   - **Solution**: In-memory heartbeat tracking, periodic flush

5. **Failure Tracking Queries**
   - **Issue**: Query `task_execution_failures` on every task assignment
   - **Impact**: Slows down task distribution
   - **Solution**: Cache failure lists in memory, TTL 60s

### Optimizations

#### A. Task Pre-fetching Pipeline

```
Manager has 16 workers, pre-fetch 32 tasks:

Old (polling):
Worker idle → Wait 5s → Poll → Get task (200ms) → Start execution
Total: 5.2s delay

New (pre-fetch + WebSocket):
Worker idle → Get task from local queue (1ms) → Start execution
Total: 1ms delay

Buffer maintenance async in background
```

#### B. Batch Database Updates

```rust
// Instead of updating on every task state change:
// Bad:
for task in tasks {
    db.update_task_state(task.id, TaskState::Running).await?;
}

// Good:
db.batch_update_task_states(&task_ids, TaskState::Running).await?;

// SQL:
UPDATE active_tasks
SET state = $1, updated_at = NOW()
WHERE id = ANY($2::bigint[]);
```

#### C. Efficient State Transitions

```sql
-- Use triggers for automatic state management
CREATE OR REPLACE FUNCTION update_task_group_pending_count()
RETURNS TRIGGER AS $$
BEGIN
    IF NEW.state != OLD.state THEN
        IF NEW.state = 'Running' THEN
            -- Task started, no change to pending count
        ELSIF NEW.state IN ('Completed', 'Failed', 'Cancelled') THEN
            -- Task finished, decrement pending count
            UPDATE task_groups
            SET pending_task_count = pending_task_count - 1
            WHERE id = NEW.task_group_id;
        END IF;
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER task_state_change_trigger
AFTER UPDATE OF state ON active_tasks
FOR EACH ROW
EXECUTE FUNCTION update_task_group_pending_count();
```

### User Experience Improvements

1. **Progress Tracking**
   - Add `tasks_completed` / `tasks_total` to task group API response
   - WebSocket push for real-time progress updates
   - Estimated completion time based on throughput

2. **Manager Health Dashboard**
   - Real-time WebSocket updates for manager metrics
   - Visualization of task group execution
   - Worker crash notifications

3. **Task Group Templates**
   - Save/reuse task group configurations
   - Quick create from template

4. **Dry-run Mode**
   - Validate task group configuration without execution
   - Check manager availability
   - Estimate resource requirements

---

## 7. Corner Cases and Edge Scenarios

### Comprehensive Corner Case Analysis

#### A. Network and Connectivity

| Scenario | Impact | Mitigation |
|----------|--------|------------|
| **WebSocket connection drops** | Manager can't receive tasks | Automatic reconnection with exponential backoff |
| **Coordinator restarts** | All WebSocket connections lost | Managers detect and reconnect, resume from DB state |
| **Network partition** | Manager isolated from coordinator | Manager continues executing buffered tasks, reconnects when network restored |
| **DNS failure** | Manager can't resolve coordinator | Retry with cached IP, fallback to configured IPs |
| **Certificate expiry** | TLS connections fail | Automatic certificate rotation, warning alerts |

#### B. Resource Exhaustion

| Scenario | Impact | Mitigation |
|----------|--------|------------|
| **Manager runs out of disk** | Task execution fails | Pre-execution disk space check, abort if insufficient |
| **Manager out of memory** | OOM killer terminates workers | Memory limits via cgroups, graceful degradation |
| **CPU saturation** | Tasks run slowly | CPU quota enforcement, task timeout detection |
| **Too many workers** | Resource contention | Validate worker_count against system capacity on task group start |
| **File descriptor exhaustion** | Can't spawn more workers | ulimit checks, fail gracefully with clear error |

#### C. Concurrent Operations

| Scenario | Impact | Mitigation |
|----------|--------|------------|
| **Task group cancelled during execution** | Workers mid-task | Graceful cancellation with timeout, force kill after 30s |
| **Manager assigned new group while cleaning up** | State confusion | Block new assignments until cleanup complete |
| **Multiple users update task group tags** | Race condition | Optimistic locking with `updated_at` versioning |
| **Task group deleted while manager executing** | Orphaned execution | Periodic validation, abort if group not found |
| **Manager lease expires during execution** | Lease conflict | Grace period (5 minutes) before forcible reassignment |

#### D. Data Consistency

| Scenario | Impact | Mitigation |
|----------|--------|------------|
| **Task marked complete but not in DB** | Lost work | Idempotent task reporting, coordinator deduplicates |
| **Worker reports task twice** | Duplicate completion | Task UUID + state machine prevents double-completion |
| **Manager crashes during env preparation** | Partial state | Store preparation checkpoints, validate on recovery |
| **Task group state mismatch (DB vs memory)** | Incorrect behavior | Periodic reconciliation, prioritize DB as source of truth |
| **Failure count mismatch** | Incorrect abort threshold | Atomic increment in DB, manager rebuilds from DB on restart |

#### E. Timing and Ordering

| Scenario | Impact | Mitigation |
|----------|--------|------------|
| **Task submitted after auto-close** | Task group reopens | Atomic check-and-update in submission API |
| **Manager fetches task for deleted group** | 404 error | Manager handles gracefully, releases group |
| **Worker spawns after cleanup started** | Zombie worker | Track cleanup state, kill late-spawning workers |
| **Heartbeat arrives after timeout** | Worker already removed | Idempotent heartbeat handling |
| **Lease renewal race with expiry** | Conflicting assignments | Use advisory locks for lease operations |

#### F. Failure Cascades

| Scenario | Impact | Mitigation |
|----------|--------|------------|
| **All managers fail same task** | Task unexecutable | Alert admin, provide failure diagnostics |
| **Coordinator database down** | System-wide outage | Managers buffer work in memory, retry with backoff |
| **S3 unavailable** | Can't fetch/upload artifacts | Task retry with exponential backoff, eventual timeout |
| **Redis down (for state tracking)** | Fallback to polling | Graceful degradation to polling mode |
| **High manager churn** | Constant reassignments | Rate-limit reassignments, increase lease duration |

#### G. Security and Authorization

| Scenario | Impact | Mitigation |
|----------|--------|------------|
| **JWT token expires mid-execution** | Authentication failure | Proactive token refresh before expiry |
| **User loses group permission** | Unauthorized access | Periodic permission validation |
| **Manager tries to execute unauthorized group** | Security violation | Authorization check on assignment |
| **Malicious manager reports false failures** | DoS attack | Rate-limit failure reports, alert on anomalies |
| **Task group with invalid tags** | No eligible managers | Validation on creation, return clear error |

#### H. Scalability Limits

| Scenario | Impact | Mitigation |
|----------|--------|------------|
| **1000 concurrent managers** | WebSocket connection limit | Connection pooling, load balancer scaling |
| **10M tasks in task group** | Memory exhaustion | Pagination, lazy loading, cursor-based queries |
| **100 workers per manager** | IPC handle exhaustion | Benchmark limits, document capacity |
| **Task group with 10K eligible managers** | Assignment algorithm slow | Limit eligible managers, prioritize by affinity |
| **High-frequency task submissions** | DB write saturation | Batch insert API, queue-based ingestion |

### RFC-Level Documentation Requirements

1. **Formal State Machines**
   - Task state transitions with guards and actions
   - Task group lifecycle with all transitions
   - Manager state machine with error transitions

2. **API Specifications**
   - OpenAPI 3.0 spec for all endpoints
   - WebSocket message schemas (JSON Schema)
   - Error code taxonomy

3. **Operational Runbook**
   - Deployment procedures
   - Monitoring and alerting setup
   - Incident response playbooks
   - Capacity planning guidelines

4. **Security Model**
   - Threat model
   - Authentication flows
   - Authorization matrix
   - Audit logging requirements

5. **Performance Benchmarks**
   - Expected throughput (tasks/sec)
   - Latency SLAs (P50, P95, P99)
   - Resource requirements (CPU, memory, network)
   - Scalability limits

6. **Testing Strategy**
   - Unit test coverage requirements
   - Integration test scenarios
   - Chaos engineering experiments
   - Load testing profiles

---

## Summary and Next Steps

### Key Refinements

1. ✅ **Manager Selection**: User-specified + tag-based with refresh API
2. ✅ **WebSocket Communication**: Coordinator ↔ Manager for low latency
3. ✅ **Communication Abstraction**: Code reuse between worker modes
4. ✅ **Exit Status Detection**: Smart abort logic based on failure type
5. ✅ **Persistence Strategy**: Clear DB vs memory classification
6. ✅ **Naming Improvements**: Direction-explicit, less ambiguous
7. ✅ **Performance Optimizations**: Pre-fetching, batching, caching
8. ✅ **Corner Cases**: Comprehensive 50+ scenarios documented

### Integration Plan

1. Update main `overview.md` with refined designs
2. Add new sections for WebSocket protocol and abstraction layer
3. Update database schema with new tables
4. Add corner case appendix
5. Create OpenAPI spec
6. Version bump to 0.5.0

### Open Questions for User

1. **WebSocket Library**: Use `tokio-tungstenite` or `axum` built-in WebSocket?
2. **Metrics Database**: TimescaleDB vs InfluxDB - preference?
3. **Task Pre-fetch Count**: Default 2x worker_count or configurable?
4. **Manager Reconnection**: Max retry attempts before marking offline?
5. **Naming Final Decision**: `DeviceManager` vs `ManagerNode` vs keep `WorkerManager`?

---

**Ready for integration into main design document pending user approval of these refinements.**
