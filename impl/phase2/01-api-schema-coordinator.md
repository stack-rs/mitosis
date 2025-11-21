# Phase 2: API Schema and Coordinator Endpoints Implementation Guide

## Overview

Phase 2 focuses on implementing the API layer and coordinator endpoints for Task Suites and Node Managers. This phase builds upon the database schema established in Phase 1 and provides the HTTP/REST interface that users and managers will interact with.

**Implementation Approach:**
This phase is broken down into 13 independently runnable and testable pieces. Each piece can be developed, tested with curl, and verified in isolation before moving to the next piece. This incremental approach ensures steady progress and early detection of issues.

**Key Deliverables:**
- Rust type definitions for all API request/response schemas
- Suite management endpoints (create, query, get details, cancel, manager assignment)
- Node manager endpoints (register, heartbeat, query, shutdown)
- Updated task submission endpoint with suite support
- Permission checking logic
- Tag matching algorithm

**Timeline:** 1 week (13 pieces)

## Prerequisites

### Completed
- ✅ Phase 1 completed (database schema and migrations in place)
- ✅ All database tables created: `task_suites`, `node_managers`, `group_node_manager`, `task_suite_managers`, `task_execution_failures`
- ✅ Database triggers implemented for auto-updating suite task counts
- ✅ SeaORM entity models generated

### Technical Knowledge Required
- **Axum Web Framework**: Understanding of handlers, extractors, routing
- **JWT Authentication**: Familiarity with EdDSA token validation
- **PostgreSQL/SeaORM**: Query building, joins, transactions
- **Rust Type System**: Serde serialization, derive macros, enums
- **Group-based Permissions**: Understanding existing `group_worker` permission model

### Development Environment
- Rust 1.70+ with cargo
- PostgreSQL 14+ running locally or accessible
- Existing mitosis coordinator codebase checked out
- Test database populated with sample users, groups

## Design References

### RFC Section 7: API Design

The complete API specifications are defined in RFC Section 7 (lines 816-1137). Key subsections:
- **7.1 Suite Management APIs** (lines 818-941): Create, query, details, cancel endpoints
- **7.2 Suite Manager Assignment APIs** (lines 943-1016): Refresh, add, remove managers
- **7.3 Task Submission API Update** (lines 1018-1051): Add suite_uuid field
- **7.4 Node Manager APIs** (lines 1053-1136): Register, heartbeat, query, shutdown

### RFC Section 4: Core Concepts

Data model definitions (lines 213-434):
- **4.1 Task Suite** (lines 215-315): Properties, lifecycle states, context variables
- **4.2 Node Manager** (lines 326-392): Responsibilities, properties, states
- **4.3 Worker Modes** (lines 394-434): Independent vs managed workers

### RFC Section 6: Data Models

Database schema reference (lines 550-815):
- **6.1 New Database Tables** (lines 552-726): Complete SQL schemas
- **6.2 Update to active_tasks** (lines 728-741): Suite foreign key
- **6.3 Database Triggers** (lines 743-815): Auto-update logic

### RFC Section 11: Security and Permissions

Permission model (lines 2310-2473):
- **11.1 Permission Model** (lines 2312-2378): Tag matching and role checks
- **11.2 JWT Token Management** (lines 2381-2442): Token lifetime, refresh logic
- **11.3 Authentication Flow** (lines 2444-2473): Manager registration and auth

## Piece-by-Piece Implementation

Each piece below is independently runnable and testable. Complete them in order, testing each one with curl or integration tests before proceeding to the next.

---

## Piece 2.1: Define All Request/Response Types in schema.rs

**Goal:** Define all Rust type definitions for API request/response schemas. These types will be used by all subsequent pieces.

**Files to Modify:**
- `src/schema.rs` or `src/api/types.rs`

**Implementation Steps:**

Create comprehensive type definitions for all request/response types.

### Suite Types

```rust
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use time::OffsetDateTime;
use std::collections::HashSet;

/// Request to create a new task suite
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateTaskSuiteReq {
    /// Optional human-readable name (non-unique)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// Optional description
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Group that owns this suite (required for permissions)
    pub group_name: String,

    /// Tags for manager matching (e.g., ["gpu", "linux", "cuda:11.8"])
    #[serde(default)]
    pub tags: Vec<String>,

    /// Labels for querying/filtering (e.g., ["project:resnet", "phase:training"])
    #[serde(default)]
    pub labels: Vec<String>,

    /// Suite scheduling priority (higher = more important)
    #[serde(default)]
    pub priority: i32,

    /// Worker allocation plan
    pub worker_schedule: WorkerSchedulePlan,

    /// Optional environment preparation hook
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_preparation: Option<EnvHookSpec>,

    /// Optional environment cleanup hook
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_cleanup: Option<EnvHookSpec>,
}

/// Response after creating a task suite
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateTaskSuiteResp {
    /// Unique UUID for this suite
    pub uuid: Uuid,

    /// Initial state (always "Open" on creation)
    pub state: TaskSuiteState,

    /// Initially empty list of assigned managers
    pub assigned_managers: Vec<Uuid>,
}

/// Worker scheduling configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerSchedulePlan {
    /// Number of workers to spawn (1-256)
    pub worker_count: u32,

    /// Optional CPU core binding strategy
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_binding: Option<CpuBinding>,

    /// How many tasks to prefetch locally (default: 16)
    #[serde(default = "default_prefetch_count")]
    pub task_prefetch_count: u32,
}

fn default_prefetch_count() -> u32 {
    16
}

/// CPU core binding configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CpuBinding {
    /// List of CPU core IDs to bind to
    pub cores: Vec<usize>,

    /// Binding strategy
    pub strategy: CpuBindingStrategy,
}

/// CPU binding strategies
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum CpuBindingStrategy {
    /// Distribute workers across cores in round-robin fashion
    RoundRobin,

    /// Each worker gets exclusive access to dedicated core(s)
    Exclusive,

    /// All workers share all specified cores
    Shared,
}

/// Environment hook specification (preparation or cleanup)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvHookSpec {
    /// Command and arguments to execute
    pub args: Vec<String>,

    /// Environment variables to set
    #[serde(default)]
    pub envs: HashMap<String, String>,

    /// Remote resources to download before execution
    #[serde(default)]
    pub resources: Vec<RemoteResourceDownload>,

    /// Execution timeout (e.g., "5m", "1h")
    #[serde(with = "humantime_serde")]
    pub timeout: Duration,
}

/// Remote resource download specification
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteResourceDownload {
    /// Remote file reference (S3 attachment or artifact)
    pub remote_file: RemoteFileRef,

    /// Local path to save the file
    pub local_path: String,
}

/// Reference to a remote file
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum RemoteFileRef {
    /// Reference to an attachment by key
    Attachment { key: String },

    /// Reference to an artifact by UUID
    Artifact { uuid: Uuid },
}

/// Task suite lifecycle states
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskSuiteState {
    /// Accepting new tasks, actively executing
    Open = 0,

    /// No new tasks for 3 minutes, but not complete
    Closed = 1,

    /// All tasks finished (can reopen if new task submitted)
    Complete = 2,

    /// Explicitly cancelled by user (terminal state)
    Cancelled = 3,
}

impl From<i32> for TaskSuiteState {
    fn from(value: i32) -> Self {
        match value {
            0 => TaskSuiteState::Open,
            1 => TaskSuiteState::Closed,
            2 => TaskSuiteState::Complete,
            3 => TaskSuiteState::Cancelled,
            _ => TaskSuiteState::Open, // Default fallback
        }
    }
}

impl From<TaskSuiteState> for i32 {
    fn from(state: TaskSuiteState) -> Self {
        state as i32
    }
}

/// Query parameters for listing suites
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSuiteQueryReq {
    /// Filter by group name
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_name: Option<String>,

    /// Filter by labels (suite must have ALL specified labels)
    #[serde(default)]
    pub labels: Vec<String>,

    /// Filter by state
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<TaskSuiteState>,

    /// Filter by tags (suite must have ALL specified tags)
    #[serde(default)]
    pub tags: Vec<String>,

    /// Pagination: offset
    #[serde(default)]
    pub offset: i64,

    /// Pagination: limit (max 100)
    #[serde(default = "default_limit")]
    pub limit: i64,
}

fn default_limit() -> i64 {
    50
}

/// Response for suite query
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSuiteQueryResp {
    /// Total count of matching suites
    pub count: i64,

    /// List of suite summaries
    pub suites: Vec<TaskSuiteSummary>,
}

/// Summary information for a task suite
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSuiteSummary {
    pub uuid: Uuid,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    pub group_name: String,
    pub tags: Vec<String>,
    pub labels: Vec<String>,
    pub state: TaskSuiteState,
    pub priority: i32,

    /// Total tasks ever submitted to this suite
    pub total_tasks: i32,

    /// Currently pending/active tasks
    pub pending_tasks: i32,

    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,

    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

/// Detailed information for a single suite
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSuiteDetails {
    pub uuid: Uuid,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    pub group_name: String,
    pub creator_username: String,

    pub tags: Vec<String>,
    pub labels: Vec<String>,
    pub priority: i32,

    pub worker_schedule: WorkerSchedulePlan,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub env_preparation: Option<EnvHookSpec>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub env_cleanup: Option<EnvHookSpec>,

    pub state: TaskSuiteState,

    #[serde(skip_serializing_if = "Option::is_none", with = "time::serde::rfc3339::option")]
    pub last_task_submitted_at: Option<OffsetDateTime>,

    pub total_tasks: i32,
    pub pending_tasks: i32,

    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,

    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,

    #[serde(skip_serializing_if = "Option::is_none", with = "time::serde::rfc3339::option")]
    pub completed_at: Option<OffsetDateTime>,

    /// List of assigned manager UUIDs
    pub assigned_managers: Vec<Uuid>,
}

/// Request to cancel a suite
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelSuiteReq {
    /// Optional cancellation reason
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,

    /// Whether to cancel currently running tasks
    #[serde(default)]
    pub cancel_running_tasks: bool,
}

/// Response after cancelling a suite
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelSuiteResp {
    /// Number of tasks that were cancelled
    pub cancelled_task_count: i32,

    /// New suite state (should be Cancelled)
    pub suite_state: TaskSuiteState,
}
```

#### Suite Manager Assignment Types

