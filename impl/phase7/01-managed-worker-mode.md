# Phase 7: Managed Worker Mode Implementation Guide

## Overview

Phase 7 implements the managed worker mode, enabling workers to run as subprocesses under Node Manager control and communicate via iceoryx2 IPC instead of HTTP. This phase transforms the existing independent worker code to support dual communication modes through trait-based abstraction.

**Duration**: 1 week

**Key Achievement**: Workers can operate in both independent (HTTP) and managed (IPC) modes using the same core execution logic, with only the communication layer varying.

## Prerequisites

### Required Phases Completed
- Phase 1: Database Schema
- Phase 2: API Schema and Coordinator Endpoints
- Phase 3: WebSocket Manager
- Phase 4: Node Manager Core
- Phase 5: Environment Hooks
- Phase 6: Worker Spawning and IPC (Manager-side IPC services must be operational)

### Knowledge Requirements
- Understanding of Rust trait-based abstraction (`#[async_trait]`, trait objects)
- Familiarity with existing worker codebase and execution logic
- Understanding of iceoryx2 IPC patterns (request-response, pub-sub)
- Knowledge of worker lifecycle (fetch task → execute → report)

### Technical Requirements
- iceoryx2 dependency already added in Phase 6
- Manager's IPC services operational (Phase 6)
- Worker binary already supports independent mode

## Timeline

**Total Duration**: 1 week (5 working days)

| Day | Tasks | Focus |
|-----|-------|-------|
| 1-2 | Task 7.1, 7.2 | CLI flags, WorkerComm trait design and implementation |
| 3-4 | Task 7.3, 7.4 | IPC communication, control message handling |
| 5 | Task 7.5, Testing | Main loop integration, end-to-end testing |

## Piece-by-Piece Breakdown

Each piece is independently testable and delivers incremental value:

**Piece 7.1: Add --managed flag to worker binary**
- CLI arg parsing
- Mode selection
- Deliverable: Worker knows if managed

**Piece 7.2: Define WorkerComm trait**
- Trait with fetch_task, report_task, send_heartbeat
- Deliverable: Trait compiles

**Piece 7.3: Implement HttpWorkerComm**
- Refactor existing HTTP code into trait
- Deliverable: Independent workers still work

**Piece 7.4: Implement IpcWorkerComm**
- Connect to manager's iceoryx2 services
- Implement trait methods via IPC
- Deliverable: Managed workers can communicate via IPC

**Piece 7.5: Update worker main loop**
- Use WorkerComm trait
- Works for both modes
- Deliverable: Unified worker code

**Piece 7.6: Handle control messages (Shutdown, CancelTask)**
- Subscribe to control topic
- Handle gracefully
- Deliverable: Workers respond to control messages

## Design References

### Section 4.3: Worker Modes

#### Independent Mode (Existing - Unchanged)
- Worker registers directly with coordinator
- Polls coordinator HTTP API for tasks
- No local manager involvement
- Visible in coordinator database

#### Managed Mode (New)
- Worker spawned by Node Manager as subprocess
- **Anonymous to coordinator** (not registered, not in database)
- Communicates with manager via **iceoryx2 IPC** (not HTTP)
- Manager authenticates on behalf of workers using **manager's JWT**
- Lifecycle tied to task suite execution
- Auto-respawned by manager on crash

### WorkerComm Trait Abstraction

**Key Insight**: Managed workers reuse existing worker code with communication abstraction:

```rust
#[async_trait]
trait WorkerComm {
    async fn fetch_task(&mut self) -> Result<Option<WorkerTaskResp>>;
    async fn report_task(&mut self, task_id: i64, op: ReportTaskOp) -> Result<ReportTaskResp>;
    async fn send_heartbeat(&mut self) -> Result<()>;
}

// Independent workers: HTTP implementation
struct HttpWorkerComm { ... }

// Managed workers: IPC implementation
struct IpcWorkerComm { ... }

// Worker code remains identical
struct Worker {
    comm: Box<dyn WorkerComm>,  // Abstract communication
    // ... rest of worker logic
}
```

### IPC Communication Pattern

#### Service Naming Convention
All services use the format: `mitosis_mgr_{manager_uuid}_{service_name}`

