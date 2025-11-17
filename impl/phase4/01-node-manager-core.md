# Phase 4: Node Manager Core Implementation Guide

## Overview

Phase 4 implements the core functionality of the Node Manager, a device-level service that manages worker pools and orchestrates task execution. The Node Manager acts as an intermediary between the Coordinator and managed workers, providing resource management, environment lifecycle orchestration, and communication proxying.

**Timeline:** 2 weeks

**Key Deliverables:**
- `mitosis-manager` binary with CLI interface
- Manager registration and JWT authentication
- WebSocket client with reconnection logic
- Node Manager state machine implementation
- Suite assignment handling
- Heartbeat system

## Prerequisites

- **Phase 1:** Core data structures and database schema
- **Phase 2:** Coordinator HTTP APIs
- **Phase 3:** WebSocket infrastructure on coordinator
- **Understanding of:**
  - Process management and lifecycle
  - WebSocket client implementation
  - JWT token management
  - State machines and transitions

## Design References

### Section 4.2: Node Manager Concept

A Node Manager is a device-level service with the following responsibilities:

**Core Functions:**
1. **Manages Worker Pools**: Spawns and monitors local worker processes
2. **Controls Resources**: CPU core binding, memory limits, GPU allocation
3. **Orchestrates Environments**: Executes preparation/cleanup hooks
4. **Proxies Communication**: Relays worker ↔ coordinator messages via WebSocket
5. **Recovers from Failures**: Auto-respawns dead workers, tracks failures

**Lifecycle:**
1. Register with coordinator, receive JWT token
2. Fetch eligible task suite (WebSocket push or poll)
3. Execute `env_preparation` hook
4. Spawn N workers according to `WorkerSchedulePlan`
5. Proxy task fetch/report between workers ↔ coordinator
6. Monitor worker health, auto-respawn on crash
7. Detect suite completion
8. Execute `env_cleanup` hook
9. Fetch next eligible suite

**Failure Handling:**
- Track task failures per worker
- Abort task after 3 worker deaths (return to coordinator)
- Respawn workers on crash
- Reconnect WebSocket on network failure

### Section 5: Architecture Components

#### System Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                         Coordinator                              │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────────────┐  │
│  │TaskDispatcher│  │ SuiteQueue   │  │  HeartbeatQueue      │  │
│  │(Workers)     │  │(Managers)    │  │(Workers + Managers)  │  │
│  └──────────────┘  └──────────────┘  └──────────────────────┘  │
│                                                                   │
│  ┌─────────────────────────────────────────────────────────┐   │
│  │            WebSocket Manager (for Node Managers)         │   │
│  │   - Maintains persistent connections                     │   │
│  │   - Routes messages by manager UUID                      │   │
│  │   - Pushes suite assignments, task cancellations         │   │
│  └─────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────┘
         │                    │
         │ HTTP               │ WebSocket
         ▼                    ▼
┌──────────────────────────────────────────┐
│          Node Manager                     │
│  ┌────────────────────────────────────┐  │
│  │ Manager Process                    │  │
│  │ • WebSocket to coordinator        │  │
│  │ • Fetches task suites            │  │
│  │ • Spawns managed workers         │  │
│  │ • Env preparation/cleanup        │  │
│  │ • IPC proxy to coordinator       │  │
│  │ • Auto-recovery on worker crash  │  │
│  └────────────────────────────────────┘  │
└──────────────────────────────────────────┘
```

#### Node Manager Data Structure

```rust
pub struct NodeManager {
    pub id: i64,                           // Database primary key
    pub uuid: Uuid,                        // Unique identifier
    pub creator_id: i64,                   // User who registered

    // Capabilities
    pub tags: HashSet<String>,             // For suite matching
    pub labels: HashSet<String>,           // For querying

    // State
    pub state: NodeManagerState,           // Idle/Preparing/Executing/Cleanup
    pub last_heartbeat: OffsetDateTime,

    // Current assignment
    pub assigned_task_suite_id: Option<i64>,
    pub lease_expires_at: Option<OffsetDateTime>,