```rust
/// Response for refreshing tag-matched managers
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefreshManagersResp {
    /// Managers that were added
    pub added_managers: Vec<ManagerAssignment>,

    /// Managers that were removed
    pub removed_managers: Vec<Uuid>,

    /// Total count of assigned managers after refresh
    pub total_assigned: i32,
}

/// Information about a manager assignment
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagerAssignment {
    pub manager_uuid: Uuid,
    pub matched_tags: Vec<String>,
    pub selection_type: SelectionType,
}

/// How a manager was selected for a suite
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SelectionType {
    /// User explicitly specified this manager
    UserSpecified = 0,

    /// Auto-matched based on tags
    TagMatched = 1,
}

impl From<i32> for SelectionType {
    fn from(value: i32) -> Self {
        match value {
            0 => SelectionType::UserSpecified,
            1 => SelectionType::TagMatched,
            _ => SelectionType::TagMatched,
        }
    }
}

impl From<SelectionType> for i32 {
    fn from(sel: SelectionType) -> Self {
        sel as i32
    }
}

/// Request to add managers to a suite
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddManagersReq {
    /// List of manager UUIDs to add
    pub manager_uuids: Vec<Uuid>,
}

/// Response after adding managers
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddManagersResp {
    /// Managers successfully added
    pub added_managers: Vec<Uuid>,

    /// Managers rejected (no permission)
    pub rejected_managers: Vec<Uuid>,

    /// Reason for rejection (if any)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Request to remove managers from a suite
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoveManagersReq {
    /// List of manager UUIDs to remove
    pub manager_uuids: Vec<Uuid>,
}

/// Response after removing managers
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoveManagersResp {
    /// Number of managers removed
    pub removed_count: i32,
}
```

#### Node Manager Types

```rust
/// Request to register a new node manager
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterManagerReq {
    /// Capabilities/requirements (e.g., ["gpu", "linux", "cuda:11.8"])
    pub tags: Vec<String>,

    /// Labels for querying (e.g., ["datacenter:us-west", "machine_id:server-42"])
    #[serde(default)]
    pub labels: Vec<String>,

    /// Groups this manager belongs to (for permissions)
    pub groups: Vec<String>,

    /// Optional token lifetime (default: 30 days)
    #[serde(default, with = "humantime_serde::option")]
    pub lifetime: Option<Duration>,
}

/// Response after registering a manager
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterManagerResp {
    /// Unique UUID for this manager
    pub manager_uuid: Uuid,

    /// JWT authentication token
    pub token: String,

    /// WebSocket URL for persistent connection
    pub websocket_url: String,
}

/// Node manager state
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeManagerState {
    /// No suite assigned, waiting for work
    Idle = 0,

    /// Running env_preparation hook
    Preparing = 1,

    /// Workers running tasks
    Executing = 2,

    /// Running env_cleanup hook
    Cleanup = 3,

    /// Heartbeat timeout
    Offline = 4,
}

impl From<i32> for NodeManagerState {
    fn from(value: i32) -> Self {
        match value {
            0 => NodeManagerState::Idle,
            1 => NodeManagerState::Preparing,
            2 => NodeManagerState::Executing,
            3 => NodeManagerState::Cleanup,
            4 => NodeManagerState::Offline,
            _ => NodeManagerState::Idle,
        }
    }
}

impl From<NodeManagerState> for i32 {
    fn from(state: NodeManagerState) -> Self {
        state as i32
    }
}

/// Manager heartbeat request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagerHeartbeatReq {
    /// Current manager state
    pub state: NodeManagerState,

    /// UUID of assigned suite (if any)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assigned_suite_uuid: Option<Uuid>,

    /// Optional metrics
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metrics: Option<ManagerMetrics>,
}

/// Manager execution metrics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagerMetrics {
    /// Number of active workers
    pub active_workers: u32,

    /// Tasks completed in this session
    pub tasks_completed: u64,

    /// Tasks failed in this session
    pub tasks_failed: u64,
}

/// Query parameters for listing managers
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagerQueryReq {
    /// Filter by group name
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_name: Option<String>,

    /// Filter by tags (manager must have ALL specified tags)
    #[serde(default)]
    pub tags: Vec<String>,

    /// Filter by state
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<NodeManagerState>,

    /// Pagination: offset
    #[serde(default)]
    pub offset: i64,

    /// Pagination: limit (max 100)
    #[serde(default = "default_limit")]
    pub limit: i64,
}

/// Response for manager query
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagerQueryResp {
    /// Total count of matching managers
    pub count: i64,

    /// List of manager summaries
    pub managers: Vec<ManagerSummary>,
}

/// Summary information for a node manager
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagerSummary {
    pub uuid: Uuid,
    pub creator_username: String,
    pub tags: Vec<String>,
    pub labels: Vec<String>,
    pub state: NodeManagerState,

    #[serde(with = "time::serde::rfc3339")]
    pub last_heartbeat: OffsetDateTime,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub assigned_suite_uuid: Option<Uuid>,

    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// Request to shutdown a manager
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShutdownManagerReq {
    /// Shutdown operation type
    pub op: ShutdownOp,
}

/// Shutdown operation types
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum ShutdownOp {
    /// Gracefully finish current tasks then shutdown
    Graceful,

    /// Force immediate shutdown
    Force,
}

/// Response after requesting manager shutdown
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShutdownManagerResp {
    /// Manager state after shutdown request
    pub state: NodeManagerState,
}
```

#### Updated Task Submission Types

```rust
/// Updated task submission request (add suite_uuid field)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmitTaskReq {
    pub group_name: String,

    /// NEW: Optional suite UUID to assign task to
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suite_uuid: Option<Uuid>,

    pub tags: Vec<String>,

    #[serde(default)]
    pub labels: Vec<String>,

    #[serde(default, with = "humantime_serde::option")]
    pub timeout: Option<Duration>,

    #[serde(default)]
    pub priority: i32,

    pub task_spec: TaskSpec,
}

/// Response after submitting a task
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmitTaskResp {
    /// Internal task ID
    pub task_id: i64,

    /// Task UUID
    pub uuid: Uuid,

    /// Suite UUID (echoed back if provided)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suite_uuid: Option<Uuid>,
}
```

**Testing:**

Run `cargo build` to verify all types compile:

```bash
cd /path/to/mitosis/coordinator
cargo build --lib
```

Expected output: No compilation errors.

Test JSON serialization/deserialization:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_suite_req_serialization() {
        let req = CreateTaskSuiteReq {
            name: Some("test-suite".to_string()),
            description: None,
            group_name: "default".to_string(),
            tags: vec!["gpu".to_string()],
            labels: vec!["project:ml".to_string()],
            priority: 10,
            worker_schedule: WorkerSchedulePlan {
                worker_count: 4,
                cpu_binding: None,
                task_prefetch_count: 16,
            },
            env_preparation: None,
            env_cleanup: None,
        };

        let json = serde_json::to_string(&req).unwrap();
        let deserialized: CreateTaskSuiteReq = serde_json::from_str(&json).unwrap();
        assert_eq!(req.name, deserialized.name);
    }

    #[test]
    fn test_task_suite_state_conversion() {
        assert_eq!(TaskSuiteState::from(0), TaskSuiteState::Open);
        assert_eq!(TaskSuiteState::from(3), TaskSuiteState::Cancelled);
        assert_eq!(i32::from(TaskSuiteState::Closed), 1);
    }
}
```

**Deliverables:**
- ✅ All types compile without errors
- ✅ Serde serialization/deserialization works for all types
- ✅ Enum conversions (i32 ↔ enum) work correctly
- ✅ Optional fields handled properly (skip_serializing_if)
- ✅ Unit tests pass for all type conversions

---

## Piece 2.2: POST /suites - Create Task Suite

**Goal:** Implement endpoint to create a new task suite. Suite is created in Open state with no assigned managers.

**Files to Modify:**
- `src/api/suites.rs` (create new file)
- `src/routes.rs` (register route)

**Route Definition:**
```rust
// In src/api/suites.rs
async fn create_suite(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<CreateTaskSuiteReq>,
) -> Result<Json<CreateTaskSuiteResp>, ApiError> {
    // Implementation
}

// In src/routes.rs
pub fn suite_routes() -> Router<AppState> {
    Router::new()
        .route("/suites", post(create_suite))
}
```

**Implementation Steps:**

1. **Validate Request**
   ```rust
   if req.worker_schedule.worker_count < 1 || req.worker_schedule.worker_count > 256 {
       return Err(ApiError::BadRequest("worker_count must be between 1 and 256"));
   }
   if req.worker_schedule.task_prefetch_count == 0 {
       return Err(ApiError::BadRequest("task_prefetch_count must be > 0"));
   }
   if req.group_name.is_empty() {
       return Err(ApiError::BadRequest("group_name is required"));
   }
   ```

2. **Resolve Group ID**
   ```rust
   let group = sqlx::query!(
       "SELECT id, name FROM groups WHERE name = $1",
       req.group_name
   )
   .fetch_optional(&state.db)
   .await?
   .ok_or(ApiError::NotFound("Group not found"))?;
   ```

3. **Permission Check**
   ```rust
   let is_member = sqlx::query_scalar!(
       "SELECT EXISTS(
           SELECT 1 FROM user_group
           WHERE user_id = $1 AND group_id = $2
       )",
       claims.user_id,
       group.id
   )
   .fetch_one(&state.db)
   .await?
   .unwrap_or(false);

   if !is_member {
       return Err(ApiError::Forbidden("Not a member of group"));
   }
   ```

4. **Insert Suite Record**
   ```rust
   let suite_uuid = Uuid::new_v4();

   sqlx::query!(
       r#"
       INSERT INTO task_suites (
           uuid, name, description, group_id, creator_id,
           tags, labels, priority, worker_schedule,
           env_preparation, env_cleanup, state
       ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, 0)
       "#,
       suite_uuid,
       req.name,
       req.description,
       group.id,
       claims.user_id,
       &req.tags,
       serde_json::to_value(&req.labels)?,
       req.priority,
       serde_json::to_value(&req.worker_schedule)?,
       req.env_preparation.map(|h| serde_json::to_value(&h)).transpose()?,
       req.env_cleanup.map(|h| serde_json::to_value(&h)).transpose()?
   )
   .execute(&state.db)
   .await?;
   ```

5. **Return Response**
   ```rust
   Ok(Json(CreateTaskSuiteResp {
       uuid: suite_uuid,
       state: TaskSuiteState::Open,
       assigned_managers: vec![],
   }))
   ```

**Database Queries:**
- `SELECT id, name FROM groups WHERE name = ?`
- `SELECT EXISTS(...) FROM user_group WHERE user_id = ? AND group_id = ?`
- `INSERT INTO task_suites (...) VALUES (...)`

**Permission Checks:**
- User must be member of specified group

**Testing with curl:**

```bash
# 1. Create a suite (minimal fields)
curl -X POST http://localhost:8080/api/v1/suites \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "group_name": "default",
    "tags": ["gpu"],
    "labels": ["project:ml"],
    "priority": 10,
    "worker_schedule": {
      "worker_count": 4,
      "task_prefetch_count": 16
    }
  }'

