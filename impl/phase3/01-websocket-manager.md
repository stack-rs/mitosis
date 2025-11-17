# Phase 3: WebSocket Manager Implementation Guide

## Overview

Phase 3 introduces the WebSocket communication layer between the Coordinator and Node Managers. This replaces the polling-based HTTP communication model with a persistent, bidirectional WebSocket connection that enables:

- **Push-based task assignment**: Coordinator can proactively assign task suites to managers
- **Request-response multiplexing**: Multiple concurrent requests over a single connection
- **Real-time heartbeats**: Continuous health monitoring without HTTP overhead
- **Low-latency task fetching**: Managed workers fetch tasks via IPC → WebSocket (vs HTTP polling)
- **Immediate cancellation**: Coordinator can instantly signal task/suite cancellations

The WebSocket manager maintains a registry of all connected node managers (`manager_uuid → WebSocket`) and handles message serialization, routing, and request-response correlation.

## Prerequisites

### Phase 1: Database Schema (Completed)
- `task_suites` table
- `node_managers` table
- `group_node_manager` table
- `task_suite_managers` table
- `task_execution_failures` table
- Database triggers for suite state transitions
- SeaORM entity models for all tables
- Enums: `TaskSuiteState`, `NodeManagerState`, `SelectionType`

### Phase 2: API Schema and Coordinator Endpoints (Completed)
- Rust types in `schema.rs`:
  - `CreateTaskSuiteReq/Resp`
  - `TaskSuiteQueryReq/Resp`
  - `RegisterManagerReq/Resp`
  - `ManagerQueryReq/Resp`
  - `WorkerSchedulePlan`
  - `EnvHookSpec`
- Suite management APIs (create, query, cancel, manage assignments)
- Manager registration APIs (register, heartbeat, query, shutdown, token refresh)
- Updated task submission API with `suite_uuid` field

### Required Knowledge
- Understanding of WebSocket protocol (RFC 6455)
- Async Rust and tokio runtime
- tokio-tungstenite for WebSocket implementation
- Request-response multiplexing patterns
- JWT authentication
- Graceful error handling and reconnection strategies

## Timeline

**Duration:** 1 week

## Design References

### Section 8.1: WebSocket Protocol (Manager ↔ Coordinator)

#### Connection Establishment

```
Manager                                  Coordinator
   │                                          │
   │ WS Upgrade: /ws/managers                │
   │ Authorization: Bearer <manager-jwt>     │
   ├─────────────────────────────────────────>│
   │                                          │
   │ 101 Switching Protocols                 │
   │<─────────────────────────────────────────┤
   │                                          │
   │ Connected                                │
   │                                          │
```

**Key Points:**
- WebSocket endpoint: `/ws/managers`
- Authentication: JWT token in `Authorization: Bearer <token>` header
- Token obtained via `POST /managers` registration
- Token contains `manager_uuid` claim for identity verification
- Connection persists for duration of manager session

#### Message Format

##### Manager → Coordinator Messages

```rust
// Manager → Coordinator
#[derive(Serialize, Deserialize)]
#[serde(tag = "type")]
enum ManagerMessage {
    Heartbeat {
        manager_uuid: Uuid,
        state: NodeManagerState,
        metrics: ManagerMetrics,
    },

    FetchTask {
        request_id: u64,
        worker_local_id: u32,  // Which managed worker is requesting
    },

    ReportTask {
        request_id: u64,
        task_id: i64,
        op: ReportTaskOp,
    },

    ReportFailure {
        task_uuid: Uuid,
        failure_count: u32,
        error_message: String,
        worker_local_id: u32,
    },

    AbortTask {
        task_uuid: Uuid,
        reason: String,
    },

    SuiteCompleted {
        suite_uuid: Uuid,
        tasks_completed: u64,
        tasks_failed: u64,
    },
}
```

**Supporting Types:**

```rust
pub enum NodeManagerState {
    Idle = 0,       // No suite assigned, waiting for work
    Preparing = 1,  // Running env_preparation hook
    Executing = 2,  // Workers running tasks
    Cleanup = 3,    // Running env_cleanup hook
    Offline = 4,    // Heartbeat timeout
}

// Define based on metrics you want to track
pub struct ManagerMetrics {
    pub active_workers: u32,
    pub total_tasks_completed: u64,
    pub total_tasks_failed: u64,
    pub current_suite_tasks_completed: u64,
    pub current_suite_tasks_failed: u64,
    pub uptime_seconds: u64,
    pub cpu_usage_percent: f32,
    pub memory_usage_mb: u64,
}

// Reuse from existing worker schema
pub enum ReportTaskOp {
    Finish { exit_code: i32 },
    Cancel { reason: String },
    Commit,
    Upload { artifact_path: String },
}
```

##### Coordinator → Manager Messages