    // Metadata
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

pub enum NodeManagerState {
    Idle = 0,       // No suite assigned, waiting for work
    Preparing = 1,  // Running env_preparation hook
    Executing = 2,  // Workers running tasks
    Cleanup = 3,    // Running env_cleanup hook
    Offline = 4,    // Heartbeat timeout
}
```

### State Machine Details

```
Registration
     │
     ▼
┌─────────┐  fetch suite   ┌────────────┐  env_preparation  ┌───────────┐
│  IDLE   │───────────────>│ PREPARING  │──────────────────>│ EXECUTING │
└─────────┘                └────────────┘   (success)       └─────┬─────┘
     ▲                          │                                  │
     │                          │ (failure)                        │
     │                          ▼                                  │
     │                     Abort Suite                             │
     │                     Try next                                │
     │                          │                                  │
     └──────────────────────────┘                                  │
                                                                   │
                                                         all tasks │
                                                         finished  │
                                                                   ▼
     ┌──────────────────────────────────────────────────┐   ┌─────────┐
     │                                                  │   │ CLEANUP │
     │  Next suite fetch                               │   └────┬────┘
     └──────────────────────────────────────────────────┘        │
                                                                  │
                                                          (complete)
                                                                  │
                                                                  ▼
                                                           Back to IDLE
```

**State Behaviors:**

| State | Description | Activities |
|-------|-------------|------------|
| **Idle** | No suite assigned | - Send heartbeats<br>- Wait for suite assignment via WebSocket push<br>- Or poll for eligible suites |
| **Preparing** | Running env_preparation | - Execute setup hook<br>- Download resources from S3<br>- Set environment variables<br>- If fails: abort suite, back to Idle<br>- If succeeds: spawn workers |
| **Executing** | Workers running tasks | - Spawn N workers per schedule<br>- Proxy IPC ↔ WebSocket<br>- Monitor worker health<br>- Track failures<br>- Prefetch tasks<br>- Auto-respawn dead workers |
| **Cleanup** | Running env_cleanup | - Shutdown all workers gracefully<br>- Execute cleanup hook<br>- Upload final artifacts<br>- Release resources |
| **Offline** | Heartbeat timeout | - Coordinator marks as offline<br>- Reclaim assigned suite<br>- Manager auto-reconnects when online |

## Piece-by-Piece Implementation Roadmap

This section breaks down Phase 4 into small, independently testable pieces. Each piece builds on the previous ones but can be tested and verified independently before moving to the next.

### Piece 4.1: Create mitosis-manager Binary Skeleton

**Goal:** Get a basic binary running with CLI argument parsing.

**Tasks:**
- Set up `mitosis-manager` crate with clap for CLI parsing
- Define arguments: `--coordinator-url`, `--tags`, `--labels`, `--config`
- Implement basic main function structure
- Set up tracing-subscriber for structured logging
- Add `--help` and `--version` flags

**Deliverable:** Binary compiles and prints help message. Running with `--help` shows all available options.

**Test:** `cargo build && ./target/debug/mitosis-manager --help`

---

### Piece 4.2: Registration Flow (POST /managers)

**Goal:** Successfully register with coordinator and receive JWT token.

**Tasks:**
- Implement HTTP POST to `/managers` endpoint
- Send tags, labels, groups, and lifetime in request body
- Parse registration response (manager_uuid, token, websocket_url)
- Save JWT token to file with 0600 permissions (`/var/lib/mitosis/manager-{uuid}.token`)
- Handle registration errors with clear error messages

**Deliverable:** Manager can register with coordinator and receives JWT token. Token is saved to disk securely.

**Test:** Run binary with valid coordinator URL, verify token file created with correct permissions.

---

### Piece 4.3: WebSocket Client Connection

**Goal:** Establish WebSocket connection to coordinator using JWT token.

**Tasks:**
- Implement WebSocket connection using tokio-tungstenite
- Pass JWT token in connection URL query parameter or headers
- Handle connection success and log connection status
- Handle connection errors (invalid token, network errors)
- Implement basic message receive loop

**Deliverable:** Manager establishes WebSocket connection successfully. Connection errors are logged clearly.

**Test:** Verify WebSocket connection established, check coordinator logs for connection.

---

### Piece 4.4: Send Heartbeat Messages

**Goal:** Send periodic heartbeat messages to coordinator.

**Tasks:**
- Create `ManagerMessage::Heartbeat` structure with state and metrics
- Implement periodic heartbeat loop (30-second interval)
- Collect basic metrics (active workers, CPU, memory)
- Send heartbeat over WebSocket
- Log heartbeat success/failure

**Deliverable:** Manager sends heartbeats every 30 seconds with current state and metrics.

**Test:** Monitor coordinator logs, verify heartbeats arrive every 30 seconds.

---

### Piece 4.5: Reconnection Logic

**Goal:** Automatically reconnect to coordinator on network failures.

**Tasks:**
- Detect WebSocket disconnections
- Implement exponential backoff (1s → 2s → 4s → ... → 60s max)
- Auto-reconnect on connection loss
- Preserve manager state across reconnections
- Send heartbeat immediately after reconnection

**Deliverable:** Manager survives network failures and reconnects automatically. State is preserved.

**Test:** Kill coordinator, restart it, verify manager reconnects within 60 seconds.

---

### Piece 4.6: State Machine Skeleton

**Goal:** Implement state machine with valid transitions.

**Tasks:**
- Define `NodeManagerState` enum (Idle, Preparing, Executing, Cleanup, Offline)
- Implement state transition validation logic
- Track state transition history
- Add logging for all state transitions
- Implement state-specific handlers (placeholders for now)

**Deliverable:** State machine works correctly with valid transitions. Invalid transitions are rejected.

**Test:** Manually trigger state transitions, verify only valid transitions succeed.

---

### Piece 4.7: Handle SuiteAssigned Message

**Goal:** Receive and process suite assignments from coordinator.

**Tasks:**
- Implement `CoordinatorMessage::SuiteAssigned` message handling
- Parse `TaskSuiteSpec` structure
- Validate suite requirements (tag matching, resource capacity)
- Transition from Idle → Preparing state
- Reject assignments when not in Idle state
- Store suite assignment details

**Deliverable:** Manager can receive suite assignments and transition to Preparing state.

**Test:** Trigger suite assignment via coordinator, verify state transition.

---

### Piece 4.8: Graceful Shutdown Handler

**Goal:** Handle SIGTERM/SIGINT signals and shutdown cleanly.

**Tasks:**
- Set up signal handlers for SIGTERM and SIGINT
- Implement graceful shutdown sequence:
  - Stop accepting new work
  - Abort current suite (if any)
  - Send final heartbeat with Offline state
  - Close WebSocket connection
  - Clean up resources
- Add timeout for graceful shutdown (30 seconds)

**Deliverable:** Manager shuts down cleanly on signals. All resources cleaned up, no zombie processes.

**Test:** Send SIGTERM, verify manager shuts down within 30 seconds, resources cleaned.

---

### Piece 4.9: Token Refresh Mechanism

**Goal:** Automatically refresh JWT token before expiry.

**Tasks:**
- Decode JWT token to extract expiry time
- Check token expiry periodically (every 1 hour)
- Refresh token when < 24 hours remaining
- Save new token to disk
- Reconnect WebSocket with new token
- Handle refresh failures gracefully

**Deliverable:** Manager runs long-term without token expiry. Token is refreshed automatically.

**Test:** Use short-lived token (1 hour), verify token refreshed automatically.

---

### Testing Strategy

Each piece should be tested independently:

1. **Unit Tests:** Test individual functions and state transitions
2. **Integration Tests:** Test with real coordinator or mock WebSocket server
3. **Manual Tests:** Run binary and observe behavior
4. **Chaos Tests:** Simulate network failures, coordinator restarts, etc.

### Dependencies Between Pieces

```
4.1 (Binary Skeleton)
 │
 └─> 4.2 (Registration)
      │
      └─> 4.3 (WebSocket Connection)
           │
           ├─> 4.4 (Heartbeats)
           │
           ├─> 4.5 (Reconnection)
           │    │
           │    └─> 4.9 (Token Refresh)
           │
           └─> 4.6 (State Machine)
                │
                ├─> 4.7 (Suite Assignment)
                │
                └─> 4.8 (Graceful Shutdown)
```

Each piece builds on previous pieces, but is independently testable and verifiable before moving to the next.

## Implementation Tasks

### Task 4.1: Create mitosis-manager Binary

**File:** `mitosis-manager/src/main.rs`

#### CLI Arguments

```rust
use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "mitosis-manager")]
#[command(about = "Node Manager for Mitosis distributed task execution")]
struct Cli {
    /// Coordinator URL
    #[arg(long, env = "MITOSIS_COORDINATOR_URL")]
    coordinator_url: String,