# Expected: 200 OK with JSON response containing suite_uuid
# {
#   "uuid": "123e4567-e89b-12d3-a456-426614174000",
#   "state": "Open",
#   "assigned_managers": []
# }

# 2. Create suite with all optional fields
curl -X POST http://localhost:8080/api/v1/suites \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "name": "Training Run #42",
    "description": "ResNet training with mixed precision",
    "group_name": "ml-team",
    "tags": ["gpu", "cuda:11.8"],
    "labels": ["project:resnet", "phase:training"],
    "priority": 100,
    "worker_schedule": {
      "worker_count": 8,
      "cpu_binding": {
        "cores": [0, 1, 2, 3],
        "strategy": "Exclusive"
      },
      "task_prefetch_count": 32
    },
    "env_preparation": {
      "args": ["bash", "-c", "echo Preparing environment"],
      "envs": {"CUDA_VISIBLE_DEVICES": "0,1"},
      "resources": [],
      "timeout": "5m"
    }
  }'

# Expected: 200 OK with suite_uuid

# 3. Error case: Invalid worker_count
curl -X POST http://localhost:8080/api/v1/suites \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "group_name": "default",
    "tags": [],
    "labels": [],
    "priority": 0,
    "worker_schedule": {
      "worker_count": 500,
      "task_prefetch_count": 16
    }
  }'

# Expected: 400 Bad Request - "worker_count must be between 1 and 256"

# 4. Error case: Not a member of group
curl -X POST http://localhost:8080/api/v1/suites \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "group_name": "other-team",
    "tags": [],
    "labels": [],
    "priority": 0,
    "worker_schedule": {
      "worker_count": 4,
      "task_prefetch_count": 16
    }
  }'

# Expected: 403 Forbidden - "Not a member of group"
```

**Deliverables:**
- ✅ Route registered and handler responds to POST /suites
- ✅ Can create suite with minimal fields
- ✅ Can create suite with all optional fields (name, description, env hooks)
- ✅ Returns suite_uuid in response
- ✅ Suite created in database with state = 0 (Open)
- ✅ Validation rejects worker_count outside 1-256 range
- ✅ Permission check enforced (403 if not group member)
- ✅ Returns 404 if group doesn't exist

---

## Piece 2.3: GET /suites - Query Suites

**Goal:** Implement endpoint to list and filter task suites with pagination. Returns only suites from groups the user is a member of.

**Files to Modify:**
- `src/api/suites.rs`
- `src/routes.rs`

**Route Definition:**
```rust
// In src/api/suites.rs
async fn query_suites(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(req): Query<TaskSuiteQueryReq>,
) -> Result<Json<TaskSuiteQueryResp>, ApiError> {
    // Implementation
}

// In src/routes.rs - add to suite_routes()
Router::new()
    .route("/suites", get(query_suites).post(create_suite))
```

**Implementation Steps:**

1. **Build Base Query**
   ```rust
   use sqlx::QueryBuilder;

   let mut query = QueryBuilder::new(
       r#"
       SELECT
           ts.uuid, ts.name, ts.state, ts.priority,
           ts.total_tasks, ts.pending_tasks,
           ts.created_at, ts.updated_at,
           ts.tags, ts.labels,
           g.name as group_name
       FROM task_suites ts
       JOIN groups g ON ts.group_id = g.id
       WHERE EXISTS(
           SELECT 1 FROM user_group
           WHERE user_id = "#
   );
   query.push_bind(claims.user_id);
   query.push(" AND group_id = ts.group_id)");
   ```

2. **Add Optional Filters**
   ```rust
   if let Some(group_name) = &req.group_name {
       query.push(" AND g.name = ");
       query.push_bind(group_name);
   }

   if let Some(state) = req.state {
       query.push(" AND ts.state = ");
       query.push_bind(i32::from(state));
   }

   if !req.tags.is_empty() {
       query.push(" AND ts.tags @> ");
       query.push_bind(&req.tags);
   }

   if !req.labels.is_empty() {
       query.push(" AND ts.labels @> ");
       query.push_bind(serde_json::to_value(&req.labels)?);
   }
   ```

3. **Add Pagination**
   ```rust
   let limit = req.limit.min(100);
   query.push(" ORDER BY ts.created_at DESC LIMIT ");
   query.push_bind(limit);
   query.push(" OFFSET ");
   query.push_bind(req.offset);
   ```

4. **Execute Query**
   ```rust
   let suites = query
       .build_query_as::<TaskSuiteSummary>()
       .fetch_all(&state.db)
       .await?;

   let count = suites.len() as i64;
   ```

5. **Return Response**
   ```rust
   Ok(Json(TaskSuiteQueryResp {
       count,
       suites,
   }))
   ```

**Database Queries:**
- Dynamic `SELECT` with filters on `task_suites` joined with `groups`
- Permission filter via EXISTS subquery

**Permission Checks:**
- User must be member of suite's group (automatic via WHERE clause)

**Testing with curl:**

```bash
# 1. List all suites (no filters)
curl -X GET http://localhost:8080/api/v1/suites \
  -H "Authorization: Bearer $TOKEN"

# Expected: 200 OK with list of suites
# {
#   "count": 3,
#   "suites": [
#     {
#       "uuid": "...",
#       "name": "Suite 1",
#       "group_name": "default",
#       "tags": ["gpu"],
#       "labels": ["project:ml"],
#       "state": "Open",
#       "priority": 10,
#       "total_tasks": 0,
#       "pending_tasks": 0,
#       "created_at": "2025-01-15T10:00:00Z",
#       "updated_at": "2025-01-15T10:00:00Z"
#     },
#     ...
#   ]
# }

# 2. Filter by group_name
curl -X GET "http://localhost:8080/api/v1/suites?group_name=ml-team" \
  -H "Authorization: Bearer $TOKEN"

# Expected: Only suites from ml-team group

# 3. Filter by state
curl -X GET "http://localhost:8080/api/v1/suites?state=Open" \
  -H "Authorization: Bearer $TOKEN"

# Expected: Only suites in Open state

# 4. Filter by tags (containment)
curl -X GET "http://localhost:8080/api/v1/suites?tags=gpu&tags=cuda:11.8" \
  -H "Authorization: Bearer $TOKEN"

# Expected: Only suites that have BOTH tags

# 5. Filter by labels
curl -X GET "http://localhost:8080/api/v1/suites?labels=project:ml" \
  -H "Authorization: Bearer $TOKEN"

# Expected: Suites with matching label

# 6. Pagination
curl -X GET "http://localhost:8080/api/v1/suites?offset=10&limit=20" \
  -H "Authorization: Bearer $TOKEN"

# Expected: 20 suites starting from offset 10

# 7. Combined filters
curl -X GET "http://localhost:8080/api/v1/suites?group_name=default&state=Open&tags=gpu" \
  -H "Authorization: Bearer $TOKEN"

# Expected: Suites matching all criteria
```

**Deliverables:**
- ✅ Route registered and handler responds to GET /suites
- ✅ Returns list of suites user has access to
- ✅ Filters work: group_name, state, tags (array containment), labels
- ✅ Pagination works (offset/limit)
- ✅ Only returns suites from groups user is member of
- ✅ Empty result set returns count=0 and empty array
- ✅ Limit capped at 100 even if higher value requested

---

## Piece 2.4: GET /suites/{uuid} - Get Suite Details

**Goal:** Fetch complete details for a single suite, including all fields and assigned managers.

**Files to Modify:**
- `src/api/suites.rs`
- `src/routes.rs`

**Route Definition:**
```rust
// In src/api/suites.rs
async fn get_suite_details(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(suite_uuid): Path<Uuid>,
) -> Result<Json<TaskSuiteDetails>, ApiError> {
    // Implementation
}

// In src/routes.rs - add to suite_routes()
Router::new()
    .route("/suites/:uuid", get(get_suite_details))
```

**Implementation Steps:**

1. **Fetch Suite**
   ```rust
   let suite = sqlx::query!(
       r#"
       SELECT
           ts.*, g.name as group_name, u.username as creator_username
       FROM task_suites ts
       JOIN groups g ON ts.group_id = g.id
       JOIN users u ON ts.creator_id = u.id
       WHERE ts.uuid = $1
       "#,
       suite_uuid
   )
   .fetch_optional(&state.db)
   .await?
   .ok_or(ApiError::NotFound("Suite not found"))?;
   ```

2. **Permission Check**
   ```rust
   let has_access = sqlx::query_scalar!(
       "SELECT EXISTS(
           SELECT 1 FROM user_group
           WHERE user_id = $1 AND group_id = $2
       )",
       claims.user_id,
       suite.group_id
   )
   .fetch_one(&state.db)
   .await?
   .unwrap_or(false);

   if !has_access {
       return Err(ApiError::Forbidden("No access to this suite"));
   }
   ```

3. **Fetch Assigned Managers**
   ```rust
   let assigned_managers = sqlx::query_scalar!(
       r#"
       SELECT nm.uuid
       FROM task_suite_managers tsm
       JOIN node_managers nm ON tsm.manager_id = nm.id
       WHERE tsm.task_suite_id = $1
       ORDER BY tsm.assigned_at
       "#,
       suite.id
   )
   .fetch_all(&state.db)
   .await?;
   ```

4. **Deserialize JSON Fields**
   ```rust
   let worker_schedule: WorkerSchedulePlan = serde_json::from_value(suite.worker_schedule)?;
   let env_preparation: Option<EnvHookSpec> = suite.env_preparation
       .map(serde_json::from_value).transpose()?;
   let env_cleanup: Option<EnvHookSpec> = suite.env_cleanup
       .map(serde_json::from_value).transpose()?;
   let labels: Vec<String> = serde_json::from_value(suite.labels)?;
   ```

5. **Return Response**
   ```rust
   Ok(Json(TaskSuiteDetails {
       uuid: suite.uuid,
       name: suite.name,
       description: suite.description,
       group_name: suite.group_name,
       creator_username: suite.creator_username,
       tags: suite.tags,
       labels,
       priority: suite.priority,
       worker_schedule,
       env_preparation,
       env_cleanup,
       state: TaskSuiteState::from(suite.state),
       last_task_submitted_at: suite.last_task_submitted_at,
       total_tasks: suite.total_tasks,
       pending_tasks: suite.pending_tasks,
       created_at: suite.created_at,
       updated_at: suite.updated_at,
       completed_at: suite.completed_at,
       assigned_managers,
   }))
   ```

**Database Queries:**
- `SELECT ts.*, g.name, u.username FROM task_suites ts JOIN ... WHERE uuid = ?`
- `SELECT EXISTS(...) FROM user_group WHERE user_id = ? AND group_id = ?`
- `SELECT nm.uuid FROM task_suite_managers tsm JOIN node_managers nm WHERE task_suite_id = ?`

**Permission Checks:**
- User must be member of suite's group

**Testing with curl:**

```bash
# 1. Get suite details (success)
SUITE_UUID="123e4567-e89b-12d3-a456-426614174000"
curl -X GET http://localhost:8080/api/v1/suites/$SUITE_UUID \
  -H "Authorization: Bearer $TOKEN"