**Request-Response Services:**
- `mitosis_mgr_{manager_uuid}_task_fetch` - Worker requests tasks
- `mitosis_mgr_{manager_uuid}_task_report` - Worker reports task status

**Pub-Sub Topics:**
- `mitosis_mgr_{manager_uuid}_control` - Manager sends control messages (shutdown, cancel)

#### IPC Message Types

**FetchTaskRequest / FetchTaskResponse:**
```rust
#[derive(Serialize, Deserialize)]
struct FetchTaskRequest {
    worker_local_id: u32,
    request_id: u64,
}

#[derive(Serialize, Deserialize)]
struct FetchTaskResponse {
    task: Option<WorkerTaskResp>,
}
```

**ReportTaskRequest / ReportTaskResponse:**
```rust
#[derive(Serialize, Deserialize)]
struct ReportTaskRequest {
    worker_local_id: u32,
    request_id: u64,
    task_id: i64,
    op: ReportTaskOp,
}

#[derive(Serialize, Deserialize)]
struct ReportTaskResponse {
    success: bool,
    url: Option<String>,
}
```

**ControlMessage (Pub-Sub):**
```rust
#[derive(Serialize, Deserialize)]
enum ControlMessage {
    Shutdown { graceful: bool },
    CancelTask { task_id: i64, graceful: bool },
    Pause,
    Resume,
}
```

### IPC Setup (Worker Side)

```rust
impl Worker {
    fn new_managed(manager_uuid: Uuid, worker_local_id: u32) -> Result<Self> {
        let service_prefix = format!("mitosis_mgr_{}", manager_uuid);

        // Connect to manager's services
        let task_fetch_client = ServiceBuilder::new(&format!("{}_task_fetch", service_prefix))
            .request_response::<FetchTaskRequest, FetchTaskResponse>()
            .open_client()?;

        let task_report_client = ServiceBuilder::new(&format!("{}_task_report", service_prefix))
            .request_response::<ReportTaskRequest, ReportTaskResponse>()
            .open_client()?;

        let control_subscriber = ServiceBuilder::new(&format!("{}_control", service_prefix))
            .publish_subscribe::<ControlMessage>()
            .open_subscriber()?;

        let comm = Box::new(IpcWorkerComm {
            manager_uuid,
            worker_local_id,
            task_fetch_client,
            task_report_client,
        });

        Ok(Worker {
            comm,
            control_subscriber,
            // ... rest of worker fields
        })
    }
}
```

## Implementation Tasks

### Task 7.1: Update Worker Binary with --managed Flag

**Objective**: Add CLI argument parsing to detect managed mode and extract IPC configuration.

**Files to Modify**:
- `crates/mitosis-worker/src/main.rs` (or equivalent CLI entry point)
- `crates/mitosis-worker/src/config.rs` (if using config module)

**Implementation Steps**:

1. **Add CLI Arguments**:
   ```rust
   use clap::Parser;

   #[derive(Parser, Debug)]
   #[command(name = "mitosis-worker")]
   #[command(about = "Mitosis distributed task worker")]
   struct Cli {
       /// Run in managed mode (spawned by Node Manager)
       #[arg(long)]
       managed: bool,

       /// Manager UUID (required for managed mode)
       #[arg(long, required_if_eq("managed", "true"))]
       manager_uuid: Option<Uuid>,

       /// Worker local ID (0-255, required for managed mode)
       #[arg(long, required_if_eq("managed", "true"))]
       worker_local_id: Option<u32>,

       /// Coordinator URL (required for independent mode)
       #[arg(long, env = "MITOSIS_COORDINATOR_URL")]
       coordinator_url: Option<String>,

       /// Worker UUID (for independent mode)
       #[arg(long, env = "MITOSIS_WORKER_UUID")]
       worker_uuid: Option<Uuid>,

       // ... existing args (JWT, tags, etc.)
   }
   ```

2. **Validate Arguments**:
   ```rust
   fn validate_args(cli: &Cli) -> Result<()> {
       if cli.managed {
           // Managed mode: require manager_uuid and worker_local_id
           if cli.manager_uuid.is_none() {
               return Err(anyhow!("--manager-uuid required for managed mode"));
           }
           if cli.worker_local_id.is_none() {
               return Err(anyhow!("--worker-local-id required for managed mode"));
           }
           if let Some(id) = cli.worker_local_id {
               if id > 255 {
                   return Err(anyhow!("worker_local_id must be 0-255"));
               }
           }
       } else {
           // Independent mode: require coordinator_url
           if cli.coordinator_url.is_none() {
               return Err(anyhow!("--coordinator-url required for independent mode"));
           }
       }
       Ok(())
   }
   ```