    /// Manager tags (comma-separated)
    #[arg(long, env = "MITOSIS_MANAGER_TAGS", value_delimiter = ',')]
    tags: Vec<String>,

    /// Manager labels (comma-separated, format: key:value)
    #[arg(long, env = "MITOSIS_MANAGER_LABELS", value_delimiter = ',')]
    labels: Vec<String>,

    /// Groups this manager belongs to
    #[arg(long, env = "MITOSIS_MANAGER_GROUPS", value_delimiter = ',')]
    groups: Vec<String>,

    /// Configuration file path
    #[arg(short, long, env = "MITOSIS_MANAGER_CONFIG")]
    config: Option<PathBuf>,

    /// Token lifetime (default: 30d)
    #[arg(long, default_value = "30d")]
    token_lifetime: String,

    /// Log level
    #[arg(long, default_value = "info")]
    log_level: String,
}
```

#### Configuration File Structure

```rust
#[derive(Debug, Deserialize)]
pub struct ManagerConfig {
    pub coordinator_url: String,
    pub tags: Vec<String>,
    pub labels: Vec<String>,
    pub groups: Vec<String>,

    // Heartbeat settings
    pub heartbeat_interval: Option<Duration>,  // Default: 30s
    pub heartbeat_timeout: Option<Duration>,   // Default: 120s

    // WebSocket settings
    pub ws_reconnect_min_backoff: Option<Duration>,  // Default: 1s
    pub ws_reconnect_max_backoff: Option<Duration>,  // Default: 60s

    // Logging
    pub log_level: String,
    pub log_format: Option<String>,  // "json" or "text"
}
```

#### Main Function Structure

```rust
#[tokio::main]
async fn main() -> Result<()> {
    // 1. Parse CLI arguments
    let cli = Cli::parse();

    // 2. Load configuration (CLI args override config file)
    let config = load_config(&cli)?;

    // 3. Setup logging
    setup_logging(&config)?;

    // 4. Create manager instance
    let mut manager = NodeManager::new(config).await?;

    // 5. Register with coordinator
    manager.register().await?;

    // 6. Setup signal handlers
    let shutdown_signal = setup_shutdown_handler();

    // 7. Run manager loop
    tokio::select! {
        result = manager.run() => {
            result?;
        }
        _ = shutdown_signal => {
            tracing::info!("Shutdown signal received, cleaning up...");
            manager.shutdown().await?;
        }
    }

    Ok(())
}
```

#### Logging Setup

```rust
fn setup_logging(config: &ManagerConfig) -> Result<()> {
    use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

    let log_level = config.log_level.parse::<tracing::Level>()
        .unwrap_or(tracing::Level::INFO);

    let subscriber = tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG")
                .unwrap_or_else(|_| format!("mitosis_manager={}", log_level))
        ));

    match config.log_format.as_deref() {
        Some("json") => {
            subscriber
                .with(tracing_subscriber::fmt::layer().json())
                .init();
        }
        _ => {
            subscriber
                .with(tracing_subscriber::fmt::layer())
                .init();
        }
    }

    Ok(())
}
```

**Deliverables:**
- [ ] CLI argument parsing with clap
- [ ] Configuration file loading (TOML/JSON)
- [ ] CLI args override config file values
- [ ] Logging setup (tracing-subscriber)
- [ ] Signal handler (SIGTERM, SIGINT)

### Task 4.2: Implement Registration Flow

**File:** `mitosis-manager/src/registration.rs`

#### Registration HTTP Call

**Endpoint:** `POST /managers`

**Request:**
```json
{
  "tags": ["gpu", "linux", "x86_64", "cuda:11.8"],
  "labels": ["datacenter:us-west", "machine_id:server-42"],
  "groups": ["ml-team", "ci-team"],
  "lifetime": "30d"
}
```

**Response:**
```json
{
  "manager_uuid": "mgr-550e8400-...",
  "token": "eyJ0eXAiOiJKV1QiLCJhbGc...",
  "websocket_url": "wss://coordinator.example.com/ws/managers"
}
```

#### Implementation

```rust
use reqwest::Client;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize)]
pub struct ManagerRegistrationRequest {
    pub tags: Vec<String>,
    pub labels: Vec<String>,
    pub groups: Vec<String>,
    pub lifetime: String,
}

#[derive(Debug, Deserialize)]
pub struct ManagerRegistrationResponse {
    pub manager_uuid: Uuid,
    pub token: String,
    pub websocket_url: String,
}