# Expected: 200 OK with full suite details
# {
#   "uuid": "123e4567-e89b-12d3-a456-426614174000",
#   "name": "Training Run #42",
#   "description": "ResNet training",
#   "group_name": "ml-team",
#   "creator_username": "alice",
#   "tags": ["gpu", "cuda:11.8"],
#   "labels": ["project:resnet"],
#   "priority": 100,
#   "worker_schedule": {
#     "worker_count": 8,
#     "task_prefetch_count": 32
#   },
#   "state": "Open",
#   "last_task_submitted_at": null,
#   "total_tasks": 0,
#   "pending_tasks": 0,
#   "created_at": "2025-01-15T10:00:00Z",
#   "updated_at": "2025-01-15T10:00:00Z",
#   "completed_at": null,
#   "assigned_managers": []
# }

# 2. Get non-existent suite (404)
curl -X GET http://localhost:8080/api/v1/suites/00000000-0000-0000-0000-000000000000 \
  -H "Authorization: Bearer $TOKEN"

# Expected: 404 Not Found - "Suite not found"

# 3. Get suite without permission (403)
# Use token from different user who's not in the suite's group
curl -X GET http://localhost:8080/api/v1/suites/$SUITE_UUID \
  -H "Authorization: Bearer $OTHER_USER_TOKEN"

# Expected: 403 Forbidden - "No access to this suite"
```

**Deliverables:**
- ✅ Route registered and handler responds to GET /suites/:uuid
- ✅ Returns full suite details including all fields
- ✅ Includes list of assigned manager UUIDs
- ✅ Returns 404 for non-existent suite UUID
- ✅ Returns 403 if user not member of suite's group
- ✅ JSON fields properly deserialized (worker_schedule, env hooks, labels)
- ✅ Timestamps formatted as RFC3339

---

## Piece 2.5: POST /suites/{uuid}/cancel - Cancel Suite

**Goal:** Cancel a task suite, optionally cancelling all pending/running tasks. Suite transitions to Cancelled state (terminal).

**Files to Modify:**
- `src/api/suites.rs`
- `src/routes.rs`

**Route Definition:**
```rust
// In src/api/suites.rs
async fn cancel_suite(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(suite_uuid): Path<Uuid>,
    Json(req): Json<CancelSuiteReq>,
) -> Result<Json<CancelSuiteResp>, ApiError> {
    // Implementation
}

// In src/routes.rs - add to suite_routes()
Router::new()
    .route("/suites/:uuid/cancel", post(cancel_suite))
```

**Implementation Steps:**

1. **Fetch Suite**
   ```rust
   let suite = sqlx::query!(
       "SELECT id, group_id, state FROM task_suites WHERE uuid = $1",
       suite_uuid
   )
   .fetch_optional(&state.db)
   .await?
   .ok_or(ApiError::NotFound("Suite not found"))?;
   ```

2. **Permission Check**
   ```rust
   let is_member = sqlx::query_scalar!(
       "SELECT EXISTS(SELECT 1 FROM user_group WHERE user_id = $1 AND group_id = $2)",
       claims.user_id, suite.group_id
   )
   .fetch_one(&state.db)
   .await?
   .unwrap_or(false);

   if !is_member {
       return Err(ApiError::Forbidden("Not authorized"));
   }
   ```

3. **Begin Transaction**
   ```rust
   let mut tx = state.db.begin().await?;
   ```

4. **Update Suite State**
   ```rust
   sqlx::query!(
       "UPDATE task_suites SET state = 3, updated_at = NOW() WHERE id = $1",
       suite.id
   )
   .execute(&mut *tx)
   .await?;
   ```

5. **Cancel Pending/Running Tasks (if requested)**
   ```rust
   let cancelled_count = if req.cancel_running_tasks {
       sqlx::query_scalar!(
           r#"
           UPDATE active_tasks
           SET state = 4, updated_at = NOW()
           WHERE task_suite_id = $1
             AND state IN (0, 1, 2)
           RETURNING COUNT(*)::int
           "#,
           suite.id
       )
       .fetch_one(&mut *tx)
       .await?
       .unwrap_or(0)
   } else {
       0
   };
   ```

6. **Commit Transaction**
   ```rust
   tx.commit().await?;
   ```

7. **Notify Managers via WebSocket (Phase 3)**
   ```rust
   // TODO: In Phase 3, send CancelSuite message to all assigned managers
   // state.websocket_manager.broadcast_to_suite(suite_uuid, CoordinatorMessage::CancelSuite { ... }).await?;
   ```

8. **Return Response**
   ```rust
   Ok(Json(CancelSuiteResp {
       cancelled_task_count: cancelled_count,
       suite_state: TaskSuiteState::Cancelled,
   }))
   ```

**Database Queries:**
- `SELECT id, group_id, state FROM task_suites WHERE uuid = ?`
- `SELECT EXISTS(...) FROM user_group WHERE user_id = ? AND group_id = ?`
- `UPDATE task_suites SET state = 3 WHERE id = ?`
- `UPDATE active_tasks SET state = 4 WHERE task_suite_id = ? AND state IN (0,1,2)` (conditional)

**Permission Checks:**
- User must be member of suite's group

**Testing with curl:**

```bash
SUITE_UUID="123e4567-e89b-12d3-a456-426614174000"

# 1. Cancel suite without cancelling tasks
curl -X POST http://localhost:8080/api/v1/suites/$SUITE_UUID/cancel \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "cancel_running_tasks": false
  }'

# Expected: 200 OK
# {
#   "cancelled_task_count": 0,
#   "suite_state": "Cancelled"
# }

# 2. Cancel suite WITH cancelling tasks
curl -X POST http://localhost:8080/api/v1/suites/$SUITE_UUID/cancel \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "reason": "Stopping training early",
    "cancel_running_tasks": true
  }'

# Expected: 200 OK with cancelled_task_count > 0
# {
#   "cancelled_task_count": 42,
#   "suite_state": "Cancelled"
# }

# 3. Verify suite state is Cancelled
curl -X GET http://localhost:8080/api/v1/suites/$SUITE_UUID \
  -H "Authorization: Bearer $TOKEN"

# Expected: Suite with state = "Cancelled"

# 4. Try to submit task to cancelled suite (should fail in Piece 2.13)
# This will be tested in Piece 2.13
```

**Deliverables:**
- ✅ Route registered and handler responds to POST /suites/:uuid/cancel
- ✅ Suite state updated to Cancelled (state = 3)
- ✅ Can cancel without affecting tasks (cancel_running_tasks = false)
- ✅ Can cancel tasks when requested (cancel_running_tasks = true)
- ✅ Returns correct cancelled_task_count
- ✅ Transaction ensures atomicity (suite + tasks updated together)
- ✅ Idempotent (calling cancel multiple times doesn't fail)
- ✅ Permission check enforced (403 if not group member)

---

## Piece 2.6: POST /suites/{uuid}/managers/refresh - Tag Matching

**Goal:** Implement automatic tag-based manager assignment. Remove all tag-matched managers and re-assign based on current tags.

**Files to Modify:**
- `src/api/suite_managers.rs` (create new file)
- `src/routes.rs`

**Route Definition:**
```rust
// In src/api/suite_managers.rs
async fn refresh_tag_matched_managers(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(suite_uuid): Path<Uuid>,
) -> Result<Json<RefreshManagersResp>, ApiError> {
    // Implementation
}

// In src/routes.rs
Router::new()
    .route("/suites/:uuid/managers/refresh", post(refresh_tag_matched_managers))
```

**Implementation Steps:**

1. **Fetch Suite**
   ```rust
   let suite = sqlx::query!(
       "SELECT id, group_id, tags FROM task_suites WHERE uuid = $1",
       suite_uuid
   )
   .fetch_optional(&state.db)
   .await?
   .ok_or(ApiError::NotFound("Suite not found"))?;
   ```

2. **Permission Check**
   ```rust
   let is_member = sqlx::query_scalar!(
       "SELECT EXISTS(SELECT 1 FROM user_group WHERE user_id = $1 AND group_id = $2)",
       claims.user_id, suite.group_id
   )
   .fetch_one(&state.db)
   .await?
   .unwrap_or(false);

   if !is_member {
       return Err(ApiError::Forbidden("Not authorized"));
   }
   ```

3. **Begin Transaction**
   ```rust
   let mut tx = state.db.begin().await?;
   ```

4. **Remove Existing Tag-Matched Managers**
   ```rust
   let removed_managers = sqlx::query_scalar!(
       r#"
       DELETE FROM task_suite_managers
       WHERE task_suite_id = $1 AND selection_type = 1
       RETURNING (SELECT uuid FROM node_managers WHERE id = manager_id)
       "#,
       suite.id
   )
   .fetch_all(&mut *tx)
   .await?;
   ```

5. **Find Eligible Managers (Tag Matching Algorithm)**
   ```rust
   // Manager is eligible if:
   // 1. manager.tags contains ALL suite.tags (suite.tags ⊆ manager.tags)
   // 2. suite.group_id has Write or Admin role on manager
   let eligible_managers = sqlx::query!(
       r#"
       SELECT nm.id, nm.uuid, nm.tags
       FROM node_managers nm
       WHERE nm.tags @> $1::text[]
         AND EXISTS(
           SELECT 1 FROM group_node_manager gnm
           WHERE gnm.manager_id = nm.id
             AND gnm.group_id = $2
             AND gnm.role >= 1
         )
       "#,
       &suite.tags,
       suite.group_id
   )
   .fetch_all(&mut *tx)
   .await?;
   ```

6. **Insert New Tag-Matched Managers**
   ```rust
   let mut added_managers = Vec::new();

   for manager in eligible_managers {
       sqlx::query!(
           r#"
           INSERT INTO task_suite_managers
               (task_suite_id, manager_id, selection_type, matched_tags, added_by_user_id)
           VALUES ($1, $2, 1, $3, $4)
           ON CONFLICT (task_suite_id, manager_id) DO NOTHING
           "#,
           suite.id,
           manager.id,
           &suite.tags,
           claims.user_id
       )
       .execute(&mut *tx)
       .await?;

       added_managers.push(ManagerAssignment {
           manager_uuid: manager.uuid,
           matched_tags: suite.tags.clone(),
           selection_type: SelectionType::TagMatched,
       });
   }
   ```

7. **Commit Transaction**
   ```rust
   tx.commit().await?;
   ```

8. **Return Response**
   ```rust
   Ok(Json(RefreshManagersResp {
       added_managers,
       removed_managers,
       total_assigned: added_managers.len() as i32,
   }))
   ```

**Database Queries:**
- `SELECT id, group_id, tags FROM task_suites WHERE uuid = ?`
- `DELETE FROM task_suite_managers WHERE task_suite_id = ? AND selection_type = 1`
- `SELECT nm.* FROM node_managers WHERE tags @> ? AND EXISTS(...)`
- `INSERT INTO task_suite_managers (...) VALUES (...)`

**Permission Checks:**
- User must be member of suite's group
- Tag matching: `suite.tags ⊆ manager.tags` (PostgreSQL @> operator)
- Role check: suite's group has Write/Admin (role >= 1) on manager

**Testing with curl:**

```bash
SUITE_UUID="123e4567-e89b-12d3-a456-426614174000"