3. **Branch on Worker Mode**:
   ```rust
   #[tokio::main]
   async fn main() -> Result<()> {
       let cli = Cli::parse();
       validate_args(&cli)?;

       let worker = if cli.managed {
           let manager_uuid = cli.manager_uuid.unwrap();
           let worker_local_id = cli.worker_local_id.unwrap();
           Worker::new_managed(manager_uuid, worker_local_id).await?
       } else {
           let coordinator_url = cli.coordinator_url.unwrap();
           Worker::new_independent(coordinator_url, cli.worker_uuid).await?
       };

       worker.run().await?;
       Ok(())
   }
   ```

**Testing**:
- Run with `--managed` flag without required args (should fail)
- Run with valid managed mode args (should not crash on startup)
- Run in independent mode (existing behavior unchanged)

---

### Task 7.2: Implement WorkerComm Trait Abstraction

**Objective**: Define trait interface and implement both HTTP and IPC variants.

**Files to Create/Modify**:
- `crates/mitosis-worker/src/comm/mod.rs` (new module)
- `crates/mitosis-worker/src/comm/trait.rs` (trait definition)
- `crates/mitosis-worker/src/comm/http.rs` (existing HTTP logic)
- `crates/mitosis-worker/src/comm/ipc.rs` (new IPC implementation)

**Implementation Steps**:

1. **Define WorkerComm Trait**:
   ```rust
   // crates/mitosis-worker/src/comm/trait.rs

   use anyhow::Result;
   use async_trait::async_trait;
   use mitosis_types::{WorkerTaskResp, ReportTaskOp, ReportTaskResp};

   #[async_trait]
   pub trait WorkerComm: Send + Sync {
       /// Fetch next task from coordinator (independent) or manager (managed)
       async fn fetch_task(&mut self) -> Result<Option<WorkerTaskResp>>;

       /// Report task status (Started, Completed, Failed)
       async fn report_task(
           &mut self,
           task_id: i64,
           op: ReportTaskOp,
       ) -> Result<ReportTaskResp>;

       /// Send heartbeat (optional for managed workers)
       async fn send_heartbeat(&mut self) -> Result<()>;
   }
   ```

2. **Implement HttpWorkerComm** (Refactor Existing Code):
   ```rust
   // crates/mitosis-worker/src/comm/http.rs

   pub struct HttpWorkerComm {
       client: reqwest::Client,
       coordinator_url: String,
       worker_uuid: Uuid,
       jwt_token: String,
   }

   impl HttpWorkerComm {
       pub fn new(coordinator_url: String, worker_uuid: Uuid, jwt_token: String) -> Self {
           let client = reqwest::Client::builder()
               .timeout(Duration::from_secs(30))
               .build()
               .expect("Failed to build HTTP client");

           Self {
               client,
               coordinator_url,
               worker_uuid,
               jwt_token,
           }
       }
   }

   #[async_trait]
   impl WorkerComm for HttpWorkerComm {
       async fn fetch_task(&mut self) -> Result<Option<WorkerTaskResp>> {
           let url = format!("{}/api/v1/workers/fetch-task", self.coordinator_url);
           let response = self.client
               .get(&url)
               .bearer_auth(&self.jwt_token)
               .send()
               .await?;

           if response.status() == StatusCode::NO_CONTENT {
               return Ok(None);
           }

           let task = response.json::<WorkerTaskResp>().await?;
           Ok(Some(task))
       }

       async fn report_task(
           &mut self,
           task_id: i64,
           op: ReportTaskOp,
       ) -> Result<ReportTaskResp> {
           let url = format!("{}/api/v1/workers/report-task", self.coordinator_url);
           let response = self.client
               .post(&url)
               .bearer_auth(&self.jwt_token)
               .json(&serde_json::json!({
                   "task_id": task_id,
                   "op": op,
               }))
               .send()
               .await?;

           let resp = response.json::<ReportTaskResp>().await?;
           Ok(resp)
       }

       async fn send_heartbeat(&mut self) -> Result<()> {
           let url = format!("{}/api/v1/workers/heartbeat", self.coordinator_url);
           self.client
               .post(&url)
               .bearer_auth(&self.jwt_token)
               .json(&serde_json::json!({
                   "worker_uuid": self.worker_uuid,
               }))
               .send()
               .await?;
           Ok(())
       }
   }
   ```

