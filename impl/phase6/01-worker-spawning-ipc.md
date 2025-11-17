# Phase 6: Worker Spawning and IPC Implementation Guide

## Overview

Phase 6 is the **most complex phase** of the Task Suite feature implementation. It focuses on implementing worker process management, iceoryx2-based IPC communication, and robust failure handling mechanisms in the Node Manager.

**Timeline:** 2 weeks

**Complexity:** This phase involves:
- Low-level process management (spawning, monitoring, CPU binding)
- Zero-copy IPC using iceoryx2
- Complex failure tracking and auto-recovery logic
- Task pre-fetching and buffering mechanisms
- Multi-threaded concurrent request handling

## Prerequisites

### Completed Phases
- **Phase 1:** Database schema (task_suites, node_managers, task_execution_failures tables)
- **Phase 2:** Coordinator APIs (suite creation, manager registration)
- **Phase 3:** WebSocket Manager (bidirectional communication)
- **Phase 4:** Suite Queue Manager (suite assignment logic)
- **Phase 5:** Manager lifecycle and environment hooks

### Required Knowledge
- **iceoryx2**: Inter-process communication framework
  - Request-response patterns
  - Pub-sub patterns
  - Service creation and discovery
- **Process Management**:
  - Spawning subprocesses
  - Monitoring process health
  - Signal handling (SIGTERM, SIGKILL, SIGSEGV, etc.)
  - Exit code analysis
- **CPU Binding**:
  - CPU affinity and core binding
  - Core allocation strategies
  - `libc::sched_setaffinity` or equivalent

### Understanding Required RFC Sections
- **Section 4.3:** Worker Modes (Independent vs Managed)
- **Section 5:** Architecture Design
- **Section 6.1:** Database Tables (task_execution_failures)
- **Section 8.2:** IPC Protocol (iceoryx2 services and messages)
- **Section 10.2:** Worker Death Handling
- **Section 10.4:** Task Pre-fetching

## Timeline

| Week | Focus Area | Deliverables |
|------|------------|-------------|
| **Week 1** | IPC Setup & Worker Spawning | - iceoryx2 services operational<br>- Worker spawning with CPU binding<br>- Basic monitoring |
| **Week 2** | Failure Handling & Pre-fetching | - Auto-recovery logic<br>- Failure tracking<br>- Task buffer management<br>- Full integration tests |

## Design References

### WorkerSchedulePlan Definition
```rust
pub struct WorkerSchedulePlan {
    pub worker_count: u32,                 // How many workers to spawn (1-256)
    pub cpu_binding: Option<CpuBinding>,   // CPU core allocation strategy
    pub task_prefetch_count: u32,          // Task buffer size (default: 16)
}
```

**Example JSON:**
```json
{
  "worker_count": 16,
  "cpu_binding": {
    "cores": [0, 1, 2, 3],
    "strategy": "RoundRobin"
  },
  "task_prefetch_count": 32
}
```

### CpuBinding Definition
```rust
pub struct CpuBinding {
    pub cores: Vec<usize>,                 // CPU core IDs to use
    pub strategy: CpuBindingStrategy,
}

pub enum CpuBindingStrategy {
    RoundRobin,    // Distribute workers across cores in round-robin fashion
    Exclusive,     // Each worker gets dedicated core(s)
    Shared,        // All workers share all cores
}
```

**Strategy Details:**
- **RoundRobin**: Worker N assigned to `cores[N % cores.len()]`
- **Exclusive**: Each worker gets `cores.len() / worker_count` cores
- **Shared**: All workers can run on any core in the list

### IPC Architecture (Section 8.2)

#### iceoryx2 Request-Response Services

**Service 1: task_fetch**
```rust
// Request from managed worker
#[derive(Serialize, Deserialize)]
struct FetchTaskRequest {
    worker_local_id: u32,     // Worker's ID within this manager (0-255)
    request_id: u64,          // For request tracking/debugging
}

// Response from manager
#[derive(Serialize, Deserialize)]
struct FetchTaskResponse {
    task: Option<WorkerTaskResp>,  // None if no tasks available
}
```

**Service 2: task_report**
```rust
// Request from managed worker
#[derive(Serialize, Deserialize)]
struct ReportTaskRequest {
    worker_local_id: u32,
    request_id: u64,
    task_id: i64,
    op: ReportTaskOp,  // StartTask, FinishTask, FailTask
}

// Response from manager
#[derive(Serialize, Deserialize)]
struct ReportTaskResponse {
    success: bool,
    url: Option<String>,  // Presigned S3 URL for uploads (on StartTask)
}
```

**Service 3: heartbeat (optional)**
```rust
#[derive(Serialize, Deserialize)]
struct HeartbeatRequest {
    worker_local_id: u32,
}

#[derive(Serialize, Deserialize)]
struct HeartbeatResponse {
    ack: bool,
}
```

#### iceoryx2 Pub-Sub Topics

**Topic: manager/control**
```rust
#[derive(Serialize, Deserialize)]
enum ControlMessage {
    Shutdown { graceful: bool },
    CancelTask { task_id: i64, graceful: bool },
    Pause,
    Resume,
}
```

**Topic: manager/config**
```rust
#[derive(Serialize, Deserialize)]
struct ConfigUpdate {
    poll_interval: Option<Duration>,
}
```

### Failure Tracking Schema

**Database Table: task_execution_failures**
```sql
CREATE TABLE task_execution_failures (
    id BIGSERIAL PRIMARY KEY,
    task_id BIGINT NOT NULL,
    task_uuid UUID NOT NULL,
    task_suite_id BIGINT REFERENCES task_suites(id) ON DELETE CASCADE,
    manager_id BIGINT NOT NULL REFERENCES node_managers(id) ON DELETE CASCADE,

    -- Failure tracking
    failure_count INTEGER NOT NULL DEFAULT 1,
    last_failure_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    error_messages TEXT[],  -- History of error messages
    worker_local_ids INTEGER[],  -- Which workers failed

    -- Metadata
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    UNIQUE(task_uuid, manager_id)
);

CREATE INDEX idx_task_failures_task_uuid ON task_execution_failures(task_uuid);
CREATE INDEX idx_task_failures_manager ON task_execution_failures(manager_id);
CREATE INDEX idx_task_failures_suite ON task_execution_failures(task_suite_id);
```