impl NodeManager {
    pub async fn register(&mut self) -> Result<()> {
        let client = Client::new();

        let request = ManagerRegistrationRequest {
            tags: self.config.tags.clone(),
            labels: self.config.labels.clone(),
            groups: self.config.groups.clone(),
            lifetime: self.config.token_lifetime.clone(),
        };

        let url = format!("{}/managers", self.config.coordinator_url);

        let response = client
            .post(&url)
            .json(&request)
            .send()
            .await?
            .error_for_status()?;

        let registration: ManagerRegistrationResponse = response.json().await?;

        // Store credentials
        self.uuid = registration.manager_uuid;
        self.jwt_token = registration.token;
        self.websocket_url = registration.websocket_url;

        // Persist token to disk for recovery
        self.save_token_to_disk()?;

        tracing::info!(
            manager_uuid = %self.uuid,
            "Successfully registered with coordinator"
        );

        Ok(())
    }

    fn save_token_to_disk(&self) -> Result<()> {
        let token_path = format!("/var/lib/mitosis/manager-{}.token", self.uuid);
        std::fs::write(token_path, &self.jwt_token)?;
        Ok(())
    }
}
```

#### JWT Token Storage

Store token securely with appropriate file permissions:

```rust
use std::fs;
use std::os::unix::fs::PermissionsExt;

fn save_token_to_disk(&self) -> Result<()> {
    let token_dir = "/var/lib/mitosis";
    fs::create_dir_all(token_dir)?;

    let token_path = format!("{}/manager-{}.token", token_dir, self.uuid);
    fs::write(&token_path, &self.jwt_token)?;

    // Set permissions to 0600 (owner read/write only)
    let metadata = fs::metadata(&token_path)?;
    let mut permissions = metadata.permissions();
    permissions.set_mode(0o600);
    fs::set_permissions(&token_path, permissions)?;

    Ok(())
}
```

**Deliverables:**
- [ ] HTTP POST to `/managers` endpoint
- [ ] Parse registration response
- [ ] Store JWT token securely
- [ ] Store manager UUID
- [ ] Persist token to disk with proper permissions
- [ ] Handle registration errors (network, auth, etc.)

### Task 4.3: Implement WebSocket Client

**File:** `mitosis-manager/src/websocket.rs`

#### Connection Establishment

```rust
use tokio_tungstenite::{connect_async, tungstenite::Message};
use futures_util::{SinkExt, StreamExt};

impl NodeManager {
    async fn connect_websocket(&self) -> Result<WebSocketStream> {
        let url = format!("{}?token={}", self.websocket_url, self.jwt_token);

        let (ws_stream, response) = connect_async(&url).await?;

        tracing::info!(
            status = response.status().as_u16(),
            "WebSocket connection established"
        );

        Ok(ws_stream)
    }
}
```

#### Message Send/Receive

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
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

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
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
```

#### Request-Response Multiplexing

```rust
use tokio::sync::{Mutex, oneshot};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

pub struct WebSocketClient {
    ws: Arc<Mutex<WebSocketStream>>,
    pending_requests: Arc<Mutex<HashMap<u64, oneshot::Sender<CoordinatorMessage>>>>,
    next_request_id: Arc<AtomicU64>,
}

impl WebSocketClient {
    pub async fn send_request(&self, request: ManagerMessage) -> Result<CoordinatorMessage> {
        let request_id = self.next_request_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();

        // Register pending request
        self.pending_requests.lock().await.insert(request_id, tx);

        // Add request_id to message and send
        let msg = serde_json::to_string(&request)?;
        self.ws.lock().await.send(Message::Text(msg)).await?;

        // Wait for response with timeout
        let response = tokio::time::timeout(
            Duration::from_secs(30),
            rx
        ).await??;

        Ok(response)
    }

    pub async fn receive_loop(&self) -> Result<()> {
        loop {
            let msg = {
                let mut ws = self.ws.lock().await;
                ws.next().await
            };

            match msg {
                Some(Ok(Message::Text(text))) => {
                    let msg: CoordinatorMessage = serde_json::from_str(&text)?;
                    self.handle_message(msg).await?;
                }
                Some(Ok(Message::Close(_))) => {
                    tracing::info!("WebSocket closed by coordinator");
                    break;
                }
                Some(Err(e)) => {
                    tracing::error!("WebSocket error: {}", e);
                    return Err(e.into());
                }
                None => {
                    tracing::info!("WebSocket stream ended");
                    break;
                }
                _ => {}
            }
        }

        Ok(())
    }

    async fn handle_message(&self, msg: CoordinatorMessage) -> Result<()> {
        match msg {
            CoordinatorMessage::TaskAvailable { request_id, .. } |
            CoordinatorMessage::TaskReportAck { request_id, .. } => {
                // Complete pending request
                if let Some(tx) = self.pending_requests.lock().await.remove(&request_id) {
                    let _ = tx.send(msg);
                }
            }
            CoordinatorMessage::SuiteAssigned { .. } |
            CoordinatorMessage::CancelTask { .. } |
            CoordinatorMessage::CancelSuite { .. } |
            CoordinatorMessage::ConfigUpdate { .. } |
            CoordinatorMessage::Shutdown { .. } => {
                // Push messages - handle asynchronously
                self.handle_push_message(msg).await?;
            }
        }

        Ok(())
    }
}
```

#### Reconnection Logic with Exponential Backoff

```rust
impl NodeManager {
    pub async fn maintain_websocket_connection(&mut self) -> Result<()> {
        let mut backoff = Duration::from_secs(1);
        let max_backoff = Duration::from_secs(60);

        loop {
            match self.connect_websocket().await {
                Ok(ws) => {
                    self.ws_client = Some(WebSocketClient::new(ws));
                    backoff = Duration::from_secs(1); // Reset backoff

                    tracing::info!("WebSocket connected, sending heartbeat");

                    // Send heartbeat to resume state
                    self.send_heartbeat().await?;

                    // If executing suite, notify coordinator
                    if let Some(suite_uuid) = self.assigned_suite_uuid {
                        tracing::info!(
                            suite_uuid = %suite_uuid,
                            "Resuming suite execution after reconnection"
                        );
                    }

                    // Run message loop until disconnected
                    if let Err(e) = self.run_message_loop().await {
                        tracing::error!("Message loop error: {}", e);
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "WebSocket connection failed: {}, retrying in {:?}",
                        e,
                        backoff
                    );
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(max_backoff); // Exponential backoff
                }
            }
        }
    }
}
```