3. **Implement IpcWorkerComm**:
   ```rust
   // crates/mitosis-worker/src/comm/ipc.rs

   use iceoryx2::prelude::*;
   use std::sync::atomic::{AtomicU64, Ordering};

   pub struct IpcWorkerComm {
       manager_uuid: Uuid,
       worker_local_id: u32,
       task_fetch_client: RequestResponseClient<FetchTaskRequest, FetchTaskResponse>,
       task_report_client: RequestResponseClient<ReportTaskRequest, ReportTaskResponse>,
       request_id_counter: AtomicU64,
   }

   impl IpcWorkerComm {
       pub fn new(
           manager_uuid: Uuid,
           worker_local_id: u32,
       ) -> Result<Self> {
           let service_prefix = format!("mitosis_mgr_{}", manager_uuid);

           // Connect to task_fetch service
           let task_fetch_client = ServiceBuilder::new(
               &format!("{}_task_fetch", service_prefix)
           )
               .request_response::<FetchTaskRequest, FetchTaskResponse>()
               .open_client()?;

           // Connect to task_report service
           let task_report_client = ServiceBuilder::new(
               &format!("{}_task_report", service_prefix)
           )
               .request_response::<ReportTaskRequest, ReportTaskResponse>()
               .open_client()?;

           Ok(Self {
               manager_uuid,
               worker_local_id,
               task_fetch_client,
               task_report_client,
               request_id_counter: AtomicU64::new(0),
           })
       }

       fn next_request_id(&self) -> u64 {
           self.request_id_counter.fetch_add(1, Ordering::SeqCst)
       }
   }

   #[async_trait]
   impl WorkerComm for IpcWorkerComm {
       async fn fetch_task(&mut self) -> Result<Option<WorkerTaskResp>> {
           let request = FetchTaskRequest {
               worker_local_id: self.worker_local_id,
               request_id: self.next_request_id(),
           };

           // Send request to manager
           let response = self.task_fetch_client
               .send(request)
               .await
               .context("Failed to send fetch_task request")?;

           Ok(response.task)
       }

       async fn report_task(
           &mut self,
           task_id: i64,
           op: ReportTaskOp,
       ) -> Result<ReportTaskResp> {
           let request = ReportTaskRequest {
               worker_local_id: self.worker_local_id,
               request_id: self.next_request_id(),
               task_id,
               op,
           };

           // Send request to manager
           let response = self.task_report_client
               .send(request)
               .await
               .context("Failed to send report_task request")?;

           Ok(ReportTaskResp {
               success: response.success,
               url: response.url,
           })
       }

       async fn send_heartbeat(&mut self) -> Result<()> {
           // Heartbeat optional for managed workers (manager tracks process health)
           Ok(())
       }
   }
   ```

**Testing**:
- Unit test: Create `HttpWorkerComm` and mock coordinator responses
- Unit test: Create `IpcWorkerComm` and verify service connection (requires running manager)
- Verify trait object creation: `Box<dyn WorkerComm>`

---

### Task 7.3: Implement IPC Communication in Worker

**Objective**: Establish IPC connections to manager services and handle message serialization.

**Files to Modify**:
- `crates/mitosis-worker/src/worker.rs` (Worker struct)

**Implementation Steps**:

1. **Add Control Subscriber to Worker Struct**:
   ```rust
   pub struct Worker {
       // Communication abstraction
       comm: Box<dyn WorkerComm>,

       // Control channel (only for managed workers)
       control_subscriber: Option<SubscriberHandle<ControlMessage>>,

       // Worker identity
       worker_id: WorkerId,  // Enum: Independent(Uuid) | Managed(u32)

       // Execution state
       executor: TaskExecutor,
       current_task: Option<ActiveTask>,

       // Metrics
       tasks_completed: AtomicU64,
       tasks_failed: AtomicU64,
   }

   pub enum WorkerId {
       Independent(Uuid),
       Managed { manager_uuid: Uuid, local_id: u32 },
   }
   ```