```rust
// Coordinator → Manager
#[derive(Serialize, Deserialize)]
#[serde(tag = "type")]
enum CoordinatorMessage {
    SuiteAssigned {
        suite_uuid: Uuid,
        suite_spec: TaskSuiteSpec,
    },

    TaskAvailable {
        request_id: u64,
        task: Option<WorkerTaskResp>,
    },

    TaskReportAck {
        request_id: u64,
        success: bool,
        url: Option<String>,  // For upload operations
    },

    CancelTask {
        task_uuid: Uuid,
        reason: String,
    },

    CancelSuite {
        suite_uuid: Uuid,
        reason: String,
        cancel_running_tasks: bool,
    },

    ConfigUpdate {
        lease_duration: Option<Duration>,
        heartbeat_interval: Option<Duration>,
    },

    Shutdown {
        graceful: bool,
    },
}
```

**Supporting Types:**

```rust
pub struct TaskSuiteSpec {
    pub uuid: Uuid,
    pub name: String,
    pub description: String,
    pub group_id: i64,
    pub tags: HashSet<String>,
    pub labels: HashSet<String>,
    pub priority: i32,
    pub worker_schedule: WorkerSchedulePlan,
    pub env_preparation: Option<EnvHookSpec>,
    pub env_cleanup: Option<EnvHookSpec>,
}

pub struct WorkerSchedulePlan {
    pub worker_count: u32,
    pub cpu_binding: Option<CpuBindingStrategy>,
    pub task_prefetch_count: u32,
}

pub struct CpuBindingStrategy {
    pub cores: Vec<u32>,
    pub strategy: BindingMode,  // RoundRobin, Exclusive, Shared
}

pub struct EnvHookSpec {
    pub args: Vec<String>,
    pub envs: HashMap<String, String>,
    pub resources: Vec<ResourceSpec>,  // S3 artifacts to download
    pub timeout: Duration,
}

// Reuse from existing worker schema
pub struct WorkerTaskResp {
    pub task_id: i64,
    pub task_uuid: Uuid,
    pub spec: TaskSpec,
    pub priority: i32,
    // ... other fields from existing schema
}
```

#### Request-Response Multiplexing

Managers send multiple concurrent requests over a single WebSocket connection. The coordinator responds out-of-order as operations complete.

**Client-side Implementation Pattern:**

```rust
struct ManagerWebSocketClient {
    ws: WebSocketStream,
    pending_requests: Arc<Mutex<HashMap<RequestId, oneshot::Sender<Response>>>>,
    next_request_id: AtomicU64,
}

impl ManagerWebSocketClient {
    async fn send_request<T>(&self, request: T) -> Result<T::Response> {
        let request_id = self.next_request_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();

        self.pending_requests.lock().await.insert(request_id, tx);

        let msg = ManagerMessage::with_request_id(request, request_id);
        self.ws.send(msg).await?;

        // Wait for response (with timeout)
        tokio::time::timeout(Duration::from_secs(30), rx).await??
    }

    async fn receive_loop(&self) {
        loop {
            let msg = self.ws.next().await?;

            if let Some(tx) = self.pending_requests.lock().await.remove(&msg.request_id) {
                let _ = tx.send(msg.response);
            }
        }
    }
}
```

**Example Flow:**

```
Worker1 → Manager: IPC FetchTask
Worker2 → Manager: IPC FetchTask
Worker3 → Manager: IPC ReportTask

Manager → Coordinator: WS FetchTask (req_id=1, worker_local_id=1)
Manager → Coordinator: WS FetchTask (req_id=2, worker_local_id=2)
Manager → Coordinator: WS ReportTask (req_id=3, worker_local_id=3, task_id=100)

Coordinator → Manager: WS TaskAvailable (req_id=2, task={...})  // Out of order OK!
Coordinator → Manager: WS TaskAvailable (req_id=1, task={...})
Coordinator → Manager: WS TaskReportAck (req_id=3, success=true)

Manager → Worker2: IPC TaskResp
Manager → Worker1: IPC TaskResp
Manager → Worker3: IPC ReportAck
```

#### Reconnection Strategy

Managers implement exponential backoff reconnection with state recovery:

```rust
impl NodeManager {
    async fn maintain_websocket_connection(&mut self) -> Result<()> {
        let mut backoff = Duration::from_secs(1);

        loop {
            match self.connect_websocket().await {
                Ok(ws) => {
                    self.ws = ws;
                    backoff = Duration::from_secs(1);  // Reset backoff

                    // Resume state
                    self.send_heartbeat().await?;

                    // If executing suite, tell coordinator
                    if let Some(suite_id) = self.assigned_suite_id {
                        self.send(ManagerMessage::Heartbeat {
                            manager_uuid: self.uuid,
                            state: NodeManagerState::Executing,
                            metrics: self.get_metrics(),
                        }).await?;
                    }

                    // Run until disconnected
                    self.run_message_loop().await?;
                }
                Err(e) => {
                    tracing::warn!("WebSocket connection failed: {}, retrying in {:?}", e, backoff);
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(60));  // Exponential backoff, max 60s
                }
            }
        }
    }
}
```