# First, create a suite with tags=["gpu", "cuda:11.8"]
# Register some managers with matching tags

# 1. Refresh managers (initial assignment)
curl -X POST http://localhost:8080/api/v1/suites/$SUITE_UUID/managers/refresh \
  -H "Authorization: Bearer $TOKEN"

# Expected: 200 OK
# {
#   "added_managers": [
#     {
#       "manager_uuid": "...",
#       "matched_tags": ["gpu", "cuda:11.8"],
#       "selection_type": "TagMatched"
#     }
#   ],
#   "removed_managers": [],
#   "total_assigned": 1
# }

# 2. Refresh again (idempotent - should remove and re-add same managers)
curl -X POST http://localhost:8080/api/v1/suites/$SUITE_UUID/managers/refresh \
  -H "Authorization: Bearer $TOKEN"

# Expected: Same managers removed and added

# 3. Verify managers assigned via GET /suites/{uuid}
curl -X GET http://localhost:8080/api/v1/suites/$SUITE_UUID \
  -H "Authorization: Bearer $TOKEN"

# Expected: assigned_managers array contains manager UUIDs
```

**Deliverables:**
- ✅ Route registered and handler responds to POST /suites/:uuid/managers/refresh
- ✅ Tag matching algorithm implemented correctly (set containment)
- ✅ Removes old tag-matched managers (selection_type = 1)
- ✅ Adds new tag-matched managers based on tags
- ✅ Preserves user-specified managers (selection_type = 0)
- ✅ Permission check: group must have Write/Admin role on manager
- ✅ Transaction ensures atomicity
- ✅ Returns lists of added/removed managers

---

## Piece 2.7: POST /suites/{uuid}/managers - Add Managers Explicitly

**Goal:** Allow users to manually assign specific managers to a suite (UserSpecified selection type).

**Files to Modify:**
- `src/api/suite_managers.rs`
- `src/routes.rs`

**Route Definition:**
```rust
// In src/api/suite_managers.rs
async fn add_managers_to_suite(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(suite_uuid): Path<Uuid>,
    Json(req): Json<AddManagersReq>,
) -> Result<Json<AddManagersResp>, ApiError> {
    // Implementation
}

// In src/routes.rs
Router::new()
    .route("/suites/:uuid/managers", post(add_managers_to_suite))
```

**Implementation Steps:**

1. **Fetch Suite**
   ```rust
   let suite = sqlx::query!(
       "SELECT id, group_id FROM task_suites WHERE uuid = $1",
       suite_uuid
   )
   .fetch_optional(&state.db)
   .await?
   .ok_or(ApiError::NotFound("Suite not found"))?;
   ```

2. **Permission Check**
   ```rust
   let is_member = sqlx::query_scalar!(
       "SELECT EXISTS(SELECT 1 FROM user_group WHERE user_id = $1 AND group_id = $2)",
       claims.user_id, suite.group_id
   )
   .fetch_one(&state.db)
   .await?
   .unwrap_or(false);

   if !is_member {
       return Err(ApiError::Forbidden("Not authorized"));
   }
   ```

3. **For Each Manager UUID**
   ```rust
   let mut added_managers = Vec::new();
   let mut rejected_managers = Vec::new();

   for manager_uuid in &req.manager_uuids {
       // Check if manager exists and group has access
       let manager_check = sqlx::query!(
           r#"
           SELECT
               nm.id,
               EXISTS(
                   SELECT 1 FROM group_node_manager gnm
                   WHERE gnm.manager_id = nm.id
                     AND gnm.group_id = $1
                     AND gnm.role >= 1
               ) as has_access
           FROM node_managers nm
           WHERE nm.uuid = $2
           "#,
           suite.group_id,
           manager_uuid
       )
       .fetch_optional(&state.db)
       .await?;

       match manager_check {
           Some(row) if row.has_access.unwrap_or(false) => {
               // Add manager with UserSpecified selection type
               sqlx::query!(
                   r#"
                   INSERT INTO task_suite_managers
                       (task_suite_id, manager_id, selection_type, matched_tags, added_by_user_id)
                   VALUES ($1, $2, 0, ARRAY[]::text[], $3)
                   ON CONFLICT (task_suite_id, manager_id) DO NOTHING
                   "#,
                   suite.id,
                   row.id,
                   claims.user_id
               )
               .execute(&state.db)
               .await?;

               added_managers.push(*manager_uuid);
           }
           _ => {
               rejected_managers.push(*manager_uuid);
           }
       }
   }
   ```

4. **Return Response**
   ```rust
   Ok(Json(AddManagersResp {
       added_managers,
       rejected_managers,
       reason: if rejected_managers.is_empty() {
           None
       } else {
           Some("Manager not found or group lacks permission".to_string())
       },
   }))
   ```

**Database Queries:**
- `SELECT id, group_id FROM task_suites WHERE uuid = ?`
- For each manager: `SELECT id, EXISTS(...) FROM node_managers WHERE uuid = ?`
- `INSERT INTO task_suite_managers (...) VALUES (...)`

**Permission Checks:**
- User must be member of suite's group
- Suite's group must have Write/Admin role on each manager

**Testing with curl:**

```bash
SUITE_UUID="123e4567-e89b-12d3-a456-426614174000"
MANAGER_UUID_1="aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
MANAGER_UUID_2="ffffffff-gggg-hhhh-iiii-jjjjjjjjjjjj"

# 1. Add managers (all succeed)
curl -X POST http://localhost:8080/api/v1/suites/$SUITE_UUID/managers \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d "{
    \"manager_uuids\": [\"$MANAGER_UUID_1\", \"$MANAGER_UUID_2\"]
  }"

# Expected: 200 OK
# {
#   "added_managers": ["aaaaaaaa-...", "ffffffff-..."],
#   "rejected_managers": []
# }

# 2. Add manager without permission (rejected)
curl -X POST http://localhost:8080/api/v1/suites/$SUITE_UUID/managers \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d "{
    \"manager_uuids\": [\"00000000-0000-0000-0000-000000000000\"]
  }"

# Expected: 200 OK (partial success)
# {
#   "added_managers": [],
#   "rejected_managers": ["00000000-0000-0000-0000-000000000000"],
#   "reason": "Manager not found or group lacks permission"
# }
```

**Deliverables:**
- ✅ Route registered and handler responds to POST /suites/:uuid/managers
- ✅ Can add multiple managers at once
- ✅ Managers added with selection_type = 0 (UserSpecified)
- ✅ Permission check: group must have Write/Admin role on manager
- ✅ Returns separate lists for added and rejected managers
- ✅ ON CONFLICT DO NOTHING prevents duplicate assignments
- ✅ Idempotent (adding same manager twice doesn't fail)

---

## Piece 2.8: DELETE /suites/{uuid}/managers - Remove Managers

**Goal:** Remove manager assignments from a suite.

**Files to Modify:**
- `src/api/suite_managers.rs`
- `src/routes.rs`

**Route Definition:**
```rust
// In src/api/suite_managers.rs
async fn remove_managers_from_suite(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(suite_uuid): Path<Uuid>,
    Json(req): Json<RemoveManagersReq>,
) -> Result<Json<RemoveManagersResp>, ApiError> {
    // Implementation
}

// In src/routes.rs
Router::new()
    .route("/suites/:uuid/managers", delete(remove_managers_from_suite))
```

**Implementation Steps:**

1. **Fetch Suite**
   ```rust
   let suite = sqlx::query!(
       "SELECT id, group_id FROM task_suites WHERE uuid = $1",
       suite_uuid
   )
   .fetch_optional(&state.db)
   .await?
   .ok_or(ApiError::NotFound("Suite not found"))?;
   ```

2. **Permission Check**
   ```rust
   let is_member = sqlx::query_scalar!(
       "SELECT EXISTS(SELECT 1 FROM user_group WHERE user_id = $1 AND group_id = $2)",
       claims.user_id, suite.group_id
   )
   .fetch_one(&state.db)
   .await?
   .unwrap_or(false);

   if !is_member {
       return Err(ApiError::Forbidden("Not authorized"));
   }
   ```

3. **Delete Manager Assignments**
   ```rust
   let removed_count = sqlx::query!(
       r#"
       DELETE FROM task_suite_managers
       WHERE task_suite_id = $1
         AND manager_id IN (
           SELECT id FROM node_managers WHERE uuid = ANY($2)
         )
       "#,
       suite.id,
       &req.manager_uuids
   )
   .execute(&state.db)
   .await?
   .rows_affected() as i32;
   ```

4. **Return Response**
   ```rust
   Ok(Json(RemoveManagersResp {
       removed_count,
   }))
   ```

**Database Queries:**
- `SELECT id, group_id FROM task_suites WHERE uuid = ?`
- `DELETE FROM task_suite_managers WHERE task_suite_id = ? AND manager_id IN (...)`

**Permission Checks:**
- User must be member of suite's group

**Testing with curl:**

```bash
SUITE_UUID="123e4567-e89b-12d3-a456-426614174000"
MANAGER_UUID_1="aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"

# 1. Remove managers
curl -X DELETE http://localhost:8080/api/v1/suites/$SUITE_UUID/managers \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d "{
    \"manager_uuids\": [\"$MANAGER_UUID_1\"]
  }"

# Expected: 200 OK
# {
#   "removed_count": 1
# }

# 2. Remove again (idempotent - returns 0)
curl -X DELETE http://localhost:8080/api/v1/suites/$SUITE_UUID/managers \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d "{
    \"manager_uuids\": [\"$MANAGER_UUID_1\"]
  }"

# Expected: 200 OK
# {
#   "removed_count": 0
# }
```

**Deliverables:**
- ✅ Route registered and handler responds to DELETE /suites/:uuid/managers
- ✅ Can remove multiple managers at once
- ✅ Returns count of actually removed managers
- ✅ Idempotent (removing non-existent assignment returns 0, not error)
- ✅ Permission check enforced

---

## Piece 2.9: POST /managers - Register Manager

**Goal:** Register a new node manager, create JWT token, return WebSocket URL.

**Files to Modify:**
- `src/api/managers.rs` (create new file)
- `src/routes.rs`
- `src/auth/mod.rs` (add ManagerClaims type)

**Route Definition:**
```rust
// In src/api/managers.rs
async fn register_manager(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<RegisterManagerReq>,
) -> Result<Json<RegisterManagerResp>, ApiError> {
    // Implementation
}