2. **Implement Worker Constructors**:
   ```rust
   impl Worker {
       pub async fn new_managed(
           manager_uuid: Uuid,
           worker_local_id: u32,
       ) -> Result<Self> {
           // Create IPC communication
           let comm = Box::new(IpcWorkerComm::new(manager_uuid, worker_local_id)?);

           // Subscribe to control messages
           let service_prefix = format!("mitosis_mgr_{}", manager_uuid);
           let control_subscriber = ServiceBuilder::new(
               &format!("{}_control", service_prefix)
           )
               .publish_subscribe::<ControlMessage>()
               .open_subscriber()?;

           Ok(Self {
               comm,
               control_subscriber: Some(control_subscriber),
               worker_id: WorkerId::Managed { manager_uuid, local_id: worker_local_id },
               executor: TaskExecutor::new()?,
               current_task: None,
               tasks_completed: AtomicU64::new(0),
               tasks_failed: AtomicU64::new(0),
           })
       }

       pub async fn new_independent(
           coordinator_url: String,
           worker_uuid: Uuid,
           jwt_token: String,
       ) -> Result<Self> {
           // Create HTTP communication
           let comm = Box::new(HttpWorkerComm::new(
               coordinator_url,
               worker_uuid,
               jwt_token,
           ));

           Ok(Self {
               comm,
               control_subscriber: None,
               worker_id: WorkerId::Independent(worker_uuid),
               executor: TaskExecutor::new()?,
               current_task: None,
               tasks_completed: AtomicU64::new(0),
               tasks_failed: AtomicU64::new(0),
           })
       }
   }
   ```

**Testing**:
- Unit test: Verify `Worker::new_managed()` connects to IPC services
- Unit test: Verify `Worker::new_independent()` creates HTTP client
- Integration test: Spawn manager and worker, verify IPC connection established

---

### Task 7.4: Implement Control Message Handling

**Objective**: Subscribe to control topic and handle shutdown/cancel messages.

**Files to Modify**:
- `crates/mitosis-worker/src/worker.rs`
- `crates/mitosis-worker/src/control.rs` (new module)

**Implementation Steps**:

1. **Define Control Message Handler**:
   ```rust
   // crates/mitosis-worker/src/control.rs

   use iceoryx2::prelude::*;

   pub struct ControlHandler {
       subscriber: SubscriberHandle<ControlMessage>,
       shutdown_requested: Arc<AtomicBool>,
       cancel_task_id: Arc<Mutex<Option<i64>>>,
   }

   impl ControlHandler {
       pub fn new(subscriber: SubscriberHandle<ControlMessage>) -> Self {
           Self {
               subscriber,
               shutdown_requested: Arc::new(AtomicBool::new(false)),
               cancel_task_id: Arc::new(Mutex::new(None)),
           }
       }

       pub async fn poll_messages(&mut self) -> Result<()> {
           // Non-blocking poll
           while let Some(msg) = self.subscriber.try_receive()? {
               self.handle_message(msg).await?;
           }
           Ok(())
       }

       async fn handle_message(&mut self, msg: ControlMessage) -> Result<()> {
           match msg {
               ControlMessage::Shutdown { graceful } => {
                   if graceful {
                       tracing::info!("Received graceful shutdown request");
                       self.shutdown_requested.store(true, Ordering::SeqCst);
                   } else {
                       tracing::warn!("Received immediate shutdown request");
                       std::process::exit(0);
                   }
               }

               ControlMessage::CancelTask { task_id, graceful } => {
                   tracing::info!("Received cancel request for task {}", task_id);
                   if graceful {
                       *self.cancel_task_id.lock().await = Some(task_id);
                   } else {
                       // Immediate cancellation: kill task process
                       // Implementation depends on TaskExecutor design
                   }
               }

               ControlMessage::Pause => {
                   tracing::info!("Received pause request");
                   // Implementation: stop fetching new tasks
               }

               ControlMessage::Resume => {
                   tracing::info!("Received resume request");
                   // Implementation: resume fetching tasks
               }
           }
           Ok(())
       }

       pub fn is_shutdown_requested(&self) -> bool {
           self.shutdown_requested.load(Ordering::SeqCst)
       }

       pub async fn is_task_cancelled(&self, task_id: i64) -> bool {
           let cancel_id = self.cancel_task_id.lock().await;
           cancel_id.map(|id| id == task_id).unwrap_or(false)
       }
   }
   ```