**Failure Logic:**
- Manager tracks failures per task UUID
- After 3 worker deaths on same task → abort task, report to coordinator
- Coordinator reassigns task to different manager (if available)

## Implementation Tasks

### Task 6.1: Setup iceoryx2 Services

**Objective:** Initialize iceoryx2 services for IPC between manager and managed workers.

#### Subtasks

**6.1.1: Create Service Definitions**

Location: `mitosis-manager/src/ipc/messages.rs`

```rust
use serde::{Serialize, Deserialize};
use uuid::Uuid;
use time::OffsetDateTime;

// Request-Response Messages
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct FetchTaskRequest {
    pub worker_local_id: u32,
    pub request_id: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct FetchTaskResponse {
    pub task: Option<WorkerTaskResp>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ReportTaskRequest {
    pub worker_local_id: u32,
    pub request_id: u64,
    pub task_id: i64,
    pub op: ReportTaskOp,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ReportTaskResponse {
    pub success: bool,
    pub url: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct HeartbeatRequest {
    pub worker_local_id: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct HeartbeatResponse {
    pub ack: bool,
}

// Pub-Sub Messages
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum ControlMessage {
    Shutdown { graceful: bool },
    CancelTask { task_id: i64, graceful: bool },
    Pause,
    Resume,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ConfigUpdate {
    pub poll_interval: Option<std::time::Duration>,
}
```

**6.1.2: Implement IPC Service Manager**

Location: `mitosis-manager/src/ipc/service.rs`

```rust
use iceoryx2::prelude::*;
use anyhow::Result;
use uuid::Uuid;

pub struct IpcServiceManager {
    manager_uuid: Uuid,
    service_name_prefix: String,

    // Request-response servers
    task_fetch_service: Option<IpcServer<FetchTaskRequest, FetchTaskResponse>>,
    task_report_service: Option<IpcServer<ReportTaskRequest, ReportTaskResponse>>,
    heartbeat_service: Option<IpcServer<HeartbeatRequest, HeartbeatResponse>>,

    // Pub-sub publishers
    control_publisher: Option<IpcPublisher<ControlMessage>>,
    config_publisher: Option<IpcPublisher<ConfigUpdate>>,
}

impl IpcServiceManager {
    pub fn new(manager_uuid: Uuid) -> Self {
        let service_name_prefix = format!("mitosis_mgr_{}", manager_uuid);

        Self {
            manager_uuid,
            service_name_prefix,
            task_fetch_service: None,
            task_report_service: None,
            heartbeat_service: None,
            control_publisher: None,
            config_publisher: None,
        }
    }

    pub async fn initialize(&mut self) -> Result<()> {
        // Create task_fetch request-response service
        let task_fetch_service_name = format!("{}_task_fetch", self.service_name_prefix);
        self.task_fetch_service = Some(
            ServiceBuilder::new(&task_fetch_service_name)
                .request_response::<FetchTaskRequest, FetchTaskResponse>()
                .create_server()?
        );

        // Create task_report request-response service
        let task_report_service_name = format!("{}_task_report", self.service_name_prefix);
        self.task_report_service = Some(
            ServiceBuilder::new(&task_report_service_name)
                .request_response::<ReportTaskRequest, ReportTaskResponse>()
                .create_server()?
        );

        // Create heartbeat request-response service (optional)
        let heartbeat_service_name = format!("{}_heartbeat", self.service_name_prefix);
        self.heartbeat_service = Some(
            ServiceBuilder::new(&heartbeat_service_name)
                .request_response::<HeartbeatRequest, HeartbeatResponse>()
                .create_server()?
        );

        // Create control pub-sub publisher
        let control_topic_name = format!("{}_control", self.service_name_prefix);
        self.control_publisher = Some(
            ServiceBuilder::new(&control_topic_name)
                .publish_subscribe::<ControlMessage>()
                .create_publisher()?
        );

        // Create config pub-sub publisher
        let config_topic_name = format!("{}_config", self.service_name_prefix);
        self.config_publisher = Some(
            ServiceBuilder::new(&config_topic_name)
                .publish_subscribe::<ConfigUpdate>()
                .create_publisher()?
        );

        tracing::info!(
            "IPC services initialized for manager {}",
            self.manager_uuid
        );

        Ok(())
    }

    pub async fn shutdown(&mut self) -> Result<()> {
        // Publish shutdown message
        if let Some(publisher) = &self.control_publisher {
            publisher.send(ControlMessage::Shutdown { graceful: true })?;
        }

        // Drop all services (closes iceoryx2 connections)
        self.task_fetch_service = None;
        self.task_report_service = None;
        self.heartbeat_service = None;
        self.control_publisher = None;
        self.config_publisher = None;

        tracing::info!("IPC services shut down for manager {}", self.manager_uuid);
        Ok(())
    }
}
```

**Testing:**
- [ ] Service creation succeeds with valid manager UUID
- [ ] Service names follow correct format: `mitosis_mgr_{uuid}_{service}`
- [ ] Services are discoverable by workers
- [ ] Cleanup properly destroys all services

---

### Task 6.2: Implement Worker Spawning

**Objective:** Spawn N managed workers as subprocesses with CPU binding and IPC configuration.

#### Subtasks

**6.2.1: Define Worker Process Tracker**

Location: `mitosis-manager/src/worker/process.rs`

```rust
use std::process::{Child, Command};
use uuid::Uuid;
use time::OffsetDateTime;
use nix::unistd::Pid;

#[derive(Debug, Clone)]
pub struct WorkerProcess {
    pub local_id: u32,              // Unique ID within this manager (0-255)
    pub pid: Option<Pid>,           // Process ID
    pub process_handle: Option<Child>, // Rust process handle
    pub cpu_cores: Vec<usize>,      // Assigned CPU cores
    pub started_at: OffsetDateTime,
    pub current_task_uuid: Option<Uuid>,  // Task currently executing
}

impl WorkerProcess {
    pub fn new(local_id: u32, cpu_cores: Vec<usize>) -> Self {
        Self {
            local_id,
            pid: None,
            process_handle: None,
            cpu_cores,
            started_at: OffsetDateTime::now_utc(),
            current_task_uuid: None,
        }
    }

    pub fn is_alive(&mut self) -> bool {
        if let Some(ref mut child) = self.process_handle {
            match child.try_wait() {
                Ok(Some(_)) => false,  // Process exited
                Ok(None) => true,      // Still running
                Err(_) => false,       // Error checking status
            }
        } else {
            false
        }
    }
}
```