#### Heartbeat Timeout Detection

Coordinator monitors manager heartbeats and marks managers offline if heartbeat expires:

```rust
impl Coordinator {
    async fn check_manager_heartbeats(&self) {
        let expired_managers = sqlx::query_as!(
            NodeManagerModel,
            "SELECT * FROM node_managers
             WHERE last_heartbeat < NOW() - INTERVAL '2 minutes'
               AND state != 4"  // Not already marked offline
        )
        .fetch_all(&self.db)
        .await?;

        for manager in expired_managers {
            tracing::warn!(
                manager_uuid = ?manager.uuid,
                last_heartbeat = ?manager.last_heartbeat,
                "Manager heartbeat timeout, marking as offline"
            );

            // Mark as offline
            sqlx::query!(
                "UPDATE node_managers
                 SET state = 4, updated_at = NOW()
                 WHERE id = $1",
                manager.id
            )
            .execute(&self.db)
            .await?;

            // Reclaim assigned suite
            if let Some(suite_id) = manager.assigned_task_suite_id {
                self.reclaim_suite_from_manager(manager.id, suite_id).await?;
            }

            // Return prefetched tasks to queue
            self.return_prefetched_tasks(manager.uuid).await?;

            // Close WebSocket connection
            if let Some(ws) = self.ws_connections.lock().await.remove(&manager.uuid) {
                let _ = ws.close().await;
            }
        }
    }
}
```

## Implementation Tasks

### Task 3.1: WebSocket Server Setup

**Objective:** Set up WebSocket endpoint on coordinator with JWT authentication

**File:** `mitosis-coordinator/src/websocket/mod.rs`

**Steps:**

1. **Add dependencies** to `Cargo.toml`:
   ```toml
   tokio-tungstenite = "0.21"
   futures-util = "0.3"
   ```

2. **Create WebSocket upgrade handler**:
   ```rust
   use tokio_tungstenite::tungstenite::protocol::Message;
   use tokio_tungstenite::WebSocketStream;
   use futures_util::{StreamExt, SinkExt};
   use axum::extract::ws::{WebSocket, WebSocketUpgrade};
   use axum::extract::State;
   use axum::response::IntoResponse;

   pub async fn websocket_handler(
       ws: WebSocketUpgrade,
       State(state): State<Arc<AppState>>,
       TypedHeader(auth): TypedHeader<Authorization<Bearer>>,
   ) -> impl IntoResponse {
       // Verify JWT token
       let token = auth.token();
       let claims = match verify_manager_jwt(token, &state.jwt_secret) {
           Ok(claims) => claims,
           Err(_) => return Err(StatusCode::UNAUTHORIZED),
       };

       let manager_uuid = claims.manager_uuid;

       // Upgrade to WebSocket
       ws.on_upgrade(move |socket| {
           handle_manager_socket(socket, manager_uuid, state)
       })
   }
   ```

3. **Register WebSocket route** in `main.rs`:
   ```rust
   let app = Router::new()
       .route("/ws/managers", get(websocket_handler))
       // ... other routes
   ```

**Success Criteria:**
- WebSocket endpoint accepts connections at `/ws/managers`
- JWT authentication rejects invalid tokens
- Valid tokens extract `manager_uuid` claim correctly

---

### Task 3.2: Connection Management

**Objective:** Maintain registry of active manager connections and handle lifecycle events

**File:** `mitosis-coordinator/src/websocket/connection_registry.rs`

**Implementation:**

```rust
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

pub type WsSender = Arc<RwLock<SplitSink<WebSocket, Message>>>;

pub struct ConnectionRegistry {
    connections: Arc<RwLock<HashMap<Uuid, WsSender>>>,
}

impl ConnectionRegistry {
    pub fn new() -> Self {
        Self {
            connections: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn register(&self, manager_uuid: Uuid, sender: WsSender) {
        self.connections.write().await.insert(manager_uuid, sender);
        tracing::info!(manager_uuid = %manager_uuid, "Manager WebSocket connected");
    }

    pub async fn unregister(&self, manager_uuid: &Uuid) {
        self.connections.write().await.remove(manager_uuid);
        tracing::info!(manager_uuid = %manager_uuid, "Manager WebSocket disconnected");
    }

    pub async fn get(&self, manager_uuid: &Uuid) -> Option<WsSender> {
        self.connections.read().await.get(manager_uuid).cloned()
    }

    pub async fn broadcast(&self, message: &CoordinatorMessage) -> Result<()> {
        let connections = self.connections.read().await;
        let msg_json = serde_json::to_string(message)?;

        for (uuid, sender) in connections.iter() {
            if let Err(e) = sender.write().await.send(Message::Text(msg_json.clone())).await {
                tracing::error!(manager_uuid = %uuid, error = %e, "Failed to broadcast message");
            }
        }
        Ok(())
    }

    pub async fn send_to_manager(&self, manager_uuid: &Uuid, message: &CoordinatorMessage) -> Result<()> {
        if let Some(sender) = self.get(manager_uuid).await {
            let msg_json = serde_json::to_string(message)?;
            sender.write().await.send(Message::Text(msg_json)).await?;
            Ok(())
        } else {
            Err(anyhow!("Manager not connected: {}", manager_uuid))
        }
    }
}
```