// In src/routes.rs
pub fn manager_routes() -> Router<AppState> {
    Router::new()
        .route("/managers", post(register_manager))
}
```

**Implementation Steps:**

1. **Validate Request**
   ```rust
   if req.tags.is_empty() {
       return Err(ApiError::BadRequest("tags cannot be empty"));
   }
   if req.groups.is_empty() {
       return Err(ApiError::BadRequest("groups cannot be empty"));
   }
   ```

2. **Resolve Group IDs**
   ```rust
   let mut group_ids = Vec::new();
   for group_name in &req.groups {
       let group_id = sqlx::query_scalar!(
           "SELECT id FROM groups WHERE name = $1",
           group_name
       )
       .fetch_optional(&state.db)
       .await?
       .ok_or_else(|| ApiError::NotFound(format!("Group '{}' not found", group_name)))?;

       group_ids.push(group_id);
   }
   ```

3. **Insert Manager Record**
   ```rust
   let manager_uuid = Uuid::new_v4();

   let manager_id = sqlx::query_scalar!(
       r#"
       INSERT INTO node_managers (uuid, creator_id, tags, labels, state)
       VALUES ($1, $2, $3, $4, 0)
       RETURNING id
       "#,
       manager_uuid,
       claims.user_id,
       &req.tags,
       serde_json::to_value(&req.labels)?
   )
   .fetch_one(&state.db)
   .await?;
   ```

4. **Insert Group Permissions (Admin role)**
   ```rust
   for group_id in group_ids {
       sqlx::query!(
           r#"
           INSERT INTO group_node_manager (group_id, manager_id, role)
           VALUES ($1, $2, 2)
           "#,
           group_id,
           manager_id
       )
       .execute(&state.db)
       .await?;
   }
   ```

5. **Generate JWT Token**
   ```rust
   use jsonwebtoken::{encode, Header, Algorithm, EncodingKey};
   use time::OffsetDateTime;

   let lifetime = req.lifetime.unwrap_or(Duration::from_secs(30 * 24 * 3600)); // 30 days

   let jwt_claims = ManagerClaims {
       sub: manager_uuid.to_string(),
       exp: (OffsetDateTime::now_utc() + lifetime).unix_timestamp(),
       iat: OffsetDateTime::now_utc().unix_timestamp(),
       manager: true,
   };

   let token = encode(
       &Header::new(Algorithm::EdDSA),
       &jwt_claims,
       &state.jwt_encoding_key
   )?;
   ```

6. **Return Response**
   ```rust
   Ok(Json(RegisterManagerResp {
       manager_uuid,
       token,
       websocket_url: format!("{}/ws/managers", state.config.coordinator_url),
   }))
   ```

**Database Queries:**
- For each group: `SELECT id FROM groups WHERE name = ?`
- `INSERT INTO node_managers (...) VALUES (...) RETURNING id`
- For each group: `INSERT INTO group_node_manager (...) VALUES (...)`

**Permission Checks:**
- User must be authenticated (have valid JWT)

**Testing with curl:**

```bash
# 1. Register manager with valid groups
curl -X POST http://localhost:8080/api/v1/managers \
  -H "Authorization: Bearer $USER_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "tags": ["gpu", "cuda:11.8", "linux"],
    "labels": ["datacenter:us-west", "rack:42"],
    "groups": ["ml-team", "gpu-cluster"]
  }'

# Expected: 200 OK
# {
#   "manager_uuid": "deadbeef-1234-5678-9abc-def012345678",
#   "token": "eyJ0eXAiOiJKV1QiLCJhbGc...",
#   "websocket_url": "http://localhost:8080/ws/managers"
# }

# 2. Save manager token for later use
MANAGER_TOKEN="eyJ0eXAiOiJKV1QiLCJhbGc..."

# 3. Error: Empty tags
curl -X POST http://localhost:8080/api/v1/managers \
  -H "Authorization: Bearer $USER_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "tags": [],
    "labels": [],
    "groups": ["default"]
  }'

# Expected: 400 Bad Request - "tags cannot be empty"

# 4. Error: Non-existent group
curl -X POST http://localhost:8080/api/v1/managers \
  -H "Authorization: Bearer $USER_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "tags": ["cpu"],
    "labels": [],
    "groups": ["non-existent-group"]
  }'

# Expected: 404 Not Found - "Group 'non-existent-group' not found"
```

**Deliverables:**
- ✅ Route registered and handler responds to POST /managers
- ✅ Manager created in database with Idle state (state = 0)
- ✅ JWT token generated with manager claims (manager: true)
- ✅ WebSocket URL returned in response
- ✅ Group permissions created with Admin role (role = 2)
- ✅ Validation rejects empty tags/groups
- ✅ Returns 404 if group doesn't exist

---

## Piece 2.10: POST /managers/heartbeat - Manager Heartbeat

**Goal:** Allow managers to send heartbeats to update their state and metrics.

**Files to Modify:**
- `src/api/managers.rs`
- `src/routes.rs`
- `src/middleware/auth.rs` (add manager auth middleware)

**Route Definition:**
```rust
// In src/api/managers.rs
async fn manager_heartbeat(
    State(state): State<AppState>,
    Extension(manager_claims): Extension<ManagerClaims>,
    Json(req): Json<ManagerHeartbeatReq>,
) -> Result<StatusCode, ApiError> {
    // Implementation
}

// In src/routes.rs - add to manager_routes()
Router::new()
    .route("/managers/heartbeat", post(manager_heartbeat))
```

**Implementation Steps:**

1. **Extract Manager UUID from JWT**
   ```rust
   let manager_uuid = Uuid::parse_str(&manager_claims.sub)
       .map_err(|_| ApiError::BadRequest("Invalid manager UUID in token"))?;
   ```

2. **Resolve Suite ID (if assigned)**
   ```rust
   let assigned_suite_id = if let Some(suite_uuid) = req.assigned_suite_uuid {
       sqlx::query_scalar!(
           "SELECT id FROM task_suites WHERE uuid = $1",
           suite_uuid
       )
       .fetch_optional(&state.db)
       .await?
   } else {
       None
   };
   ```

3. **Update Manager Record**
   ```rust
   sqlx::query!(
       r#"
       UPDATE node_managers
       SET last_heartbeat = NOW(),
           state = $1,
           assigned_task_suite_id = $2,
           updated_at = NOW()
       WHERE uuid = $3
       "#,
       i32::from(req.state),
       assigned_suite_id,
       manager_uuid
   )
   .execute(&state.db)
   .await?;
   ```

4. **Store Metrics (Optional)**
   ```rust
   if let Some(metrics) = req.metrics {
       tracing::info!(
           "Manager {} metrics: workers={}, completed={}, failed={}",
           manager_uuid,
           metrics.active_workers,
           metrics.tasks_completed,
           metrics.tasks_failed
       );
       // TODO: Store in time-series database or send to monitoring system
   }
   ```

5. **Return Success**
   ```rust
   Ok(StatusCode::NO_CONTENT)
   ```

**Database Queries:**
- `SELECT id FROM task_suites WHERE uuid = ?` (conditional)
- `UPDATE node_managers SET last_heartbeat = NOW(), state = ?, assigned_task_suite_id = ? WHERE uuid = ?`

**Permission Checks:**
- Manager must have valid JWT token

**Testing with curl:**

```bash
MANAGER_TOKEN="eyJ0eXAiOiJKV1QiLCJhbGc..."

# 1. Send heartbeat (Idle state)
curl -X POST http://localhost:8080/api/v1/managers/heartbeat \
  -H "Authorization: Bearer $MANAGER_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "state": "Idle"
  }'

# Expected: 204 No Content

# 2. Send heartbeat with assigned suite
SUITE_UUID="123e4567-e89b-12d3-a456-426614174000"
curl -X POST http://localhost:8080/api/v1/managers/heartbeat \
  -H "Authorization: Bearer $MANAGER_TOKEN" \
  -H "Content-Type: application/json" \
  -d "{
    \"state\": \"Executing\",
    \"assigned_suite_uuid\": \"$SUITE_UUID\",
    \"metrics\": {
      \"active_workers\": 8,
      \"tasks_completed\": 142,
      \"tasks_failed\": 3
    }
  }"

# Expected: 204 No Content

# 3. Verify last_heartbeat updated
# Query manager via GET /managers (Piece 2.11) and check last_heartbeat timestamp
```

**Deliverables:**
- ✅ Route registered and handler responds to POST /managers/heartbeat
- ✅ Updates last_heartbeat timestamp
- ✅ Updates manager state
- ✅ Updates assigned_task_suite_id
- ✅ Accepts and logs metrics
- ✅ Returns 204 No Content on success
- ✅ Manager authentication required (JWT with manager: true claim)

---

## Piece 2.11: GET /managers - Query Managers

**Goal:** List and filter node managers with pagination.

**Files to Modify:**
- `src/api/managers.rs`
- `src/routes.rs`

**Route Definition:**
```rust
// In src/api/managers.rs
async fn query_managers(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(req): Query<ManagerQueryReq>,
) -> Result<Json<ManagerQueryResp>, ApiError> {
    // Implementation
}

// In src/routes.rs - add to manager_routes()
Router::new()
    .route("/managers", get(query_managers).post(register_manager))