**6.2.2: Implement Worker Spawner**

Location: `mitosis-manager/src/worker/spawner.rs`

```rust
use std::process::Command;
use anyhow::{Result, Context};
use uuid::Uuid;
use nix::sched::{sched_setaffinity, CpuSet};
use nix::unistd::Pid;

pub struct WorkerSpawner {
    manager_uuid: Uuid,
    worker_binary_path: String,
}

impl WorkerSpawner {
    pub fn new(manager_uuid: Uuid, worker_binary_path: String) -> Self {
        Self {
            manager_uuid,
            worker_binary_path,
        }
    }

    pub async fn spawn_worker(
        &self,
        local_id: u32,
        cpu_cores: Vec<usize>,
    ) -> Result<WorkerProcess> {
        let mut worker = WorkerProcess::new(local_id, cpu_cores.clone());

        // Build command: mitosis worker --managed --manager-uuid <uuid> --worker-id <id>
        let mut cmd = Command::new(&self.worker_binary_path);
        cmd.arg("worker")
            .arg("--managed")
            .arg("--manager-uuid")
            .arg(self.manager_uuid.to_string())
            .arg("--worker-id")
            .arg(local_id.to_string());

        // Spawn process
        let child = cmd.spawn()
            .context("Failed to spawn worker process")?;

        let pid = Pid::from_raw(child.id() as i32);

        // Apply CPU affinity
        if !cpu_cores.is_empty() {
            Self::bind_to_cores(pid, &cpu_cores)?;
        }

        worker.pid = Some(pid);
        worker.process_handle = Some(child);

        tracing::info!(
            "Spawned worker {} (PID: {}, cores: {:?})",
            local_id,
            pid,
            cpu_cores
        );

        Ok(worker)
    }

    fn bind_to_cores(pid: Pid, cores: &[usize]) -> Result<()> {
        let mut cpu_set = CpuSet::new();

        for &core in cores {
            cpu_set.set(core)
                .context(format!("Invalid CPU core: {}", core))?;
        }

        sched_setaffinity(pid, &cpu_set)
            .context("Failed to set CPU affinity")?;

        tracing::debug!("Bound process {} to cores {:?}", pid, cores);
        Ok(())
    }
}
```

**6.2.3: Implement CPU Binding Strategies**

Location: `mitosis-manager/src/worker/cpu_binding.rs`

```rust
use anyhow::{Result, bail};

#[derive(Debug, Clone)]
pub enum CpuBindingStrategy {
    RoundRobin,
    Exclusive,
    Shared,
}

#[derive(Debug, Clone)]
pub struct CpuBinding {
    pub cores: Vec<usize>,
    pub strategy: CpuBindingStrategy,
}

impl CpuBinding {
    /// Compute CPU cores for each worker based on strategy
    pub fn allocate_cores(&self, worker_count: u32) -> Result<Vec<Vec<usize>>> {
        if self.cores.is_empty() {
            bail!("CPU cores list cannot be empty");
        }

        let mut allocations = Vec::new();

        match self.strategy {
            CpuBindingStrategy::RoundRobin => {
                // Each worker gets one core, cycling through available cores
                for i in 0..worker_count {
                    let core_idx = (i as usize) % self.cores.len();
                    allocations.push(vec![self.cores[core_idx]]);
                }
            }

            CpuBindingStrategy::Exclusive => {
                // Divide cores equally among workers
                let cores_per_worker = self.cores.len() / worker_count as usize;

                if cores_per_worker == 0 {
                    bail!(
                        "Not enough cores ({}) for exclusive binding of {} workers",
                        self.cores.len(),
                        worker_count
                    );
                }

                for i in 0..worker_count {
                    let start_idx = (i as usize) * cores_per_worker;
                    let end_idx = if i == worker_count - 1 {
                        self.cores.len()  // Last worker gets remaining cores
                    } else {
                        start_idx + cores_per_worker
                    };

                    allocations.push(self.cores[start_idx..end_idx].to_vec());
                }
            }

            CpuBindingStrategy::Shared => {
                // All workers share all cores
                for _ in 0..worker_count {
                    allocations.push(self.cores.clone());
                }
            }
        }

        Ok(allocations)
    }
}
```

**6.2.4: Spawn Workers According to Schedule Plan**

Location: `mitosis-manager/src/worker/pool.rs`

```rust
use anyhow::Result;
use uuid::Uuid;
use std::collections::HashMap;

pub struct WorkerPool {
    manager_uuid: Uuid,
    spawner: WorkerSpawner,
    workers: HashMap<u32, WorkerProcess>,  // local_id -> WorkerProcess
}

impl WorkerPool {
    pub fn new(manager_uuid: Uuid, worker_binary_path: String) -> Self {
        Self {
            manager_uuid,
            spawner: WorkerSpawner::new(manager_uuid, worker_binary_path),
            workers: HashMap::new(),
        }
    }

    pub async fn spawn_workers(
        &mut self,
        schedule: &WorkerSchedulePlan,
    ) -> Result<()> {
        let worker_count = schedule.worker_count;

        // Compute CPU allocations
        let cpu_allocations = if let Some(binding) = &schedule.cpu_binding {
            binding.allocate_cores(worker_count)?
        } else {
            // No binding - empty core list for all workers
            vec![vec![]; worker_count as usize]
        };

        // Spawn each worker
        for local_id in 0..worker_count {
            let cpu_cores = cpu_allocations[local_id as usize].clone();

            let worker = self.spawner.spawn_worker(local_id, cpu_cores).await?;
            self.workers.insert(local_id, worker);
        }

        tracing::info!(
            "Spawned {} workers for manager {}",
            worker_count,
            self.manager_uuid
        );

        Ok(())
    }

    pub fn get_worker(&self, local_id: u32) -> Option<&WorkerProcess> {
        self.workers.get(&local_id)
    }

    pub fn get_worker_mut(&mut self, local_id: u32) -> Option<&mut WorkerProcess> {
        self.workers.get_mut(&local_id)
    }

    pub async fn shutdown_all(&mut self) -> Result<()> {
        for (local_id, worker) in self.workers.iter_mut() {
            if let Some(ref mut child) = worker.process_handle {
                let _ = child.kill();  // Send SIGKILL
                tracing::info!("Killed worker {}", local_id);
            }
        }

        self.workers.clear();
        Ok(())
    }
}
```