**Connection Lifecycle Handler:**

```rust
async fn handle_manager_socket(
    socket: WebSocket,
    manager_uuid: Uuid,
    state: Arc<AppState>,
) {
    let (sender, mut receiver) = socket.split();
    let sender = Arc::new(RwLock::new(sender));

    // Register connection
    state.ws_registry.register(manager_uuid, sender.clone()).await;

    // Update database: mark manager as online
    if let Err(e) = sqlx::query!(
        "UPDATE node_managers SET state = $1, last_heartbeat = NOW(), updated_at = NOW() WHERE uuid = $2",
        NodeManagerState::Idle as i32,
        manager_uuid
    )
    .execute(&state.db)
    .await {
        tracing::error!(manager_uuid = %manager_uuid, error = %e, "Failed to update manager state");
    }

    // Handle incoming messages
    while let Some(msg) = receiver.next().await {
        match msg {
            Ok(Message::Text(text)) => {
                if let Err(e) = handle_manager_message(&text, manager_uuid, &state).await {
                    tracing::error!(manager_uuid = %manager_uuid, error = %e, "Error handling message");
                }
            }
            Ok(Message::Close(_)) => {
                tracing::info!(manager_uuid = %manager_uuid, "Manager closed connection");
                break;
            }
            Ok(Message::Ping(data)) => {
                if let Err(e) = sender.write().await.send(Message::Pong(data)).await {
                    tracing::error!(manager_uuid = %manager_uuid, error = %e, "Failed to send pong");
                    break;
                }
            }
            Err(e) => {
                tracing::error!(manager_uuid = %manager_uuid, error = %e, "WebSocket error");
                break;
            }
            _ => {}
        }
    }

    // Cleanup on disconnect
    state.ws_registry.unregister(&manager_uuid).await;

    // Mark manager as offline
    if let Err(e) = sqlx::query!(
        "UPDATE node_managers SET state = $1, updated_at = NOW() WHERE uuid = $2",
        NodeManagerState::Offline as i32,
        manager_uuid
    )
    .execute(&state.db)
    .await {
        tracing::error!(manager_uuid = %manager_uuid, error = %e, "Failed to mark manager offline");
    }
}
```

**Success Criteria:**
- Connections registered on connect, unregistered on disconnect
- Database updated with manager state changes
- Graceful handling of connection close and errors

---

### Task 3.3: Message Protocol Implementation

**Objective:** Serialize/deserialize messages and route to appropriate handlers

**File:** `mitosis-coordinator/src/websocket/messages.rs`

**Message Definitions:**

```rust
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use std::time::Duration;

// Manager → Coordinator
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ManagerMessage {
    Heartbeat {
        manager_uuid: Uuid,
        state: NodeManagerState,
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
        task_uuid: Uuid,
        failure_count: u32,
        error_message: String,
        worker_local_id: u32,
    },
    AbortTask {
        task_uuid: Uuid,
        reason: String,
    },
    SuiteCompleted {
        suite_uuid: Uuid,
        tasks_completed: u64,
        tasks_failed: u64,
    },
}

// Coordinator → Manager
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CoordinatorMessage {
    SuiteAssigned {
        suite_uuid: Uuid,
        suite_spec: TaskSuiteSpec,
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
        task_uuid: Uuid,
        reason: String,
    },
    CancelSuite {
        suite_uuid: Uuid,
        reason: String,
        cancel_running_tasks: bool,
    },
    ConfigUpdate {
        lease_duration: Option<Duration>,
        heartbeat_interval: Option<Duration>,
    },
    Shutdown {
        graceful: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagerMetrics {
    pub active_workers: u32,
    pub total_tasks_completed: u64,
    pub total_tasks_failed: u64,
    pub current_suite_tasks_completed: u64,
    pub current_suite_tasks_failed: u64,
    pub uptime_seconds: u64,
    pub cpu_usage_percent: f32,
    pub memory_usage_mb: u64,
}

impl Default for ManagerMetrics {
    fn default() -> Self {
        Self {
            active_workers: 0,
            total_tasks_completed: 0,
            total_tasks_failed: 0,
            current_suite_tasks_completed: 0,
            current_suite_tasks_failed: 0,
            uptime_seconds: 0,
            cpu_usage_percent: 0.0,
            memory_usage_mb: 0,
        }
    }
}
```

**Message Router:**