2. **Integrate Control Handler into Worker**:
   ```rust
   impl Worker {
       pub async fn run(mut self) -> Result<()> {
           // Initialize control handler (managed mode only)
           let mut control_handler = if let Some(subscriber) = self.control_subscriber {
               Some(ControlHandler::new(subscriber))
           } else {
               None
           };

           loop {
               // Check for control messages (managed mode)
               if let Some(ref mut handler) = control_handler {
                   handler.poll_messages().await?;

                   if handler.is_shutdown_requested() {
                       tracing::info!("Shutdown requested, finishing current task");
                       if let Some(task) = &self.current_task {
                           self.finish_current_task().await?;
                       }
                       break;
                   }
               }

               // Main worker loop continues...
               // (fetch task, execute, report)
           }

           Ok(())
       }
   }
   ```

**Testing**:
- Unit test: Send `ControlMessage::Shutdown{graceful:true}`, verify worker exits after current task
- Unit test: Send `ControlMessage::CancelTask`, verify task is cancelled
- Integration test: Manager sends shutdown, worker exits cleanly

---

### Task 7.5: Update Worker Main Loop

**Objective**: Integrate trait-based communication and managed mode lifecycle.

**Files to Modify**:
- `crates/mitosis-worker/src/worker.rs`

**Implementation Steps**:

1. **Refactor Main Loop to Use Trait**:
   ```rust
   impl Worker {
       pub async fn run(mut self) -> Result<()> {
           tracing::info!("Worker starting: {:?}", self.worker_id);

           // Initialize control handler
           let mut control_handler = self.control_subscriber
               .take()
               .map(|sub| ControlHandler::new(sub));

           // Main execution loop
           loop {
               // 1. Check for control messages (managed mode only)
               if let Some(ref mut handler) = control_handler {
                   handler.poll_messages().await?;

                   if handler.is_shutdown_requested() {
                       tracing::info!("Graceful shutdown requested");
                       self.finish_current_task().await?;
                       break;
                   }
               }

               // 2. Fetch next task (via trait abstraction)
               let task = match self.comm.fetch_task().await {
                   Ok(Some(task)) => task,
                   Ok(None) => {
                       // No task available, wait and retry
                       tokio::time::sleep(Duration::from_secs(5)).await;
                       continue;
                   }
                   Err(e) => {
                       tracing::error!("Failed to fetch task: {}", e);
                       tokio::time::sleep(Duration::from_secs(10)).await;
                       continue;
                   }
               };

               tracing::info!("Fetched task: id={}", task.id);

               // 3. Report task started
               if let Err(e) = self.comm.report_task(task.id, ReportTaskOp::Started).await {
                   tracing::error!("Failed to report task started: {}", e);
                   continue;
               }

               // 4. Execute task
               self.current_task = Some(ActiveTask {
                   id: task.id,
                   started_at: Instant::now(),
               });

               let result = self.executor.execute_task(&task).await;

               // 5. Check if task was cancelled during execution
               if let Some(ref handler) = control_handler {
                   if handler.is_task_cancelled(task.id).await {
                       tracing::warn!("Task {} was cancelled", task.id);
                       // Report cancellation
                       let _ = self.comm.report_task(
                           task.id,
                           ReportTaskOp::Failed {
                               error: "Task cancelled by manager".to_string(),
                           },
                       ).await;
                       self.current_task = None;
                       continue;
                   }
               }

               // 6. Report task result
               let report_op = match result {
                   Ok(output) => {
                       self.tasks_completed.fetch_add(1, Ordering::SeqCst);
                       ReportTaskOp::Completed { output }
                   }
                   Err(e) => {
                       self.tasks_failed.fetch_add(1, Ordering::SeqCst);
                       ReportTaskOp::Failed { error: e.to_string() }
                   }
               };

               if let Err(e) = self.comm.report_task(task.id, report_op).await {
                   tracing::error!("Failed to report task result: {}", e);
               }

               self.current_task = None;

               // 7. Send heartbeat (independent mode only)
               if matches!(self.worker_id, WorkerId::Independent(_)) {
                   if let Err(e) = self.comm.send_heartbeat().await {
                       tracing::error!("Failed to send heartbeat: {}", e);
                   }
               }
           }

           tracing::info!("Worker shutting down");
           Ok(())
       }

       async fn finish_current_task(&mut self) -> Result<()> {
           if let Some(task) = &self.current_task {
               tracing::info!("Waiting for current task {} to complete", task.id);
               // Wait up to 30 seconds for task to finish
               let timeout = Duration::from_secs(30);
               tokio::time::timeout(timeout, async {
                   // Poll executor for completion
                   while !self.executor.is_finished() {
                       tokio::time::sleep(Duration::from_millis(100)).await;
                   }
               }).await.ok();
           }
           Ok(())
       }
   }
   ```