#### Token Refresh

```rust
impl NodeManager {
    async fn check_and_refresh_token(&mut self) -> Result<()> {
        let token_expiry = self.decode_token_expiry(&self.jwt_token)?;
        let time_until_expiry = token_expiry - OffsetDateTime::now_utc();

        // Refresh if expiring within 24 hours
        if time_until_expiry < Duration::from_secs(86400) {
            tracing::info!("JWT token expiring soon, refreshing");

            let new_token = self.refresh_token().await?;
            self.jwt_token = new_token;
            self.save_token_to_disk()?;

            // Reconnect WebSocket with new token
            self.reconnect_websocket().await?;
        }

        Ok(())
    }

    async fn refresh_token(&self) -> Result<String> {
        let client = Client::new();
        let url = format!(
            "{}/managers/{}/refresh-token",
            self.config.coordinator_url,
            self.uuid
        );

        let response = client
            .post(&url)
            .bearer_auth(&self.jwt_token)
            .send()
            .await?
            .error_for_status()?;

        let data: serde_json::Value = response.json().await?;
        let new_token = data["token"].as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing token in response"))?
            .to_string();

        Ok(new_token)
    }
}
```

**Deliverables:**
- [ ] WebSocket connection establishment with JWT auth
- [ ] Message serialization/deserialization
- [ ] Request-response multiplexing
- [ ] Message receive loop
- [ ] Reconnection logic with exponential backoff (1s to 60s)
- [ ] Token refresh mechanism
- [ ] Handle connection errors gracefully

### Task 4.4: Implement State Machine

**File:** `mitosis-manager/src/state_machine.rs`

#### State Enum and Transitions

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(i32)]
pub enum NodeManagerState {
    Idle = 0,       // No suite assigned, waiting for work
    Preparing = 1,  // Running env_preparation hook
    Executing = 2,  // Workers running tasks
    Cleanup = 3,    // Running env_cleanup hook
    Offline = 4,    // Heartbeat timeout (set by coordinator)
}

impl NodeManagerState {
    pub fn can_transition_to(&self, next: NodeManagerState) -> bool {
        use NodeManagerState::*;

        matches!(
            (self, next),
            // From Idle
            (Idle, Preparing) |
            (Idle, Offline) |

            // From Preparing
            (Preparing, Executing) |  // Success
            (Preparing, Idle) |       // Failure or abort
            (Preparing, Offline) |

            // From Executing
            (Executing, Cleanup) |    // Suite completed
            (Executing, Idle) |       // Suite aborted
            (Executing, Offline) |

            // From Cleanup
            (Cleanup, Idle) |         // Cleanup complete
            (Cleanup, Offline) |

            // From Offline
            (Offline, Idle) |         // Reconnected
        )
    }
}
```

#### State Machine Implementation

```rust
pub struct StateMachine {
    current_state: NodeManagerState,
    state_entered_at: OffsetDateTime,
    transition_history: Vec<StateTransition>,
}

#[derive(Debug, Clone)]
struct StateTransition {
    from: NodeManagerState,
    to: NodeManagerState,
    timestamp: OffsetDateTime,
    reason: String,
}

impl StateMachine {
    pub fn new() -> Self {
        Self {
            current_state: NodeManagerState::Idle,
            state_entered_at: OffsetDateTime::now_utc(),
            transition_history: Vec::new(),
        }
    }

    pub fn transition(&mut self, next_state: NodeManagerState, reason: String) -> Result<()> {
        if !self.current_state.can_transition_to(next_state) {
            return Err(anyhow::anyhow!(
                "Invalid state transition: {:?} -> {:?}",
                self.current_state,
                next_state
            ));
        }

        tracing::info!(
            from = ?self.current_state,
            to = ?next_state,
            reason = %reason,
            "State transition"
        );

        // Record transition
        self.transition_history.push(StateTransition {
            from: self.current_state,
            to: next_state,
            timestamp: OffsetDateTime::now_utc(),
            reason: reason.clone(),
        });

        // Update state
        self.current_state = next_state;
        self.state_entered_at = OffsetDateTime::now_utc();

        Ok(())
    }

    pub fn current_state(&self) -> NodeManagerState {
        self.current_state
    }

    pub fn time_in_current_state(&self) -> Duration {
        OffsetDateTime::now_utc() - self.state_entered_at
    }
}
```

#### State-Specific Logic

```rust
impl NodeManager {
    pub async fn handle_state(&mut self) -> Result<()> {
        match self.state_machine.current_state() {
            NodeManagerState::Idle => self.handle_idle_state().await,
            NodeManagerState::Preparing => self.handle_preparing_state().await,
            NodeManagerState::Executing => self.handle_executing_state().await,
            NodeManagerState::Cleanup => self.handle_cleanup_state().await,
            NodeManagerState::Offline => self.handle_offline_state().await,
        }
    }

    async fn handle_idle_state(&mut self) -> Result<()> {
        // Send heartbeats
        self.send_heartbeat().await?;

        // Wait for suite assignment via WebSocket push
        // (handled in message loop)

        tokio::time::sleep(Duration::from_secs(30)).await;
        Ok(())
    }

    async fn handle_preparing_state(&mut self) -> Result<()> {
        // Environment preparation logic (Phase 5)
        // For Phase 4, just transition immediately
        tracing::info!("Environment preparation (placeholder for Phase 5)");

        self.state_machine.transition(
            NodeManagerState::Executing,
            "Preparation complete".to_string()
        )?;

        Ok(())
    }