```rust
async fn handle_manager_message(
    text: &str,
    manager_uuid: Uuid,
    state: &Arc<AppState>,
) -> Result<()> {
    let message: ManagerMessage = serde_json::from_str(text)?;

    match message {
        ManagerMessage::Heartbeat { manager_uuid, state: mgr_state, metrics } => {
            handle_heartbeat(manager_uuid, mgr_state, metrics, state).await?;
        }
        ManagerMessage::FetchTask { request_id, worker_local_id } => {
            handle_fetch_task(manager_uuid, request_id, worker_local_id, state).await?;
        }
        ManagerMessage::ReportTask { request_id, task_id, op } => {
            handle_report_task(manager_uuid, request_id, task_id, op, state).await?;
        }
        ManagerMessage::ReportFailure { task_uuid, failure_count, error_message, worker_local_id } => {
            handle_report_failure(manager_uuid, task_uuid, failure_count, error_message, worker_local_id, state).await?;
        }
        ManagerMessage::AbortTask { task_uuid, reason } => {
            handle_abort_task(manager_uuid, task_uuid, reason, state).await?;
        }
        ManagerMessage::SuiteCompleted { suite_uuid, tasks_completed, tasks_failed } => {
            handle_suite_completed(manager_uuid, suite_uuid, tasks_completed, tasks_failed, state).await?;
        }
    }

    Ok(())
}
```

**Success Criteria:**
- All message types deserialize correctly
- Invalid JSON returns appropriate errors
- Messages route to correct handlers

---

### Task 3.4: Coordinator Message Handlers

**Objective:** Implement business logic for each manager message type

**File:** `mitosis-coordinator/src/websocket/handlers.rs`

#### Handler: ManagerMessage::Heartbeat

```rust
async fn handle_heartbeat(
    manager_uuid: Uuid,
    state: NodeManagerState,
    metrics: ManagerMetrics,
    app_state: &Arc<AppState>,
) -> Result<()> {
    tracing::debug!(
        manager_uuid = %manager_uuid,
        state = ?state,
        active_workers = metrics.active_workers,
        "Received heartbeat"
    );

    // Update manager state and last_heartbeat in database
    sqlx::query!(
        "UPDATE node_managers
         SET state = $1, last_heartbeat = NOW(), updated_at = NOW()
         WHERE uuid = $2",
        state as i32,
        manager_uuid
    )
    .execute(&app_state.db)
    .await?;

    // Optionally store metrics (future enhancement)
    // store_manager_metrics(manager_uuid, metrics).await?;

    Ok(())
}
```

#### Handler: ManagerMessage::FetchTask

```rust
async fn handle_fetch_task(
    manager_uuid: Uuid,
    request_id: u64,
    worker_local_id: u32,
    app_state: &Arc<AppState>,
) -> Result<()> {
    tracing::debug!(
        manager_uuid = %manager_uuid,
        request_id = request_id,
        worker_local_id = worker_local_id,
        "Fetching task for worker"
    );

    // Get manager's assigned suite
    let manager = sqlx::query_as!(
        NodeManagerModel,
        "SELECT * FROM node_managers WHERE uuid = $1",
        manager_uuid
    )
    .fetch_one(&app_state.db)
    .await?;

    let task = if let Some(suite_id) = manager.assigned_task_suite_id {
        // Fetch task from assigned suite
        app_state.task_dispatcher.fetch_task_from_suite(suite_id, manager.id).await?
    } else {
        None
    };

    // Send response
    let response = CoordinatorMessage::TaskAvailable {
        request_id,
        task,
    };

    app_state.ws_registry.send_to_manager(&manager_uuid, &response).await?;

    Ok(())
}
```

#### Handler: ManagerMessage::ReportTask

```rust
async fn handle_report_task(
    manager_uuid: Uuid,
    request_id: u64,
    task_id: i64,
    op: ReportTaskOp,
    app_state: &Arc<AppState>,
) -> Result<()> {
    tracing::debug!(
        manager_uuid = %manager_uuid,
        request_id = request_id,
        task_id = task_id,
        op = ?op,
        "Reporting task"
    );

    // Process task report (reuse existing worker logic)
    let result = match op {
        ReportTaskOp::Finish { exit_code } => {
            app_state.task_dispatcher.finish_task(task_id, exit_code).await
        }
        ReportTaskOp::Cancel { reason } => {
            app_state.task_dispatcher.cancel_task(task_id, reason).await
        }
        ReportTaskOp::Commit => {
            app_state.task_dispatcher.commit_task(task_id).await
        }
        ReportTaskOp::Upload { artifact_path } => {
            app_state.task_dispatcher.upload_task_artifact(task_id, artifact_path).await
        }
    };

    let (success, url) = match result {
        Ok(upload_url) => (true, upload_url),
        Err(e) => {
            tracing::error!(task_id = task_id, error = %e, "Failed to report task");
            (false, None)
        }
    };

    // Send acknowledgment
    let response = CoordinatorMessage::TaskReportAck {
        request_id,
        success,
        url,
    };

    app_state.ws_registry.send_to_manager(&manager_uuid, &response).await?;

    Ok(())
}
```