**Testing:**
- [ ] Spawn 1 worker successfully
- [ ] Spawn 16 workers successfully
- [ ] Spawn 256 workers (stress test)
- [ ] RoundRobin binding: workers cycle through cores
- [ ] Exclusive binding: each worker gets dedicated cores
- [ ] Shared binding: all workers share all cores
- [ ] Verify CPU affinity with `taskset -cp <pid>`
- [ ] Handle invalid core IDs gracefully

---

### Task 6.3: Implement Worker Monitoring and Auto-Recovery

**Objective:** Monitor worker process health, detect crashes, and automatically respawn failed workers.

#### Subtasks

**6.3.1: Define Exit Reason Classification**

Location: `mitosis-manager/src/worker/exit_status.rs`

```rust
use nix::sys::signal::Signal;

#[derive(Debug, Clone)]
pub enum WorkerExitReason {
    Success,
    ExitCode(i32),
    Signal(Signal),
    Unknown,
}

impl WorkerExitReason {
    /// Determine if task should be aborted based on exit reason and failure count
    pub fn should_abort(&self, failure_count: u32) -> bool {
        match self {
            // Hardware/memory errors - abort faster (after 2 failures)
            WorkerExitReason::Signal(Signal::SIGSEGV) |  // Segmentation fault
            WorkerExitReason::Signal(Signal::SIGILL) |   // Illegal instruction
            WorkerExitReason::Signal(Signal::SIGBUS) |   // Bus error
            WorkerExitReason::Signal(Signal::SIGFPE) => { // FP exception
                failure_count >= 2
            }

            // Normal failures - abort after 3
            WorkerExitReason::ExitCode(_) |
            WorkerExitReason::Signal(_) => {
                failure_count >= 3
            }

            // Success or unknown - don't abort
            WorkerExitReason::Success |
            WorkerExitReason::Unknown => false,
        }
    }

    pub fn to_error_message(&self) -> String {
        match self {
            WorkerExitReason::Success => "Worker exited successfully".to_string(),
            WorkerExitReason::ExitCode(code) => format!("Worker exited with code {}", code),
            WorkerExitReason::Signal(sig) => format!("Worker killed by signal: {:?}", sig),
            WorkerExitReason::Unknown => "Worker exited with unknown reason".to_string(),
        }
    }
}

impl From<std::process::ExitStatus> for WorkerExitReason {
    fn from(status: std::process::ExitStatus) -> Self {
        use nix::sys::wait::WaitStatus;

        if status.success() {
            return WorkerExitReason::Success;
        }

        if let Some(code) = status.code() {
            return WorkerExitReason::ExitCode(code);
        }

        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            if let Some(signal) = status.signal() {
                if let Ok(sig) = Signal::try_from(signal) {
                    return WorkerExitReason::Signal(sig);
                }
            }
        }

        WorkerExitReason::Unknown
    }
}
```

**6.3.2: Implement Worker Monitor**

Location: `mitosis-manager/src/worker/monitor.rs`

```rust
use tokio::time::{interval, Duration};
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct WorkerMonitor {
    worker_pool: Arc<RwLock<WorkerPool>>,
    poll_interval: Duration,
}

impl WorkerMonitor {
    pub fn new(worker_pool: Arc<RwLock<WorkerPool>>) -> Self {
        Self {
            worker_pool,
            poll_interval: Duration::from_secs(1),  // Poll every 1 second
        }
    }

    /// Start monitoring loop (runs indefinitely)
    pub async fn start(self) -> Result<()> {
        let mut ticker = interval(self.poll_interval);

        loop {
            ticker.tick().await;

            if let Err(e) = self.check_all_workers().await {
                tracing::error!("Worker monitoring error: {}", e);
            }
        }
    }

    async fn check_all_workers(&self) -> Result<()> {
        let mut pool = self.worker_pool.write().await;

        let worker_ids: Vec<u32> = pool.workers.keys().copied().collect();

        for local_id in worker_ids {
            if let Some(worker) = pool.get_worker_mut(local_id) {
                if !worker.is_alive() {
                    // Worker died - handle it
                    self.handle_worker_death(&mut pool, local_id).await?;
                }
            }
        }

        Ok(())
    }

    async fn handle_worker_death(
        &self,
        pool: &mut WorkerPool,
        local_id: u32,
    ) -> Result<()> {
        // Implementation continues in Task 6.3.3
        todo!()
    }
}
```

**6.3.3: Implement Auto-Respawn Logic**

Location: `mitosis-manager/src/worker/recovery.rs`