```

**Implementation Steps:**

1. **Build Base Query**
   ```rust
   use sqlx::QueryBuilder;

   let mut query = QueryBuilder::new(
       r#"
       SELECT
           nm.uuid, nm.tags, nm.labels, nm.state,
           nm.last_heartbeat, nm.created_at,
           u.username as creator_username,
           ts.uuid as assigned_suite_uuid
       FROM node_managers nm
       JOIN users u ON nm.creator_id = u.id
       LEFT JOIN task_suites ts ON nm.assigned_task_suite_id = ts.id
       WHERE EXISTS(
           SELECT 1 FROM group_node_manager gnm
           JOIN user_group ug ON gnm.group_id = ug.group_id
           WHERE gnm.manager_id = nm.id
             AND ug.user_id = "#
   );
   query.push_bind(claims.user_id);
   query.push(")");
   ```

2. **Add Optional Filters**
   ```rust
   if let Some(group_name) = &req.group_name {
       query.push(" AND EXISTS(SELECT 1 FROM group_node_manager gnm JOIN groups g ON gnm.group_id = g.id WHERE gnm.manager_id = nm.id AND g.name = ");
       query.push_bind(group_name);
       query.push(")");
   }

   if let Some(state) = req.state {
       query.push(" AND nm.state = ");
       query.push_bind(i32::from(state));
   }

   if !req.tags.is_empty() {
       query.push(" AND nm.tags @> ");
       query.push_bind(&req.tags);
   }
   ```

3. **Add Pagination**
   ```rust
   let limit = req.limit.min(100);
   query.push(" ORDER BY nm.created_at DESC LIMIT ");
   query.push_bind(limit);
   query.push(" OFFSET ");
   query.push_bind(req.offset);
   ```

4. **Execute Query**
   ```rust
   let managers = query
       .build_query_as::<ManagerSummary>()
       .fetch_all(&state.db)
       .await?;

   let count = managers.len() as i64;
   ```

5. **Return Response**
   ```rust
   Ok(Json(ManagerQueryResp {
       count,
       managers,
   }))
   ```

**Database Queries:**
- Dynamic `SELECT` with filters on `node_managers` joined with `users` and `task_suites`
- Permission filter via EXISTS subquery

**Permission Checks:**
- User must be member of groups that have access to managers

**Testing with curl:**

```bash
# 1. List all managers
curl -X GET http://localhost:8080/api/v1/managers \
  -H "Authorization: Bearer $USER_TOKEN"

# Expected: 200 OK with list of managers
# {
#   "count": 2,
#   "managers": [
#     {
#       "uuid": "...",
#       "creator_username": "alice",
#       "tags": ["gpu", "cuda:11.8"],
#       "labels": ["datacenter:us-west"],
#       "state": "Idle",
#       "last_heartbeat": "2025-01-15T10:05:00Z",
#       "assigned_suite_uuid": null,
#       "created_at": "2025-01-15T10:00:00Z"
#     }
#   ]
# }

# 2. Filter by state
curl -X GET "http://localhost:8080/api/v1/managers?state=Executing" \
  -H "Authorization: Bearer $USER_TOKEN"

# Expected: Only managers in Executing state

# 3. Filter by tags
curl -X GET "http://localhost:8080/api/v1/managers?tags=gpu&tags=cuda:11.8" \
  -H "Authorization: Bearer $USER_TOKEN"

# Expected: Only managers with both tags
```

**Deliverables:**
- ✅ Route registered and handler responds to GET /managers
- ✅ Returns list of managers user has access to
- ✅ Filters work: group_name, state, tags
- ✅ Pagination works (offset/limit)
- ✅ Only returns managers from groups user is member of
- ✅ Includes assigned_suite_uuid if manager is assigned
- ✅ Limit capped at 100

---

## Piece 2.12: POST /managers/{uuid}/shutdown - Shutdown Manager

**Goal:** Request manager shutdown (graceful or force). In Phase 3, this will send WebSocket message to manager.

**Files to Modify:**
- `src/api/managers.rs`
- `src/routes.rs`

**Route Definition:**
```rust
// In src/api/managers.rs
async fn shutdown_manager(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(manager_uuid): Path<Uuid>,
    Json(req): Json<ShutdownManagerReq>,
) -> Result<Json<ShutdownManagerResp>, ApiError> {
    // Implementation
}

// In src/routes.rs - add to manager_routes()
Router::new()
    .route("/managers/:uuid/shutdown", post(shutdown_manager))
```

**Implementation Steps:**

1. **Fetch Manager**
   ```rust
   let manager = sqlx::query!(
       "SELECT id, state FROM node_managers WHERE uuid = $1",
       manager_uuid
   )
   .fetch_optional(&state.db)
   .await?
   .ok_or(ApiError::NotFound("Manager not found"))?;
   ```

2. **Permission Check (Admin role required)**
   ```rust
   let is_admin = sqlx::query_scalar!(
       r#"
       SELECT EXISTS(
           SELECT 1 FROM group_node_manager gnm
           JOIN user_group ug ON gnm.group_id = ug.group_id
           WHERE ug.user_id = $1
             AND gnm.manager_id = $2
             AND gnm.role = 2
       )
       "#,
       claims.user_id,
       manager.id
   )
   .fetch_one(&state.db)
   .await?
   .unwrap_or(false);

   if !is_admin {
       return Err(ApiError::Forbidden("Requires Admin role on manager"));
   }
   ```

3. **Send Shutdown Message via WebSocket (Phase 3)**
   ```rust
   // TODO: In Phase 3, send shutdown message to manager
   // state.websocket_manager.send_to_manager(
   //     manager_uuid,
   //     CoordinatorMessage::Shutdown {
   //         graceful: matches!(req.op, ShutdownOp::Graceful)
   //     }
   // ).await?;

   tracing::info!("Shutdown requested for manager {}: {:?}", manager_uuid, req.op);
   ```

4. **Return Response**
   ```rust
   Ok(Json(ShutdownManagerResp {
       state: NodeManagerState::from(manager.state),
   }))
   ```

**Database Queries:**
- `SELECT id, state FROM node_managers WHERE uuid = ?`
- `SELECT EXISTS(...) FROM group_node_manager JOIN user_group WHERE ... AND role = 2`

**Permission Checks:**
- User must have Admin role (role = 2) on manager via group membership

**Testing with curl:**

```bash
MANAGER_UUID="deadbeef-1234-5678-9abc-def012345678"

# 1. Request graceful shutdown (with Admin role)
curl -X POST http://localhost:8080/api/v1/managers/$MANAGER_UUID/shutdown \
  -H "Authorization: Bearer $ADMIN_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "op": "Graceful"
  }'

# Expected: 200 OK
# {
#   "state": "Idle"
# }

# 2. Request force shutdown
curl -X POST http://localhost:8080/api/v1/managers/$MANAGER_UUID/shutdown \
  -H "Authorization: Bearer $ADMIN_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "op": "Force"
  }'

# Expected: 200 OK

# 3. Error: Without Admin role
curl -X POST http://localhost:8080/api/v1/managers/$MANAGER_UUID/shutdown \
  -H "Authorization: Bearer $NON_ADMIN_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "op": "Graceful"
  }'

# Expected: 403 Forbidden - "Requires Admin role on manager"
```

**Deliverables:**
- ✅ Route registered and handler responds to POST /managers/:uuid/shutdown
- ✅ Permission check: requires Admin role (role = 2)
- ✅ Logs shutdown request (will send WebSocket message in Phase 3)
- ✅ Returns current manager state
- ✅ Handles both Graceful and Force shutdown ops
- ✅ Returns 403 if user lacks Admin role
- ✅ Returns 404 if manager doesn't exist

---

## Piece 2.13: Update POST /tasks to Support suite_uuid

**Goal:** Update existing task submission endpoint to support optional suite_uuid field. Link tasks to suites and trigger database counters.

**Files to Modify:**
- `src/api/tasks.rs` (existing file)
- `src/schema.rs` (already updated in Piece 2.1)

**Route:** `POST /tasks` (existing)

**Implementation Steps:**

1. **Update SubmitTaskReq Type (Already done in Piece 2.1)**
   ```rust
   // In src/schema.rs
   #[derive(Debug, Clone, Serialize, Deserialize)]
   pub struct SubmitTaskReq {
       pub group_name: String,

       /// NEW: Optional suite UUID to assign task to
       #[serde(default, skip_serializing_if = "Option::is_none")]
       pub suite_uuid: Option<Uuid>,

       // ... existing fields ...
   }
   ```

2. **Add Suite Resolution Logic (in existing submit_task handler)**
   ```rust
   // In src/api/tasks.rs - modify existing submit_task function

   // ... existing validation code ...

   // NEW: Resolve suite_id if suite_uuid provided
   let task_suite_id = if let Some(suite_uuid) = req.suite_uuid {
       // Fetch suite
       let suite = sqlx::query!(
           "SELECT id, group_id, state FROM task_suites WHERE uuid = $1",
           suite_uuid
       )
       .fetch_optional(&state.db)
       .await?
       .ok_or(ApiError::NotFound("Suite not found"))?;

       // Check suite is not cancelled
       if suite.state == 3 {
           return Err(ApiError::BadRequest("Cannot submit tasks to cancelled suite"));
       }

       // Permission check: user must be member of suite's group
       let is_member = sqlx::query_scalar!(
           "SELECT EXISTS(SELECT 1 FROM user_group WHERE user_id = $1 AND group_id = $2)",
           claims.user_id,
           suite.group_id
       )
       .fetch_one(&state.db)
       .await?
       .unwrap_or(false);

       if !is_member {
           return Err(ApiError::Forbidden("Not authorized to submit to this suite"));
       }

       Some(suite.id)
   } else {
       None
   };
   ```

3. **Update INSERT Statement**
   ```rust
   // MODIFIED: Add task_suite_id to INSERT
   let task_id = sqlx::query_scalar!(
       r#"
       INSERT INTO active_tasks (
           uuid, group_id, task_suite_id, tags, labels,
           priority, timeout, task_spec, state, submitter_id
       ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 0, $9)
       RETURNING id
       "#,
       task_uuid,
       group_id,
       task_suite_id,  // NEW field
       &req.tags,
       serde_json::to_value(&req.labels)?,
       req.priority,
       req.timeout.map(|d| d.as_secs() as i32),
       serde_json::to_value(&req.task_spec)?,
       claims.user_id
   )
   .fetch_one(&state.db)
   .await?;

   // NOTE: Database trigger on_task_submitted() will automatically update:
   // - suite.total_tasks += 1
   // - suite.pending_tasks += 1
   // - suite.last_task_submitted_at = NOW()
   // - suite.state = 0 (if was Closed or Complete)
   ```

4. **Update Response**
   ```rust
   // Return response with suite_uuid
   Ok(Json(SubmitTaskResp {
       task_id,
       uuid: task_uuid,
       suite_uuid: req.suite_uuid,  // Echo back
   }))
   ```

**Database Queries (Added):**
- `SELECT id, group_id, state FROM task_suites WHERE uuid = ?` (conditional)
- `SELECT EXISTS(...) FROM user_group WHERE user_id = ? AND group_id = ?` (conditional)
- Modified: `INSERT INTO active_tasks (..., task_suite_id, ...) VALUES (...)`

**Permission Checks (Added):**
- User must be member of suite's group (if suite_uuid provided)

**Database Trigger Integration:**

The `on_task_submitted()` trigger (created in Phase 1) will automatically:
- Increment `task_suites.total_tasks`
- Increment `task_suites.pending_tasks`
- Update `task_suites.last_task_submitted_at` to NOW()
- Set `task_suites.state` to 0 (Open) if it was Closed (1) or Complete (2)

**Testing with curl:**

```bash
# 1. Submit task without suite (independent task)
curl -X POST http://localhost:8080/api/v1/tasks \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "group_name": "default",
    "tags": ["cpu"],
    "labels": [],
    "priority": 0,
    "task_spec": {
      "args": ["echo", "hello"],
      "envs": {},
      "resources": []
    }
  }'