#### Handler: ManagerMessage::ReportFailure

```rust
async fn handle_report_failure(
    manager_uuid: Uuid,
    task_uuid: Uuid,
    failure_count: u32,
    error_message: String,
    worker_local_id: u32,
    app_state: &Arc<AppState>,
) -> Result<()> {
    tracing::warn!(
        manager_uuid = %manager_uuid,
        task_uuid = %task_uuid,
        failure_count = failure_count,
        worker_local_id = worker_local_id,
        error = error_message,
        "Task failure reported"
    );

    // Get manager details
    let manager = sqlx::query_as!(
        NodeManagerModel,
        "SELECT * FROM node_managers WHERE uuid = $1",
        manager_uuid
    )
    .fetch_one(&app_state.db)
    .await?;

    // Record failure in task_execution_failures table
    sqlx::query!(
        "INSERT INTO task_execution_failures
         (task_suite_id, task_uuid, node_manager_id, failure_count, error_message, worker_local_id, occurred_at)
         VALUES ($1, $2, $3, $4, $5, $6, NOW())",
        manager.assigned_task_suite_id,
        task_uuid,
        manager.id,
        failure_count as i32,
        error_message,
        worker_local_id as i32
    )
    .execute(&app_state.db)
    .await?;

    Ok(())
}
```

#### Handler: ManagerMessage::AbortTask

```rust
async fn handle_abort_task(
    manager_uuid: Uuid,
    task_uuid: Uuid,
    reason: String,
    app_state: &Arc<AppState>,
) -> Result<()> {
    tracing::info!(
        manager_uuid = %manager_uuid,
        task_uuid = %task_uuid,
        reason = reason,
        "Task aborted"
    );

    // Mark task as aborted
    app_state.task_dispatcher.abort_task(&task_uuid, &reason).await?;

    Ok(())
}
```

#### Handler: ManagerMessage::SuiteCompleted

```rust
async fn handle_suite_completed(
    manager_uuid: Uuid,
    suite_uuid: Uuid,
    tasks_completed: u64,
    tasks_failed: u64,
    app_state: &Arc<AppState>,
) -> Result<()> {
    tracing::info!(
        manager_uuid = %manager_uuid,
        suite_uuid = %suite_uuid,
        tasks_completed = tasks_completed,
        tasks_failed = tasks_failed,
        "Suite execution completed"
    );

    // Update manager state to Cleanup
    sqlx::query!(
        "UPDATE node_managers
         SET state = $1, assigned_task_suite_id = NULL, lease_expires_at = NULL, updated_at = NOW()
         WHERE uuid = $2",
        NodeManagerState::Cleanup as i32,
        manager_uuid
    )
    .execute(&app_state.db)
    .await?;

    // Check if suite is fully complete
    let suite = sqlx::query!(
        "SELECT pending_tasks FROM task_suites WHERE uuid = $1",
        suite_uuid
    )
    .fetch_one(&app_state.db)
    .await?;

    if suite.pending_tasks == 0 {
        // Mark suite as complete
        sqlx::query!(
            "UPDATE task_suites
             SET state = $1, updated_at = NOW()
             WHERE uuid = $2",
            TaskSuiteState::Complete as i32,
            suite_uuid
        )
        .execute(&app_state.db)
        .await?;

        tracing::info!(suite_uuid = %suite_uuid, "Suite fully completed");
    }

    Ok(())
}
```

**Success Criteria:**
- All handlers update database correctly
- Handlers send appropriate responses back to manager
- Error handling logs and propagates errors appropriately

---

### Task 3.5: Manager Disconnection Handling

**Objective:** Detect manager failures and reclaim resources

**File:** `mitosis-coordinator/src/websocket/health_monitor.rs`

**Heartbeat Monitor:**