    async fn handle_executing_state(&mut self) -> Result<()> {
        // Worker spawning and monitoring (Phase 6)
        // For Phase 4, just send heartbeats
        self.send_heartbeat().await?;

        tokio::time::sleep(Duration::from_secs(10)).await;
        Ok(())
    }

    async fn handle_cleanup_state(&mut self) -> Result<()> {
        // Environment cleanup logic (Phase 5)
        tracing::info!("Environment cleanup (placeholder for Phase 5)");

        self.state_machine.transition(
            NodeManagerState::Idle,
            "Cleanup complete".to_string()
        )?;

        // Clear assignment
        self.assigned_suite_uuid = None;

        Ok(())
    }

    async fn handle_offline_state(&mut self) -> Result<()> {
        // Reconnect and transition back to Idle
        tracing::warn!("Manager is offline, attempting to reconnect");

        self.maintain_websocket_connection().await?;

        self.state_machine.transition(
            NodeManagerState::Idle,
            "Reconnected".to_string()
        )?;

        Ok(())
    }
}
```

**Deliverables:**
- [ ] `NodeManagerState` enum with all states
- [ ] State transition validation
- [ ] State transition history tracking
- [ ] State-specific handlers (placeholders for Phase 5/6)
- [ ] Logging for all state transitions
- [ ] Time tracking per state

### Task 4.5: Implement Suite Assignment Logic

**File:** `mitosis-manager/src/suite_assignment.rs`

#### Receiving SuiteAssigned Message

```rust
impl NodeManager {
    async fn handle_suite_assigned(
        &mut self,
        suite_uuid: Uuid,
        suite_spec: TaskSuiteSpec,
    ) -> Result<()> {
        tracing::info!(
            suite_uuid = %suite_uuid,
            suite_name = %suite_spec.name,
            "Received suite assignment"
        );

        // Verify we're in Idle state
        if self.state_machine.current_state() != NodeManagerState::Idle {
            tracing::warn!(
                current_state = ?self.state_machine.current_state(),
                "Received suite assignment but not in Idle state, rejecting"
            );
            return Ok(());
        }

        // Store assignment
        self.assigned_suite_uuid = Some(suite_uuid);
        self.current_suite_spec = Some(suite_spec);

        // Transition to Preparing
        self.state_machine.transition(
            NodeManagerState::Preparing,
            format!("Suite assigned: {}", suite_uuid)
        )?;

        Ok(())
    }
}
```

#### Parsing Suite Spec

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSuiteSpec {
    pub uuid: Uuid,
    pub name: String,
    pub group_id: i64,
    pub tags: Vec<String>,

    // Environment lifecycle
    pub env_preparation: Option<EnvHookSpec>,
    pub env_cleanup: Option<EnvHookSpec>,

    // Worker scheduling
    pub worker_schedule: WorkerSchedulePlan,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvHookSpec {
    pub command: String,
    pub args: Vec<String>,
    pub envs: HashMap<String, String>,
    pub timeout: Duration,
    pub resources: Vec<S3ResourceSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerSchedulePlan {
    pub worker_count: u32,
    pub cpu_binding: Option<CpuBindingSpec>,
    pub task_prefetch_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct S3ResourceSpec {
    pub bucket: String,
    pub key: String,
    pub destination: PathBuf,
}
```

#### Suite Assignment Validation

```rust
impl NodeManager {
    fn validate_suite_assignment(&self, suite_spec: &TaskSuiteSpec) -> Result<()> {
        // 1. Check tag matching
        let suite_tags: HashSet<_> = suite_spec.tags.iter().collect();
        let manager_tags: HashSet<_> = self.config.tags.iter().collect();

        if !suite_tags.is_subset(&manager_tags) {
            return Err(anyhow::anyhow!(
                "Suite requires tags {:?} but manager only has {:?}",
                suite_tags,
                manager_tags
            ));
        }

        // 2. Check resource capacity
        if suite_spec.worker_schedule.worker_count > 256 {
            return Err(anyhow::anyhow!(
                "Worker count {} exceeds maximum (256)",
                suite_spec.worker_schedule.worker_count
            ));
        }

        // 3. Validate environment hooks
        if let Some(prep) = &suite_spec.env_preparation {
            if prep.timeout > Duration::from_secs(3600) {
                return Err(anyhow::anyhow!(
                    "Environment preparation timeout too long: {:?}",
                    prep.timeout
                ));
            }
        }

        Ok(())
    }
}
```

**Deliverables:**
- [ ] Handle `SuiteAssigned` WebSocket message
- [ ] Parse `TaskSuiteSpec` structure
- [ ] Validate suite requirements against manager capabilities
- [ ] Store suite assignment
- [ ] Transition to Preparing state
- [ ] Reject assignments when not in Idle state

### Task 4.6: Implement Heartbeat System

**File:** `mitosis-manager/src/heartbeat.rs`

#### Heartbeat Message Structure

```rust
#[derive(Debug, Serialize, Deserialize)]
pub struct ManagerMetrics {
    pub active_workers: u32,
    pub tasks_completed: u64,
    pub tasks_failed: u64,
    pub cpu_usage: f64,
    pub memory_usage: u64,
}

impl NodeManager {
    async fn send_heartbeat(&mut self) -> Result<()> {
        let metrics = self.collect_metrics()?;

        let heartbeat = ManagerMessage::Heartbeat {
            manager_uuid: self.uuid,
            state: self.state_machine.current_state(),
            metrics,
        };

        if let Some(ws_client) = &self.ws_client {
            ws_client.send_message(heartbeat).await?;

            self.last_heartbeat_sent = OffsetDateTime::now_utc();

            tracing::debug!(
                state = ?self.state_machine.current_state(),
                "Heartbeat sent"
            );
        }

        Ok(())
    }

    fn collect_metrics(&self) -> Result<ManagerMetrics> {
        Ok(ManagerMetrics {
            active_workers: self.active_workers.len() as u32,
            tasks_completed: self.stats.tasks_completed,
            tasks_failed: self.stats.tasks_failed,
            cpu_usage: self.get_cpu_usage()?,
            memory_usage: self.get_memory_usage()?,
        })
    }
}
```