```rust
use uuid::Uuid;
use std::collections::HashMap;
use time::OffsetDateTime;

#[derive(Debug, Clone)]
pub struct TaskFailureInfo {
    pub task_uuid: Uuid,
    pub count: u32,
    pub last_failure_at: OffsetDateTime,
    pub error_messages: Vec<String>,
    pub failed_worker_ids: Vec<u32>,
}

pub struct WorkerRecoveryManager {
    manager_uuid: Uuid,
    failure_tracker: HashMap<Uuid, TaskFailureInfo>,  // task_uuid -> failures
}

impl WorkerRecoveryManager {
    pub fn new(manager_uuid: Uuid) -> Self {
        Self {
            manager_uuid,
            failure_tracker: HashMap::new(),
        }
    }

    pub async fn handle_worker_exit(
        &mut self,
        pool: &mut WorkerPool,
        local_id: u32,
        exit_reason: WorkerExitReason,
    ) -> Result<()> {
        let worker = pool.get_worker_mut(local_id)
            .context("Worker not found")?;

        tracing::warn!(
            "Worker {} exited: {}",
            local_id,
            exit_reason.to_error_message()
        );

        // Check if worker was executing a task
        if let Some(task_uuid) = worker.current_task_uuid {
            // Record failure
            let failure_count = self.record_task_failure(
                task_uuid,
                local_id,
                &exit_reason.to_error_message(),
            ).await?;

            // Check if we should abort the task
            if exit_reason.should_abort(failure_count) {
                tracing::error!(
                    "Task {} failed {} times, aborting",
                    task_uuid,
                    failure_count
                );

                // Report failure to coordinator
                // (coordinator will reassign to different manager)
                self.abort_task(task_uuid, failure_count).await?;

                // Do NOT respawn for this task
                return Ok(());
            } else {
                tracing::info!(
                    "Task {} failed {} times, retrying",
                    task_uuid,
                    failure_count
                );
            }
        }

        // Respawn worker
        self.respawn_worker(pool, local_id).await?;

        Ok(())
    }

    async fn record_task_failure(
        &mut self,
        task_uuid: Uuid,
        worker_local_id: u32,
        error_message: &str,
    ) -> Result<u32> {
        // Update local tracking
        let info = self.failure_tracker
            .entry(task_uuid)
            .or_insert_with(|| TaskFailureInfo {
                task_uuid,
                count: 0,
                last_failure_at: OffsetDateTime::now_utc(),
                error_messages: vec![],
                failed_worker_ids: vec![],
            });

        info.count += 1;
        info.last_failure_at = OffsetDateTime::now_utc();
        info.error_messages.push(error_message.to_string());
        info.failed_worker_ids.push(worker_local_id);

        // Persist to database via coordinator (WebSocket message)
        // This allows coordinator to track failures across all managers
        // (Implementation depends on WebSocket client from Phase 3)

        Ok(info.count)
    }

    async fn abort_task(&self, task_uuid: Uuid, failure_count: u32) -> Result<()> {
        // Send abort message to coordinator via WebSocket
        // Coordinator will mark task as failed and potentially reassign
        // to a different manager

        tracing::info!(
            "Aborting task {} after {} failures",
            task_uuid,
            failure_count
        );

        Ok(())
    }

    async fn respawn_worker(
        &self,
        pool: &mut WorkerPool,
        local_id: u32,
    ) -> Result<()> {
        // Get CPU cores from old worker
        let cpu_cores = pool.get_worker(local_id)
            .map(|w| w.cpu_cores.clone())
            .unwrap_or_default();

        // Remove old worker
        pool.workers.remove(&local_id);

        // Spawn new worker with same configuration
        let new_worker = pool.spawner.spawn_worker(local_id, cpu_cores).await?;
        pool.workers.insert(local_id, new_worker);

        tracing::info!("Respawned worker {}", local_id);

        Ok(())
    }
}
```

**Testing:**
- [ ] Worker crash detected within 1 second
- [ ] Worker auto-respawns after crash
- [ ] Task retried after worker death (failure_count < 3)
- [ ] Task aborted after 3 failures
- [ ] SIGSEGV causes abort after 2 failures
- [ ] Exit code 1 causes abort after 3 failures
- [ ] Worker respawn maintains CPU binding

---

### Task 6.4: Implement IPC Request Handlers

**Objective:** Handle FetchTask and ReportTask requests from managed workers via iceoryx2.

#### Subtasks

**6.4.1: Implement FetchTask Handler**

Location: `mitosis-manager/src/ipc/handlers.rs`

```rust
use anyhow::Result;
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct IpcRequestHandler {
    manager_uuid: Uuid,
    local_task_queue: Arc<RwLock<VecDeque<WorkerTaskResp>>>,  // Pre-fetched tasks
    coordinator_client: Arc<CoordinatorClient>,  // WebSocket client
    task_prefetch_count: u32,
}

impl IpcRequestHandler {
    pub fn new(
        manager_uuid: Uuid,
        coordinator_client: Arc<CoordinatorClient>,
        task_prefetch_count: u32,
    ) -> Self {
        Self {
            manager_uuid,
            local_task_queue: Arc::new(RwLock::new(VecDeque::new())),
            coordinator_client,
            task_prefetch_count,
        }
    }

    pub async fn handle_fetch_task(
        &self,
        req: FetchTaskRequest,
    ) -> Result<FetchTaskResponse> {
        tracing::debug!(
            "Worker {} requesting task (request_id: {})",
            req.worker_local_id,
            req.request_id
        );

        // Fast path: serve from local buffer
        {
            let mut queue = self.local_task_queue.write().await;
            if let Some(task) = queue.pop_front() {
                tracing::debug!(
                    "Serving task {} from buffer to worker {}",
                    task.id,
                    req.worker_local_id
                );

                // Trigger background refill
                self.trigger_buffer_refill();

                return Ok(FetchTaskResponse { task: Some(task) });
            }
        }

        // Slow path: buffer empty, fetch directly from coordinator
        tracing::debug!(
            "Buffer empty, fetching task directly for worker {}",
            req.worker_local_id
        );

        let task = self.coordinator_client
            .fetch_task_from_suite()
            .await?;

        // Trigger background refill
        self.trigger_buffer_refill();

        Ok(FetchTaskResponse { task })
    }

    fn trigger_buffer_refill(&self) {
        let queue = Arc::clone(&self.local_task_queue);
        let client = Arc::clone(&self.coordinator_client);
        let prefetch_count = self.task_prefetch_count;

        tokio::spawn(async move {
            if let Err(e) = Self::maintain_task_buffer(queue, client, prefetch_count).await {
                tracing::error!("Failed to refill task buffer: {}", e);
            }
        });
    }

    async fn maintain_task_buffer(
        queue: Arc<RwLock<VecDeque<WorkerTaskResp>>>,
        client: Arc<CoordinatorClient>,
        prefetch_count: u32,
    ) -> Result<()> {
        // Implementation in Task 6.5
        todo!()
    }
}
```

**6.4.2: Implement ReportTask Handler**