```rust
use tokio::time::{interval, Duration};

pub struct HealthMonitor {
    db: Pool<Postgres>,
    ws_registry: Arc<ConnectionRegistry>,
    task_dispatcher: Arc<TaskDispatcher>,
}

impl HealthMonitor {
    pub fn new(
        db: Pool<Postgres>,
        ws_registry: Arc<ConnectionRegistry>,
        task_dispatcher: Arc<TaskDispatcher>,
    ) -> Self {
        Self {
            db,
            ws_registry,
            task_dispatcher,
        }
    }

    pub async fn run(&self) {
        let mut ticker = interval(Duration::from_secs(30));

        loop {
            ticker.tick().await;

            if let Err(e) = self.check_manager_heartbeats().await {
                tracing::error!(error = %e, "Error checking manager heartbeats");
            }
        }
    }

    async fn check_manager_heartbeats(&self) -> Result<()> {
        let expired_managers = sqlx::query_as!(
            NodeManagerModel,
            "SELECT * FROM node_managers
             WHERE last_heartbeat < NOW() - INTERVAL '2 minutes'
               AND state != $1",
            NodeManagerState::Offline as i32
        )
        .fetch_all(&self.db)
        .await?;

        for manager in expired_managers {
            tracing::warn!(
                manager_uuid = ?manager.uuid,
                last_heartbeat = ?manager.last_heartbeat,
                "Manager heartbeat timeout, marking as offline"
            );

            // Mark as offline
            sqlx::query!(
                "UPDATE node_managers
                 SET state = $1, updated_at = NOW()
                 WHERE id = $2",
                NodeManagerState::Offline as i32,
                manager.id
            )
            .execute(&self.db)
            .await?;

            // Reclaim assigned suite
            if let Some(suite_id) = manager.assigned_task_suite_id {
                self.reclaim_suite_from_manager(manager.id, suite_id).await?;
            }

            // Close WebSocket connection
            self.ws_registry.unregister(&manager.uuid).await;
        }

        Ok(())
    }

    async fn reclaim_suite_from_manager(
        &self,
        manager_id: i64,
        suite_id: i64,
    ) -> Result<()> {
        tracing::info!(
            manager_id = manager_id,
            suite_id = suite_id,
            "Reclaiming suite from offline manager"
        );

        // Remove suite assignment
        sqlx::query!(
            "DELETE FROM task_suite_managers
             WHERE node_manager_id = $1 AND task_suite_id = $2",
            manager_id,
            suite_id
        )
        .execute(&self.db)
        .await?;

        // Return prefetched tasks to queue
        self.task_dispatcher.return_prefetched_tasks(manager_id).await?;

        // Update suite state if no other managers assigned
        let remaining_managers = sqlx::query_scalar!(
            "SELECT COUNT(*) FROM task_suite_managers WHERE task_suite_id = $1",
            suite_id
        )
        .fetch_one(&self.db)
        .await?;

        if remaining_managers.unwrap_or(0) == 0 {
            tracing::info!(suite_id = suite_id, "No managers remaining for suite");
            // Suite can be reassigned to other managers matching tags
        }

        Ok(())
    }
}
```

**Start Monitor in Coordinator:**

```rust
// In coordinator startup (main.rs or app initialization)
let health_monitor = HealthMonitor::new(
    db.clone(),
    ws_registry.clone(),
    task_dispatcher.clone(),
);

tokio::spawn(async move {
    health_monitor.run().await;
});
```

**Success Criteria:**
- Managers marked offline after 2-minute heartbeat timeout
- Assigned suites reclaimed from offline managers
- Prefetched tasks returned to queue
- WebSocket connections closed properly

---

### Task 3.6: Request-Response Multiplexing

**Objective:** Handle concurrent requests from managers efficiently

**Implementation Notes:**

The coordinator doesn't need explicit multiplexing state since it responds directly to each request. However, it must ensure:

1. **Request ID Preservation**: When coordinator responds, it includes the same `request_id` from the manager's request
2. **Concurrent Processing**: Each handler runs independently, allowing out-of-order responses
3. **Timeout Handling**: Manager-side timeouts (30s) mean coordinator should respond promptly

**Example Flow:**

```rust
// Manager sends:
ManagerMessage::FetchTask { request_id: 1, worker_local_id: 1 }
ManagerMessage::FetchTask { request_id: 2, worker_local_id: 2 }

// Coordinator processes concurrently and responds:
CoordinatorMessage::TaskAvailable { request_id: 2, task: Some(...) }  // Faster query
CoordinatorMessage::TaskAvailable { request_id: 1, task: Some(...) }  // Slower query

// Manager matches responses to waiting channels via request_id
```

**Coordinator Implementation Pattern:**

Each handler receives `request_id` and includes it in the response:

```rust
async fn handle_fetch_task(
    manager_uuid: Uuid,
    request_id: u64,  // <-- Received from manager
    worker_local_id: u32,
    app_state: &Arc<AppState>,
) -> Result<()> {
    // ... fetch task logic ...

    let response = CoordinatorMessage::TaskAvailable {
        request_id,  // <-- Echoed back in response
        task,
    };

    app_state.ws_registry.send_to_manager(&manager_uuid, &response).await?;
    Ok(())
}
```

**Success Criteria:**
- Multiple concurrent requests processed independently
- Responses include correct `request_id`
- Out-of-order responses handled correctly by manager

---

## Testing Checklist

### Unit Tests

- [ ] **Message Serialization/Deserialization**
  - [ ] All `ManagerMessage` variants serialize to JSON
  - [ ] All `CoordinatorMessage` variants serialize to JSON
  - [ ] Invalid JSON returns proper error
  - [ ] Test round-trip serialization (serialize → deserialize → equals original)

- [ ] **Connection Registry**
  - [ ] `register()` adds connection to registry
  - [ ] `unregister()` removes connection from registry
  - [ ] `get()` retrieves correct connection
  - [ ] `broadcast()` sends to all connected managers
  - [ ] `send_to_manager()` sends to specific manager

### Integration Tests