2. **Add Logging and Metrics**:
   ```rust
   impl Worker {
       fn log_startup_info(&self) {
           match &self.worker_id {
               WorkerId::Independent(uuid) => {
                   tracing::info!("Worker mode: Independent");
                   tracing::info!("Worker UUID: {}", uuid);
               }
               WorkerId::Managed { manager_uuid, local_id } => {
                   tracing::info!("Worker mode: Managed");
                   tracing::info!("Manager UUID: {}", manager_uuid);
                   tracing::info!("Worker local ID: {}", local_id);
               }
           }
       }

       fn log_metrics(&self) {
           let completed = self.tasks_completed.load(Ordering::SeqCst);
           let failed = self.tasks_failed.load(Ordering::SeqCst);
           tracing::info!(
               "Worker metrics: completed={}, failed={}",
               completed,
               failed
           );
       }
   }
   ```

**Testing**:
- Unit test: Verify worker fetches task via trait
- Unit test: Verify worker reports task via trait
- Integration test: Run full cycle (fetch → execute → report) in managed mode
- Integration test: Verify worker respects graceful shutdown

---

## Testing Checklist

### Unit Tests

- [ ] CLI argument parsing
  - [ ] Valid managed mode args accepted
  - [ ] Invalid managed mode args rejected
  - [ ] Independent mode args still work
  - [ ] worker_local_id range validation (0-255)

- [ ] WorkerComm trait
  - [ ] `HttpWorkerComm` implements trait correctly
  - [ ] `IpcWorkerComm` implements trait correctly
  - [ ] Trait objects can be created: `Box<dyn WorkerComm>`

- [ ] IpcWorkerComm
  - [ ] Service name generation correct
  - [ ] Request serialization/deserialization
  - [ ] Request ID counter increments

- [ ] Control message handling
  - [ ] Graceful shutdown sets flag
  - [ ] Immediate shutdown exits process
  - [ ] Task cancellation sets cancel ID
  - [ ] Pause/resume messages handled

### Integration Tests

- [ ] Managed worker connects to manager via IPC
  - [ ] Worker can connect to task_fetch service
  - [ ] Worker can connect to task_report service
  - [ ] Worker can subscribe to control topic
  - [ ] Connection fails if manager not running

- [ ] Task fetch via IPC
  - [ ] Worker fetches task from manager
  - [ ] Manager returns task from local buffer
  - [ ] Manager returns None when no tasks available

- [ ] Task report via IPC
  - [ ] Worker reports task started
  - [ ] Worker reports task completed
  - [ ] Worker reports task failed
  - [ ] Manager proxies report to coordinator

- [ ] Control messages
  - [ ] Worker receives graceful shutdown
  - [ ] Worker finishes current task before exiting
  - [ ] Worker receives immediate shutdown and exits
  - [ ] Worker receives cancel task and aborts execution

- [ ] Full lifecycle
  - [ ] Manager spawns worker with correct args
  - [ ] Worker connects to IPC services
  - [ ] Worker fetches and executes tasks
  - [ ] Worker respects shutdown signal
  - [ ] Worker logs metrics on exit

### End-to-End Tests

- [ ] Suite execution with managed workers
  - [ ] Create task suite (via coordinator API)
  - [ ] Assign suite to manager (via WebSocket)
  - [ ] Manager spawns N workers
  - [ ] Workers fetch tasks via IPC
  - [ ] Workers execute and report via IPC
  - [ ] Suite completes successfully