```rust
impl IpcRequestHandler {
    pub async fn handle_report_task(
        &self,
        req: ReportTaskRequest,
    ) -> Result<ReportTaskResponse> {
        tracing::debug!(
            "Worker {} reporting task {} (op: {:?})",
            req.worker_local_id,
            req.task_id,
            req.op
        );

        // Proxy request to coordinator via WebSocket
        let resp = self.coordinator_client
            .report_task(req.task_id, req.op)
            .await?;

        Ok(ReportTaskResponse {
            success: resp.success,
            url: resp.url,
        })
    }

    pub async fn handle_heartbeat(
        &self,
        req: HeartbeatRequest,
    ) -> Result<HeartbeatResponse> {
        // Optional: track worker liveness
        tracing::trace!("Heartbeat from worker {}", req.worker_local_id);

        Ok(HeartbeatResponse { ack: true })
    }
}
```

**6.4.3: Integrate Handlers with IPC Service Loop**

Location: `mitosis-manager/src/ipc/server.rs`

```rust
pub struct IpcServer {
    service_manager: IpcServiceManager,
    request_handler: Arc<IpcRequestHandler>,
}

impl IpcServer {
    pub fn new(
        manager_uuid: Uuid,
        coordinator_client: Arc<CoordinatorClient>,
        task_prefetch_count: u32,
    ) -> Self {
        let service_manager = IpcServiceManager::new(manager_uuid);
        let request_handler = Arc::new(IpcRequestHandler::new(
            manager_uuid,
            coordinator_client,
            task_prefetch_count,
        ));

        Self {
            service_manager,
            request_handler,
        }
    }

    pub async fn start(&mut self) -> Result<()> {
        // Initialize iceoryx2 services
        self.service_manager.initialize().await?;

        // Spawn handler loops
        self.spawn_fetch_task_loop();
        self.spawn_report_task_loop();
        self.spawn_heartbeat_loop();

        Ok(())
    }

    fn spawn_fetch_task_loop(&self) {
        let service = self.service_manager.task_fetch_service.clone();
        let handler = Arc::clone(&self.request_handler);

        tokio::spawn(async move {
            loop {
                match service.receive().await {
                    Ok(req) => {
                        let response = handler.handle_fetch_task(req).await
                            .unwrap_or_else(|e| {
                                tracing::error!("FetchTask error: {}", e);
                                FetchTaskResponse { task: None }
                            });

                        let _ = service.send_response(response).await;
                    }
                    Err(e) => {
                        tracing::error!("IPC receive error: {}", e);
                    }
                }
            }
        });
    }

    fn spawn_report_task_loop(&self) {
        // Similar to fetch_task_loop
    }

    fn spawn_heartbeat_loop(&self) {
        // Similar to fetch_task_loop
    }
}
```

**Testing:**
- [ ] FetchTask returns task from buffer (fast path)
- [ ] FetchTask fetches from coordinator when buffer empty (slow path)
- [ ] ReportTask proxies to coordinator correctly
- [ ] Multiple concurrent FetchTask requests handled
- [ ] Heartbeat responses received

---

### Task 6.5: Implement Task Pre-fetching

**Objective:** Maintain a local buffer of pre-fetched tasks to minimize worker idle time.

#### Subtasks

**6.5.1: Implement Buffer Maintenance Logic**

Location: `mitosis-manager/src/ipc/prefetch.rs`

```rust
use std::collections::VecDeque;
use tokio::sync::RwLock;
use std::sync::Arc;

pub async fn maintain_task_buffer(
    queue: Arc<RwLock<VecDeque<WorkerTaskResp>>>,
    client: Arc<CoordinatorClient>,
    prefetch_count: u32,
) -> Result<()> {
    let current_size = queue.read().await.len();

    if current_size >= prefetch_count as usize {
        // Buffer is full
        return Ok(());
    }

    let needed = (prefetch_count as usize) - current_size;

    tracing::debug!(
        "Prefetching {} tasks (current: {}, target: {})",
        needed,
        current_size,
        prefetch_count
    );

    // Fetch tasks from coordinator
    for _ in 0..needed {
        match client.fetch_task_from_suite().await {
            Ok(Some(task)) => {
                queue.write().await.push_back(task);
            }
            Ok(None) => {
                tracing::debug!("No more tasks available");
                break;
            }
            Err(e) => {
                tracing::error!("Failed to prefetch task: {}", e);
                break;
            }
        }
    }

    let final_size = queue.read().await.len();
    tracing::debug!("Buffer refilled: {} tasks", final_size);

    Ok(())
}
```

**6.5.2: Implement Proactive Prefetch Trigger**

```rust
pub struct PrefetchManager {
    queue: Arc<RwLock<VecDeque<WorkerTaskResp>>>,
    client: Arc<CoordinatorClient>,
    prefetch_count: u32,
    refill_threshold: u32,  // Trigger refill when queue < threshold
}

impl PrefetchManager {
    pub fn new(
        queue: Arc<RwLock<VecDeque<WorkerTaskResp>>>,
        client: Arc<CoordinatorClient>,
        prefetch_count: u32,
    ) -> Self {
        let refill_threshold = prefetch_count / 2;  // Refill at 50% capacity

        Self {
            queue,
            client,
            prefetch_count,
            refill_threshold,
        }
    }

    /// Start background task to maintain buffer
    pub async fn start_background_maintenance(self) {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(5));

            loop {
                interval.tick().await;

                let current_size = self.queue.read().await.len();

                if current_size < self.refill_threshold as usize {
                    tracing::debug!(
                        "Buffer below threshold ({} < {}), refilling",
                        current_size,
                        self.refill_threshold
                    );

                    if let Err(e) = maintain_task_buffer(
                        Arc::clone(&self.queue),
                        Arc::clone(&self.client),
                        self.prefetch_count,
                    ).await {
                        tracing::error!("Buffer maintenance failed: {}", e);
                    }
                }
            }
        });
    }
}
```

**Testing:**
- [ ] Buffer fills to `task_prefetch_count` on startup
- [ ] Buffer refills when below threshold
- [ ] No unnecessary fetches when buffer is full
- [ ] Workers receive tasks with minimal latency (< 10ms)
- [ ] Buffer handles "no tasks available" gracefully

---

### Task 6.6: Implement Failure Tracking

**Objective:** Track task execution failures in database and abort tasks after 3 worker deaths.

#### Subtasks

**6.6.1: Implement Database Failure Recording**

Location: `mitosis-coordinator/src/suite/failure_tracker.rs`