- [ ] **WebSocket Connection/Disconnection**
  - [ ] Manager connects with valid JWT token
  - [ ] Manager rejected with invalid JWT token
  - [ ] Manager UUID extracted from JWT claims
  - [ ] Database updated with manager state on connect
  - [ ] Database updated with manager state on disconnect
  - [ ] Connection removed from registry on disconnect

- [ ] **Message Handling**
  - [ ] Heartbeat message updates `last_heartbeat` in database
  - [ ] FetchTask returns task from assigned suite
  - [ ] FetchTask returns `None` when no tasks available
  - [ ] ReportTask updates task state correctly
  - [ ] ReportTask returns upload URL for Upload operation
  - [ ] ReportFailure inserts row in `task_execution_failures`
  - [ ] AbortTask marks task as aborted
  - [ ] SuiteCompleted clears manager assignment
  - [ ] SuiteCompleted marks suite complete when no pending tasks

- [ ] **Concurrent Requests**
  - [ ] Multiple managers can connect simultaneously
  - [ ] Single manager can send multiple concurrent requests
  - [ ] Responses matched to correct manager
  - [ ] Out-of-order responses handled correctly

- [ ] **Manager Heartbeat Timeout**
  - [ ] Manager marked offline after 2-minute timeout
  - [ ] Assigned suite reclaimed from offline manager
  - [ ] Prefetched tasks returned to queue
  - [ ] WebSocket connection closed

- [ ] **Reconnection After Network Failure**
  - [ ] Manager reconnects after network interruption
  - [ ] Exponential backoff works (1s, 2s, 4s, ..., max 60s)
  - [ ] Manager sends heartbeat on reconnect
  - [ ] Manager resumes suite execution if previously assigned

### End-to-End Tests

- [ ] **Full Suite Execution**
  1. Manager connects
  2. Coordinator assigns suite via `SuiteAssigned` message
  3. Manager fetches tasks via `FetchTask` messages
  4. Manager reports task completions via `ReportTask` messages
  5. Manager sends `SuiteCompleted` when done
  6. Coordinator marks suite complete
  7. Manager returns to Idle state

- [ ] **Suite Cancellation**
  1. Manager executing suite
  2. User cancels suite via API
  3. Coordinator sends `CancelSuite` to manager
  4. Manager stops workers and cleans up
  5. Suite marked as Cancelled

- [ ] **Manager Crash Recovery**
  1. Manager executing suite crashes (kill process)
  2. Coordinator detects heartbeat timeout
  3. Coordinator reclaims suite and returns tasks to queue
  4. Manager restarts and reconnects
  5. Coordinator assigns new suite to recovered manager

## Success Criteria

Phase 3 is complete when:

1. **WebSocket Server Operational**
   - WebSocket endpoint accepts manager connections
   - JWT authentication enforced
   - Connection registry maintains active connections

2. **All Message Types Implemented**
   - All 6 `ManagerMessage` types handled
   - All 7 `CoordinatorMessage` types can be sent
   - Message serialization/deserialization robust

3. **Request-Response Multiplexing Works**
   - Multiple concurrent requests processed
   - Out-of-order responses handled correctly
   - Request timeouts work (30s default)

4. **Heartbeat Monitoring Active**
   - Health monitor runs continuously
   - Managers marked offline after timeout
   - Resources reclaimed from offline managers

5. **All Tests Pass**
   - Unit tests: 100% pass
   - Integration tests: 100% pass
   - End-to-end tests: 100% pass

6. **Documentation Complete**
   - WebSocket protocol documented
   - Message types documented
   - Reconnection strategy documented

## Dependencies

- **Phase 1: Database Schema** (Completed)
  - Tables: `task_suites`, `node_managers`, `group_node_manager`, `task_suite_managers`, `task_execution_failures`
  - Enums: `TaskSuiteState`, `NodeManagerState`

- **Phase 2: API Schema and Coordinator Endpoints** (Completed)
  - Rust types: `TaskSuiteSpec`, `WorkerSchedulePlan`, `EnvHookSpec`
  - Manager registration API: `POST /managers`
  - JWT token generation for managers

## Next Phase

**Phase 4: Node Manager Core (2 weeks)**

After completing Phase 3, you'll implement the Node Manager binary:

- **Manager Binary Setup**: CLI, configuration, logging
- **Registration**: HTTP call to `POST /managers` to obtain JWT token
- **WebSocket Client**: Connect to coordinator, handle messages
- **State Machine**: Implement `NodeManagerState` transitions
- **Suite Lifecycle**: Handle `SuiteAssigned`, run env_preparation hook, spawn workers
- **Worker Management**: Spawn/monitor/respawn managed workers
- **IPC Communication**: iceoryx2-based communication with workers
- **Task Pre-fetching**: Maintain local buffer of tasks
- **Cleanup**: Graceful shutdown, env_cleanup hook

The WebSocket foundation built in Phase 3 enables all real-time communication between coordinator and managers in Phase 4.

---

**End of Phase 3 Implementation Guide**