#### Periodic Heartbeat Loop

```rust
impl NodeManager {
    async fn heartbeat_loop(&mut self) -> Result<()> {
        let mut interval = tokio::time::interval(
            self.config.heartbeat_interval.unwrap_or(Duration::from_secs(30))
        );

        loop {
            interval.tick().await;

            if let Err(e) = self.send_heartbeat().await {
                tracing::error!("Failed to send heartbeat: {}", e);

                // If heartbeat fails repeatedly, reconnect
                let time_since_last = OffsetDateTime::now_utc() - self.last_heartbeat_sent;
                if time_since_last > Duration::from_secs(120) {
                    tracing::warn!("Heartbeat timeout, reconnecting WebSocket");
                    self.maintain_websocket_connection().await?;
                }
            }
        }
    }
}
```

#### Failure Handling

```rust
impl NodeManager {
    async fn handle_heartbeat_failure(&mut self) -> Result<()> {
        self.heartbeat_failure_count += 1;

        if self.heartbeat_failure_count >= 3 {
            tracing::error!(
                failures = self.heartbeat_failure_count,
                "Multiple heartbeat failures, attempting reconnection"
            );

            // Reset failure count
            self.heartbeat_failure_count = 0;

            // Trigger reconnection
            self.maintain_websocket_connection().await?;
        }

        Ok(())
    }
}
```

**Deliverables:**
- [ ] `ManagerMetrics` data structure
- [ ] Heartbeat message construction
- [ ] Periodic heartbeat loop (default: 30s interval)
- [ ] Metrics collection (workers, tasks, CPU, memory)
- [ ] Heartbeat failure detection
- [ ] Reconnection on repeated failures
- [ ] Logging for heartbeat events

### Task 4.7: Implement Graceful Shutdown

**File:** `mitosis-manager/src/shutdown.rs`

#### Shutdown Signal Handler

```rust
use tokio::signal;

pub async fn setup_shutdown_handler() -> tokio::sync::oneshot::Receiver<()> {
    let (tx, rx) = tokio::sync::oneshot::channel();

    tokio::spawn(async move {
        let mut sigterm = signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("Failed to setup SIGTERM handler");
        let mut sigint = signal::unix::signal(signal::unix::SignalKind::interrupt())
            .expect("Failed to setup SIGINT handler");

        tokio::select! {
            _ = sigterm.recv() => {
                tracing::info!("Received SIGTERM");
            }
            _ = sigint.recv() => {
                tracing::info!("Received SIGINT");
            }
        }

        let _ = tx.send(());
    });

    rx
}
```

#### Graceful Shutdown Logic

```rust
impl NodeManager {
    pub async fn shutdown(&mut self) -> Result<()> {
        tracing::info!("Starting graceful shutdown");

        // 1. Stop accepting new suites
        self.accepting_work = false;

        // 2. If executing a suite, complete it or abort
        if let Some(suite_uuid) = self.assigned_suite_uuid {
            tracing::info!(
                suite_uuid = %suite_uuid,
                "Aborting current suite due to shutdown"
            );

            self.abort_current_suite().await?;
        }

        // 3. Shutdown all workers (Phase 6)
        // For Phase 4, placeholder
        tracing::info!("Shutting down workers (placeholder)");

        // 4. Send final heartbeat with Offline state
        self.state_machine.transition(
            NodeManagerState::Offline,
            "Shutting down".to_string()
        )?;
        self.send_heartbeat().await?;

        // 5. Close WebSocket connection
        if let Some(ws_client) = &mut self.ws_client {
            ws_client.close().await?;
        }

        // 6. Clean up resources
        self.cleanup_resources().await?;

        tracing::info!("Graceful shutdown complete");
        Ok(())
    }

    async fn abort_current_suite(&mut self) -> Result<()> {
        if let Some(suite_uuid) = self.assigned_suite_uuid {
            // Notify coordinator
            if let Some(ws_client) = &self.ws_client {
                let msg = ManagerMessage::SuiteCompleted {
                    suite_uuid,
                    tasks_completed: self.stats.tasks_completed,
                    tasks_failed: self.stats.tasks_failed,
                };

                ws_client.send_message(msg).await?;
            }

            // Transition to cleanup
            self.state_machine.transition(
                NodeManagerState::Cleanup,
                "Aborted due to shutdown".to_string()
            )?;

            // Run cleanup (Phase 5)
            // For Phase 4, just clear assignment
            self.assigned_suite_uuid = None;
            self.current_suite_spec = None;
        }

        Ok(())
    }

    async fn cleanup_resources(&mut self) -> Result<()> {
        // Clean up temporary files
        if let Some(work_dir) = &self.work_dir {
            if work_dir.exists() {
                std::fs::remove_dir_all(work_dir)?;
            }
        }

        // Remove IPC services (Phase 6)

        Ok(())
    }
}
```

#### Force Shutdown

```rust
impl NodeManager {
    pub async fn force_shutdown(&mut self) -> Result<()> {
        tracing::warn!("Force shutdown initiated");

        // Skip graceful steps, immediately:
        // 1. Kill all workers
        // 2. Close WebSocket
        // 3. Exit

        if let Some(ws_client) = &mut self.ws_client {
            let _ = ws_client.close().await;
        }

        Ok(())
    }
}
```

**Deliverables:**
- [ ] Signal handler for SIGTERM and SIGINT
- [ ] Graceful shutdown sequence
- [ ] Abort current suite if executing
- [ ] Send final heartbeat with Offline state
- [ ] Close WebSocket connection
- [ ] Cleanup temporary resources
- [ ] Force shutdown option
- [ ] Timeout for graceful shutdown (fallback to force)

## Testing Checklist