```rust
use sqlx::PgPool;
use uuid::Uuid;

pub async fn record_task_failure(
    db: &PgPool,
    task_id: i64,
    task_uuid: Uuid,
    task_suite_id: i64,
    manager_id: i64,
    failure_count: i32,
    error_message: &str,
    worker_local_id: i32,
) -> Result<()> {
    sqlx::query!(
        r#"
        INSERT INTO task_execution_failures
            (task_id, task_uuid, task_suite_id, manager_id, failure_count,
             last_failure_at, error_messages, worker_local_ids)
        VALUES ($1, $2, $3, $4, $5, NOW(), ARRAY[$6], ARRAY[$7])
        ON CONFLICT (task_uuid, manager_id)
        DO UPDATE SET
            failure_count = $5,
            last_failure_at = NOW(),
            error_messages = task_execution_failures.error_messages || $6,
            worker_local_ids = task_execution_failures.worker_local_ids || $7,
            updated_at = NOW()
        "#,
        task_id,
        task_uuid,
        task_suite_id,
        manager_id,
        failure_count,
        error_message,
        worker_local_id,
    )
    .execute(db)
    .await?;

    tracing::info!(
        "Recorded failure #{} for task {} on manager {}",
        failure_count,
        task_uuid,
        manager_id
    );

    Ok(())
}

pub async fn get_failure_count(
    db: &PgPool,
    task_uuid: Uuid,
    manager_id: i64,
) -> Result<i32> {
    let row = sqlx::query!(
        "SELECT failure_count FROM task_execution_failures
         WHERE task_uuid = $1 AND manager_id = $2",
        task_uuid,
        manager_id
    )
    .fetch_optional(db)
    .await?;

    Ok(row.map(|r| r.failure_count).unwrap_or(0))
}

pub async fn abort_task(
    db: &PgPool,
    task_uuid: Uuid,
    reason: &str,
) -> Result<()> {
    // Mark task as failed in active_tasks
    sqlx::query!(
        r#"
        UPDATE active_tasks
        SET status = 'failed',
            error_message = $2,
            updated_at = NOW()
        WHERE task_uuid = $1
        "#,
        task_uuid,
        reason
    )
    .execute(db)
    .await?;

    tracing::warn!("Aborted task {}: {}", task_uuid, reason);

    Ok(())
}
```

**6.6.2: Integrate Failure Tracking in Manager**

Location: `mitosis-manager/src/coordinator/client.rs`

```rust
impl CoordinatorClient {
    pub async fn report_task_failure(
        &self,
        task_uuid: Uuid,
        manager_uuid: Uuid,
        failure_count: u32,
        error_message: &str,
        worker_local_id: u32,
    ) -> Result<()> {
        // Send WebSocket message to coordinator
        let msg = WebSocketMessage::ReportTaskFailure {
            task_uuid,
            manager_uuid,
            failure_count,
            error_message: error_message.to_string(),
            worker_local_id,
        };

        self.ws_send(msg).await?;

        Ok(())
    }
}
```

**Testing:**
- [ ] First failure recorded with count=1
- [ ] Second failure increments count to 2
- [ ] Third failure increments count to 3 and aborts task
- [ ] Error messages array accumulates all failures
- [ ] Worker local IDs tracked correctly
- [ ] Task not reassigned to same manager after abort

---

### Task 6.7: Implement CPU Core Binding

**Objective:** Bind workers to CPU cores according to specified strategy.

#### Subtasks

**6.7.1: Implement Core Validation**

Location: `mitosis-manager/src/worker/cpu_validation.rs`

```rust
use anyhow::{Result, bail};

/// Get total number of CPU cores on system
pub fn get_cpu_count() -> Result<usize> {
    Ok(num_cpus::get())
}

/// Validate that specified cores exist on system
pub fn validate_cores(cores: &[usize]) -> Result<()> {
    let total_cores = get_cpu_count()?;

    for &core in cores {
        if core >= total_cores {
            bail!(
                "Invalid CPU core {}: system has {} cores (0-{})",
                core,
                total_cores,
                total_cores - 1
            );
        }
    }

    Ok(())
}
```

**6.7.2: Test Each Binding Strategy**

**Test: RoundRobin**
```rust
#[tokio::test]
async fn test_round_robin_binding() {
    let binding = CpuBinding {
        cores: vec![0, 1, 2, 3],
        strategy: CpuBindingStrategy::RoundRobin,
    };

    let allocations = binding.allocate_cores(8).unwrap();

    assert_eq!(allocations.len(), 8);
    assert_eq!(allocations[0], vec![0]);  // Worker 0 → core 0
    assert_eq!(allocations[1], vec![1]);  // Worker 1 → core 1
    assert_eq!(allocations[2], vec![2]);  // Worker 2 → core 2
    assert_eq!(allocations[3], vec![3]);  // Worker 3 → core 3
    assert_eq!(allocations[4], vec![0]);  // Worker 4 → core 0 (cycle)
    assert_eq!(allocations[5], vec![1]);  // Worker 5 → core 1
}
```

**Test: Exclusive**
```rust
#[tokio::test]
async fn test_exclusive_binding() {
    let binding = CpuBinding {
        cores: vec![0, 1, 2, 3],
        strategy: CpuBindingStrategy::Exclusive,
    };

    let allocations = binding.allocate_cores(2).unwrap();

    assert_eq!(allocations.len(), 2);
    assert_eq!(allocations[0], vec![0, 1]);  // Worker 0 → cores 0,1
    assert_eq!(allocations[1], vec![2, 3]);  // Worker 1 → cores 2,3
}
```

**Test: Shared**
```rust
#[tokio::test]
async fn test_shared_binding() {
    let binding = CpuBinding {
        cores: vec![0, 1, 2, 3],
        strategy: CpuBindingStrategy::Shared,
    };

    let allocations = binding.allocate_cores(4).unwrap();

    assert_eq!(allocations.len(), 4);
    for i in 0..4 {
        assert_eq!(allocations[i], vec![0, 1, 2, 3]);  // All workers share all cores
    }
}
```

**Testing:**
- [ ] RoundRobin: workers cycle through cores correctly
- [ ] Exclusive: cores divided equally (or with remainder to last worker)
- [ ] Shared: all workers get all cores
- [ ] Invalid core IDs rejected
- [ ] Exclusive with too few cores returns error
- [ ] Verify with `taskset -cp <pid>` after spawning