- [ ] Multi-worker scenarios
  - [ ] 1 managed worker executes suite
  - [ ] 16 managed workers execute suite in parallel
  - [ ] 256 managed workers (max) execute suite

- [ ] Failure scenarios
  - [ ] Worker crashes, manager respawns
  - [ ] Worker reports task failure
  - [ ] Manager sends shutdown during task execution
  - [ ] Manager kills worker after timeout

### Performance Tests

- [ ] IPC latency
  - [ ] Measure fetch_task roundtrip time
  - [ ] Measure report_task roundtrip time
  - [ ] Compare to HTTP API latency

- [ ] Throughput
  - [ ] Measure tasks/second with 1 worker
  - [ ] Measure tasks/second with 16 workers
  - [ ] Compare managed vs independent mode

## Success Criteria

### Functional Requirements

- [ ] Worker binary accepts `--managed` flag and required args
- [ ] Worker can operate in both independent and managed modes
- [ ] `WorkerComm` trait abstracts communication layer
- [ ] `IpcWorkerComm` successfully communicates with manager
- [ ] Worker handles control messages (shutdown, cancel)
- [ ] Worker main loop uses trait-based communication
- [ ] No code duplication between independent and managed workers

### Quality Requirements

- [ ] All unit tests pass
- [ ] All integration tests pass
- [ ] Code coverage >80% for new code
- [ ] No clippy warnings
- [ ] Documentation updated (inline comments, README)

### Performance Requirements

- [ ] IPC latency <1ms (fetch_task, report_task)
- [ ] Worker can handle 100+ tasks/second (small tasks)
- [ ] No memory leaks during long-running execution
- [ ] Graceful shutdown completes within 30 seconds

### Operational Requirements

- [ ] Worker logs startup mode (independent vs managed)
- [ ] Worker logs IPC connection status
- [ ] Worker logs task execution metrics
- [ ] Worker logs shutdown reason
- [ ] Error messages are actionable

## Dependencies

### Required Phases
- **Phase 6**: Worker Spawning and IPC
  - Manager's IPC services must be operational
  - iceoryx2 setup complete
  - Service naming conventions established

### Blocking Issues
- If Phase 6 IPC services not working, Phase 7 cannot begin
- If iceoryx2 dependency issues, resolve before starting Phase 7

### External Dependencies
- `iceoryx2` crate (already added in Phase 6)
- `async-trait` crate
- `clap` crate (for CLI parsing)

## Next Phase

**Phase 8**: Integration and E2E Testing (1 week)

Phase 8 will test the complete system end-to-end:
- Suite creation → manager assignment → task execution → cleanup
- Multi-manager scenarios (10 managers competing for suites)
- Failure scenarios (worker crash, manager disconnect, task timeout)
- Performance benchmarks (latency, throughput, resource usage)

Phase 8 depends on Phase 7 completion because it requires functional managed workers.

## Notes

### Design Decisions

1. **Trait-based abstraction**: Enables code reuse between independent and managed workers. The core execution logic remains identical.

2. **Optional heartbeat**: Managed workers don't send heartbeats because the manager tracks process health directly via `waitpid()`.

3. **Graceful shutdown**: Workers finish their current task before exiting, preventing task loss and partial results.

4. **Request ID counter**: Used for debugging and request-response matching (though iceoryx2 handles this internally).

### Common Issues

1. **Service not found**: Ensure manager UUID matches exactly in service names.
2. **Permission denied**: Ensure iceoryx2 shared memory has correct permissions.
3. **Connection timeout**: Increase timeout if manager is slow to start services.
4. **Worker hangs on shutdown**: Implement timeout for `finish_current_task()`.

### Future Enhancements

- **Dynamic mode switching**: Allow worker to switch between independent and managed modes at runtime.
- **Hybrid mode**: Worker registers with coordinator but uses IPC for task communication.
- **Performance monitoring**: Add metrics for IPC message rates and latencies.
- **Graceful restart**: Support hot-reload of worker binary without task loss.

---

## References

- **RFC Section 4.3**: Worker Modes (Independent vs Managed)
- **RFC Section 8.4**: IPC Communication Protocols
- **RFC Section 9.2**: Worker Lifecycle and State Machine
- **Phase 6 Implementation Guide**: Worker Spawning and IPC (Manager-side)
- **iceoryx2 Documentation**: https://iceoryx.io/v2.0.0/