### Manager Registration
- [ ] Successful registration with valid tags/labels
- [ ] Registration with invalid coordinator URL
- [ ] JWT token stored correctly
- [ ] Token persisted to disk with proper permissions
- [ ] Re-registration after token expiry

### WebSocket Connection
- [ ] Establish connection with valid JWT
- [ ] Connection rejected with invalid JWT
- [ ] Connection stability over 1 hour
- [ ] Multiple concurrent requests multiplexed correctly
- [ ] Out-of-order responses handled correctly

### Token Refresh
- [ ] Token refreshed before expiry
- [ ] New token stored and used
- [ ] WebSocket reconnects with new token
- [ ] Failed refresh handled gracefully

### Reconnection Logic
- [ ] Reconnect after coordinator restart
- [ ] Exponential backoff (1s → 2s → 4s → ... → 60s)
- [ ] State preserved across reconnection
- [ ] Resume suite execution after reconnection
- [ ] Handle reconnection during different states

### State Machine
- [ ] Valid state transitions work
- [ ] Invalid transitions rejected
- [ ] State history tracked
- [ ] Time in state calculated correctly

### Suite Assignment
- [ ] Receive and parse `SuiteAssigned` message
- [ ] Transition to Preparing state
- [ ] Reject assignment when not Idle
- [ ] Validate suite requirements
- [ ] Tag matching works correctly

### Heartbeat System
- [ ] Heartbeats sent every 30 seconds
- [ ] Correct state and metrics included
- [ ] Heartbeat failures trigger reconnection
- [ ] Metrics accurately reflect manager state

### Graceful Shutdown
- [ ] SIGTERM triggers graceful shutdown
- [ ] SIGINT triggers graceful shutdown
- [ ] Current suite aborted if executing
- [ ] Final heartbeat sent
- [ ] WebSocket closed cleanly
- [ ] Resources cleaned up

## Success Criteria

1. **Manager Lifecycle**
   - Manager successfully registers with coordinator
   - JWT token obtained and stored securely
   - WebSocket connection established
   - Heartbeats sent every 30 seconds

2. **Connection Resilience**
   - Manager reconnects after network interruption
   - Exponential backoff implemented correctly (1s to 60s)
   - Token refresh works before expiry
   - State preserved across reconnections

3. **State Management**
   - All state transitions work correctly
   - Invalid transitions prevented
   - State history tracked accurately
   - Each state handler implemented (placeholders for Phase 5/6)

4. **Suite Assignment**
   - Receives `SuiteAssigned` message
   - Parses suite specification correctly
   - Validates suite requirements
   - Transitions to Preparing state

5. **Error Handling**
   - Network errors handled gracefully
   - Authentication failures logged
   - Heartbeat timeouts trigger reconnection
   - All errors logged with appropriate levels

6. **Shutdown**
   - Graceful shutdown completes within 30 seconds
   - Current suite aborted cleanly
   - All resources cleaned up
   - No zombie processes left behind

## Dependencies

**Required from Previous Phases:**
- Phase 1: Database schema with `node_managers` table
- Phase 2: `POST /managers` registration endpoint
- Phase 2: `POST /managers/heartbeat` endpoint
- Phase 2: `POST /managers/{uuid}/refresh-token` endpoint
- Phase 3: WebSocket server at `/ws/managers`
- Phase 3: `SuiteAssigned` message implementation

**External Dependencies:**
- tokio-tungstenite (WebSocket client)
- reqwest (HTTP client)
- clap (CLI parsing)
- tracing/tracing-subscriber (logging)
- serde/serde_json (serialization)

## Next Phase

**Phase 5: Environment Hooks (1 week)**

Phase 5 will implement:
- Environment preparation hooks
- S3 resource downloads
- Environment variable setup
- Preparation timeout handling
- Environment cleanup hooks
- Cleanup error handling (log but don't fail)

The state machine handlers implemented in Phase 4 (`handle_preparing_state` and `handle_cleanup_state`) will be expanded in Phase 5 with actual environment lifecycle logic.

## Additional Notes

### Code Organization

```
mitosis-manager/
├── src/
│   ├── main.rs              # Entry point, CLI parsing
│   ├── config.rs            # Configuration loading
│   ├── manager.rs           # NodeManager main struct
│   ├── registration.rs      # Registration logic
│   ├── websocket.rs         # WebSocket client
│   ├── state_machine.rs     # State machine implementation
│   ├── suite_assignment.rs  # Suite assignment handling
│   ├── heartbeat.rs         # Heartbeat system
│   ├── shutdown.rs          # Graceful shutdown
│   ├── error.rs             # Error types
│   └── metrics.rs           # Metrics collection
├── Cargo.toml
└── README.md
```

### Logging Guidelines

Use structured logging with appropriate levels:

```rust
// Info: Normal operations
tracing::info!(
    manager_uuid = %self.uuid,
    "Successfully registered with coordinator"
);

// Debug: Detailed state information
tracing::debug!(
    state = ?self.state_machine.current_state(),
    "Heartbeat sent"
);

// Warn: Recoverable errors
tracing::warn!(
    "WebSocket connection failed: {}, retrying in {:?}",
    e,
    backoff
);

// Error: Serious errors requiring attention
tracing::error!(
    "Failed to send heartbeat: {}",
    e
);
```

### Performance Targets

| Operation | Target | Max |
|-----------|--------|-----|
| Registration | 100ms | 500ms |
| WebSocket connection | 50ms | 200ms |
| Heartbeat send | 10ms | 50ms |
| State transition | 1ms | 10ms |
| Token refresh | 100ms | 500ms |

### Security Considerations

1. **Token Storage**: JWT tokens stored with 0600 permissions
2. **Token Transmission**: Only over TLS (wss://)
3. **Token Refresh**: Automatic refresh before expiry
4. **WebSocket Auth**: JWT included in connection handshake
5. **Error Messages**: Don't leak sensitive information in logs