---

## Testing Checklist

### Unit Tests
- [ ] IPC service creation and cleanup
- [ ] CPU binding strategy allocation logic
- [ ] Exit reason classification
- [ ] Task failure counting logic
- [ ] Buffer prefetch logic

### Integration Tests
- [ ] Spawn 1 worker and execute 1 task
- [ ] Spawn 16 workers and execute 100 tasks
- [ ] Spawn 256 workers (stress test)
- [ ] Worker crash and auto-respawn
- [ ] Worker death with task in progress
- [ ] Task aborted after 3 failures
- [ ] Task pre-fetching buffer maintains level
- [ ] IPC message roundtrip (fetch + report)
- [ ] All CPU binding strategies

### E2E Tests
- [ ] Complete suite execution with managed workers
- [ ] Worker crash mid-task, task retried successfully
- [ ] Worker SIGSEGV, task aborted after 2 failures
- [ ] No buffer underruns during high load
- [ ] Multiple managers executing same suite

### Performance Tests
- [ ] Worker task fetch latency < 10ms (buffer hit)
- [ ] Worker task fetch latency < 100ms (buffer miss)
- [ ] 16 workers execute 1000 tasks with 0 idle time
- [ ] CPU binding verified with `top` or `htop`

---

## Success Criteria

### Functional Requirements
- ✅ Manager spawns N workers according to WorkerSchedulePlan
- ✅ Workers communicate with manager via iceoryx2 IPC
- ✅ Workers bound to correct CPU cores per strategy
- ✅ Worker crashes detected within 1 second
- ✅ Workers auto-respawn after crash
- ✅ Tasks aborted after 3 worker deaths
- ✅ Task buffer maintained at specified level
- ✅ Failures recorded in task_execution_failures table

### Performance Requirements
- ✅ Task fetch latency < 10ms (buffer hit)
- ✅ Worker respawn time < 2 seconds
- ✅ Support 256 workers per manager
- ✅ Zero task loss on worker crash

### Quality Requirements
- ✅ All unit tests pass
- ✅ All integration tests pass
- ✅ Code coverage > 80%
- ✅ No memory leaks (run with valgrind)
- ✅ Graceful shutdown (no orphaned workers)

---

## Dependencies

### Required Phases
- **Phase 1:** Database schema
- **Phase 2:** Coordinator APIs
- **Phase 3:** WebSocket Manager
- **Phase 4:** Suite Queue Manager
- **Phase 5:** Manager lifecycle and environment hooks

### External Dependencies
- **iceoryx2** (>= 0.4.0): IPC framework
- **nix** (>= 0.27): Process management, signals, CPU affinity
- **tokio** (>= 1.35): Async runtime
- **sqlx** (>= 0.7): Database access

---

## Next Phase

**Phase 7: Managed Worker Mode**

Implement the worker-side IPC client to communicate with the manager:
- Add `--managed` CLI flag to worker binary
- Implement `IpcWorkerComm` struct (vs `HttpWorkerComm`)
- Subscribe to control messages (shutdown, cancel task)
- Test managed worker end-to-end with manager

See `/home/user/mitosis/impl/phase7/01-managed-worker-mode.md`

---

## Common Pitfalls and Troubleshooting

### IPC Issues
**Problem:** Worker cannot connect to manager's iceoryx2 services
**Solution:**
- Verify service name format: `mitosis_mgr_{uuid}_{service}`
- Check iceoryx2 RouDi daemon is running
- Ensure worker has permission to access shared memory

### CPU Binding Issues
**Problem:** `sched_setaffinity` returns `EINVAL`
**Solution:**
- Verify core IDs are valid (< total CPU count)
- Check process has CAP_SYS_NICE capability
- Use `taskset -cp <pid>` to verify affinity

### Worker Crash Loop
**Problem:** Worker repeatedly crashes immediately after spawn
**Solution:**
- Check worker binary path is correct
- Verify worker has execute permission
- Inspect worker stderr for error messages
- Check IPC service availability

### Buffer Underruns
**Problem:** Workers frequently idle waiting for tasks
**Solution:**
- Increase `task_prefetch_count`
- Reduce background refill interval
- Check coordinator task fetch performance
- Monitor WebSocket latency

### Memory Leaks
**Problem:** Manager memory usage grows over time
**Solution:**
- Ensure workers are properly cleaned up after exit
- Verify failure_tracker is bounded (clear old entries)
- Check iceoryx2 services are properly destroyed
- Run with valgrind to identify leaks

---

## Reference Code Examples

### Complete Worker Lifecycle

```rust
async fn execute_suite(&mut self, suite: TaskSuite) -> Result<()> {
    // 1. Setup IPC services
    self.ipc_server.start().await?;

    // 2. Spawn workers
    self.worker_pool.spawn_workers(&suite.worker_schedule).await?;

    // 3. Start monitoring
    let monitor = WorkerMonitor::new(Arc::clone(&self.worker_pool));
    tokio::spawn(monitor.start());

    // 4. Start prefetch manager
    let prefetch_mgr = PrefetchManager::new(
        Arc::clone(&self.local_task_queue),
        Arc::clone(&self.coordinator_client),
        suite.worker_schedule.task_prefetch_count,
    );
    prefetch_mgr.start_background_maintenance().await;

    // 5. Wait for suite completion
    self.wait_for_completion().await?;

    // 6. Shutdown workers gracefully
    self.worker_pool.shutdown_all().await?;

    // 7. Cleanup IPC services
    self.ipc_server.shutdown().await?;

    Ok(())
}
```

---

## Metrics and Monitoring

### Key Metrics to Track
- **Worker spawn rate**: workers/second
- **Worker crash rate**: crashes/minute
- **Task fetch latency**: p50, p95, p99 (milliseconds)
- **Buffer level**: current size vs target
- **Failure rate**: failures/total tasks
- **Respawn time**: time from death to new worker ready

### Logging Strategy
- **DEBUG**: IPC messages, buffer refills, worker status checks
- **INFO**: Worker spawn/death, task assignments, suite transitions
- **WARN**: Worker crashes, buffer underruns, retry attempts
- **ERROR**: Unrecoverable failures, task aborts, IPC errors

---

**End of Phase 6 Implementation Guide**