# Expected: 200 OK (suite_uuid is null)
# {
#   "task_id": 123,
#   "uuid": "...",
#   "suite_uuid": null
# }

# 2. Submit task to suite
SUITE_UUID="123e4567-e89b-12d3-a456-426614174000"
curl -X POST http://localhost:8080/api/v1/tasks \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d "{
    \"group_name\": \"ml-team\",
    \"suite_uuid\": \"$SUITE_UUID\",
    \"tags\": [\"gpu\"],
    \"labels\": [],
    \"priority\": 10,
    \"task_spec\": {
      \"args\": [\"python\", \"train.py\"],
      \"envs\": {},
      \"resources\": []
    }
  }"

# Expected: 200 OK with suite_uuid echoed back
# {
#   "task_id": 124,
#   "uuid": "...",
#   "suite_uuid": "123e4567-e89b-12d3-a456-426614174000"
# }

# 3. Verify suite counters updated via GET /suites/{uuid}
curl -X GET http://localhost:8080/api/v1/suites/$SUITE_UUID \
  -H "Authorization: Bearer $TOKEN"

# Expected: total_tasks = 1, pending_tasks = 1, last_task_submitted_at is recent

# 4. Submit multiple tasks and verify counters
for i in {1..10}; do
  curl -X POST http://localhost:8080/api/v1/tasks \
    -H "Authorization: Bearer $TOKEN" \
    -H "Content-Type: application/json" \
    -d "{
      \"group_name\": \"ml-team\",
      \"suite_uuid\": \"$SUITE_UUID\",
      \"tags\": [\"gpu\"],
      \"labels\": [],
      \"priority\": 0,
      \"task_spec\": {
        \"args\": [\"echo\", \"task-$i\"],
        \"envs\": {},
        \"resources\": []
      }
    }"
done

# Expected: total_tasks = 11, pending_tasks = 11

# 5. Error: Submit to non-existent suite
curl -X POST http://localhost:8080/api/v1/tasks \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "group_name": "default",
    "suite_uuid": "00000000-0000-0000-0000-000000000000",
    "tags": [],
    "labels": [],
    "priority": 0,
    "task_spec": {
      "args": ["echo", "test"],
      "envs": {},
      "resources": []
    }
  }'

# Expected: 404 Not Found - "Suite not found"

# 6. Error: Submit to cancelled suite
# First cancel a suite, then try to submit
curl -X POST http://localhost:8080/api/v1/suites/$SUITE_UUID/cancel \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"cancel_running_tasks": false}'

curl -X POST http://localhost:8080/api/v1/tasks \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d "{
    \"group_name\": \"ml-team\",
    \"suite_uuid\": \"$SUITE_UUID\",
    \"tags\": [],
    \"labels\": [],
    \"priority\": 0,
    \"task_spec\": {
      \"args\": [\"echo\", \"test\"],
      \"envs\": {},
      \"resources\": []
    }
  }"

# Expected: 400 Bad Request - "Cannot submit tasks to cancelled suite"

# 7. Error: Submit to suite without permission
curl -X POST http://localhost:8080/api/v1/tasks \
  -H "Authorization: Bearer $OTHER_USER_TOKEN" \
  -H "Content-Type: application/json" \
  -d "{
    \"group_name\": \"ml-team\",
    \"suite_uuid\": \"$SUITE_UUID\",
    \"tags\": [],
    \"labels\": [],
    \"priority\": 0,
    \"task_spec\": {
      \"args\": [\"echo\", \"test\"],
      \"envs\": {},
      \"resources\": []
    }
  }"

# Expected: 403 Forbidden - "Not authorized to submit to this suite"
```

**Deliverables:**
- ✅ Existing POST /tasks endpoint updated
- ✅ Accepts optional suite_uuid field
- ✅ Links task to suite via task_suite_id foreign key
- ✅ Validates suite exists and is not cancelled
- ✅ Permission check: user must be member of suite's group
- ✅ Database trigger updates suite counters automatically
- ✅ Returns suite_uuid in response
- ✅ Backward compatible (can still submit independent tasks without suite_uuid)
- ✅ Suite state transitions from Closed/Complete → Open on new task submission

---
## Testing Checklist

### Unit Tests

#### Suite Management
- [ ] Create suite with minimal fields
- [ ] Create suite with all optional fields
- [ ] Create suite with invalid worker_count (< 1, > 256)
- [ ] Create suite as non-member of group (should fail)
- [ ] Query suites with no filters
- [ ] Query suites filtered by group_name
- [ ] Query suites filtered by state
- [ ] Query suites filtered by tags (containment)
- [ ] Query suites filtered by labels (containment)
- [ ] Query suites with pagination
- [ ] Get suite details (success)
- [ ] Get non-existent suite (404)
- [ ] Get suite without permission (403)
- [ ] Cancel suite with running tasks
- [ ] Cancel suite without cancelling tasks
- [ ] Cancel already cancelled suite (idempotent)

#### Manager Assignment
- [ ] Refresh tag-matched managers (add 2, remove 1)
- [ ] Refresh with no eligible managers
- [ ] Refresh with suite having no tags
- [ ] Add managers explicitly (all succeed)
- [ ] Add managers with mixed permissions (some rejected)
- [ ] Add non-existent manager UUID (rejected)
- [ ] Remove managers (success)
- [ ] Remove non-assigned managers (idempotent)

#### Node Manager
- [ ] Register manager with valid groups
- [ ] Register manager with non-existent group (should fail)
- [ ] Register manager with empty tags (should fail)
- [ ] Verify JWT token generation
- [ ] Manager heartbeat updates last_heartbeat
- [ ] Manager heartbeat updates state
- [ ] Query managers with filters
- [ ] Shutdown manager with Admin role
- [ ] Shutdown manager without Admin role (403)

#### Task Submission
- [ ] Submit task without suite_uuid (independent task)
- [ ] Submit task with valid suite_uuid
- [ ] Submit task to non-existent suite (404)
- [ ] Submit task to cancelled suite (400)
- [ ] Submit task to suite without permission (403)
- [ ] Verify suite task counts update via trigger

### Integration Tests

#### Permission Model
- [ ] User A cannot access User B's suite
- [ ] User can access suite from shared group
- [ ] Manager can only be assigned to suites with Write/Admin role
- [ ] Admin role required for manager shutdown

#### Tag Matching Algorithm
- [ ] Suite tags ["gpu"] matches manager tags ["gpu", "linux"]
- [ ] Suite tags ["gpu", "cuda:11.8"] matches manager tags ["gpu", "linux", "cuda:11.8"]
- [ ] Suite tags ["gpu", "cuda:11.8"] does NOT match manager tags ["gpu", "cuda:12.0"]
- [ ] Empty suite tags match all managers
- [ ] Tag refresh removes old matches, adds new matches

#### State Transitions
- [ ] Suite starts in Open state
- [ ] Suite transitions Open → Closed after 3 minutes (via background job)
- [ ] Suite transitions Closed → Open on new task submission
- [ ] Suite transitions Open → Cancelled on cancel request
- [ ] Cancelled state is terminal

#### Database Triggers
- [ ] Submitting task increments suite.total_tasks and suite.pending_tasks
- [ ] Finishing task decrements suite.pending_tasks
- [ ] Cancelling task decrements suite.pending_tasks
- [ ] Multiple concurrent task submissions update counts correctly

### API Contract Tests

- [ ] All endpoints return correct HTTP status codes
- [ ] Error responses include descriptive messages
- [ ] Pagination works correctly (offset/limit)
- [ ] JSON serialization handles all data types (UUID, timestamps, enums)
- [ ] Optional fields are omitted from JSON when null

### Performance Tests

- [ ] Query 10,000 suites with filters (< 100ms)
- [ ] Create 1,000 suites concurrently (no deadlocks)
- [ ] Refresh tag-matched managers with 100 eligible managers (< 500ms)
- [ ] Submit 10,000 tasks to same suite (trigger performance)

---

## Success Criteria

### Functional Requirements
✅ All 11 endpoints implemented and working:
- 7 suite management endpoints
- 4 manager endpoints
- 1 updated task submission endpoint

✅ All request/response types defined with proper serialization

✅ Permission model enforced:
- Group membership checked for all suite operations
- Role-based access for manager operations
- Tag matching respects group permissions

✅ Database integration:
- All queries optimized with proper indices
- Transactions used for multi-step operations
- Triggers functioning correctly

### Code Quality
✅ All code passes `cargo clippy` with no warnings
✅ All code formatted with `rustfmt`
✅ Comprehensive error handling (no unwraps in production code)
✅ Logging at appropriate levels (info, warn, error)

### Testing
✅ All unit tests pass
✅ All integration tests pass
✅ Code coverage > 80%
✅ API documentation generated (swagger/openapi)

### Documentation
✅ All endpoints documented with examples
✅ Permission model documented
✅ Tag matching algorithm documented
✅ Database schema changes documented in migration

---

## Dependencies

### Upstream (Must Complete Before Phase 2)
- **Phase 1: Database Schema**
  - All tables created and migrated
  - All indices created
  - All triggers created
  - SeaORM entities generated

### Downstream (Depends on Phase 2)
- **Phase 3: WebSocket Manager**
  - Requires message type definitions from Phase 2
  - Requires manager authentication (JWT) from Phase 2
  - Requires suite/manager database models from Phase 2

---

## Next Phase

**Phase 3: WebSocket Manager (1 week)**

After completing Phase 2, proceed to implementing the WebSocket layer for persistent manager connections:
- WebSocket server in coordinator
- Connection registry (manager UUID → WebSocket)
- Message routing and multiplexing
- Push-based suite assignment
- Task fetch/report over WebSocket
- Heartbeat timeout detection
- Reconnection handling

**Key Handoff:**
- All type definitions from `schema.rs` will be reused for WebSocket messages
- Database queries for manager/suite state will be called from WebSocket handlers
- Permission checks will be applied to WebSocket operations

---

## Additional Resources

### RFC References
- Full RFC: `/home/user/mitosis/rfc.md`
- Section 7 (API Design): Lines 816-1137
- Section 4 (Core Concepts): Lines 213-434
- Section 6 (Data Models): Lines 550-815
- Section 11 (Security): Lines 2310-2473

### Code Locations
- API types: `src/schema.rs` or `src/api/types.rs`
- Route handlers: `src/api/suites.rs`, `src/api/managers.rs`
- Permission utils: `src/auth/permissions.rs`
- Database queries: Inline in handlers or `src/db/queries.rs`

### Tools
- Axum docs: https://docs.rs/axum
- SeaORM docs: https://www.sea-ql.org/SeaORM/
- JWT (jsonwebtoken): https://docs.rs/jsonwebtoken

---

**End of Phase 2 Implementation Guide**
