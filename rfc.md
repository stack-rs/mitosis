# RFC: Node Manager and Task Suite System

**Status:** ✅ Design Complete - Ready for Implementation
**Version:** 1.0.0
**Last Updated:** 2025-11-11
**Authors:** Design Team

---

## Table of Contents

1. [Executive Summary](#executive-summary)
2. [Current Architecture](#current-architecture)
3. [Motivation and Use Cases](#motivation-and-use-cases)
4. [Core Concepts](#core-concepts)
5. [Architecture Design](#architecture-design)
6. [Data Models](#data-models)
7. [API Design](#api-design)
8. [Communication Protocols](#communication-protocols)
9. [State Machines and Workflows](#state-machines-and-workflows)
10. [Error Handling and Fault Tolerance](#error-handling-and-fault-tolerance)
11. [Security and Permissions](#security-and-permissions)
12. [Performance and Scalability](#performance-and-scalability)
13. [Migration Strategy](#migration-strategy)
14. [Implementation Plan](#implementation-plan)
15. [Appendices](#appendices)

---

## 1. Executive Summary

This RFC proposes a comprehensive enhancement to the Mitosis task execution system by introducing two new first-class concepts:

1. **Node Manager**: A device-level service that manages worker pools and orchestrates task execution with environment lifecycle management
2. **Task Suite**: A collection of related tasks with shared execution environment, scheduling policies, and lifecycle management

### Key Benefits

- **Device-Level Resource Management**: Prevent resource contention between workers on the same machine
- **Environment Orchestration**: Automated setup/teardown hooks at the suite level
- **Batch Job Support**: Natural abstraction for ML training campaigns, CI/CD pipelines, and data processing workflows
- **Improved Efficiency**: WebSocket-based communication, task pre-fetching, and reduced polling overhead
- **Better Reliability**: Automatic worker recovery, failure tracking, and intelligent retry strategies

### Design Principles

1. **Backward Compatibility**: Independent workers continue to work unchanged
2. **Simplicity**: Start with essential features, extend incrementally
3. **Robustness**: Graceful degradation, automatic recovery, clear error handling
4. **Consistency**: Align with existing patterns (permissions, authentication, state management)
5. **Performance**: Minimize latency, maximize throughput for managed workers

---

## 2. Current Architecture

### 2.1 Existing Components

#### Workers (Independent Mode)

**Lifecycle:**
- Workers register with coordinator via `POST /workers`
- Receive JWT token for authentication
- Poll for tasks via `GET /workers/tasks` (default 5s interval)
- Execute tasks, upload results to S3
- Report status via `POST /workers/tasks` (Finish/Cancel/Commit/Upload)
- Send heartbeats via `POST /workers/heartbeat` (default 600s timeout)

**Properties:**
- Tags: For task matching (e.g., `["gpu", "linux", "x86_64"]`)
- Labels: For querying/filtering (e.g., `["datacenter:us-west", "tier:production"]`)
- Groups: Workers belong to groups with permissions (Read/Write/Admin roles)

**Permission Model:**
```
Groups have roles ON workers (via group_worker table):
- Read: Reserved for future use
- Write: Group can submit tasks to worker
- Admin: Group can manage worker ACL
```

#### Coordinator

**Core Services:**
- **TaskDispatcher**: In-memory priority queues per worker
  `HashMap<WorkerId → PriorityQueue<TaskId, Priority>>`
- **HeartbeatQueue**: Priority queue tracking worker timeout
- **Authentication**: JWT-based (EdDSA) with group-based access control
- **Task Distribution**: Batch assigns tasks to eligible workers based on tags and group membership

**Database (PostgreSQL):**
- `users`, `groups`, `user_group` - Authentication and authorization
- `workers`, `group_worker` - Worker registration and permissions
- `active_tasks` - Tasks in flight
- `archived_tasks` - Completed tasks
- `artifacts`, `attachments` - S3-backed file storage

**Current Task Flow:**
```
User → Submit task to Group
     → Task enters coordinator queue
     → Worker polls and fetches task (tag-matched)
     → Worker executes task
     → Worker uploads artifacts to S3
     → Worker commits result
     → Task archived
```

### 2.2 Current Limitations

1. **No Device-Level Resource Management**
   Multiple workers on same device compete for GPU, CPU cores, memory

2. **No Environment Orchestration**
   Cannot prepare shared environments (download datasets, setup containers)

3. **Single-Task Granularity**
   No grouping of related tasks with shared lifecycle

4. **Polling Overhead**
   Workers constantly poll even when idle, increasing coordinator load

5. **No Batch Job Abstraction**
   ML campaigns, CI/CD pipelines require manual coordination

---

## 3. Motivation and Use Cases

### 3.1 Core Problems

#### Resource Contention
**Problem:** Multiple independent workers on same GPU server conflict:
- Worker A: Binds to GPU 0, uses 80% VRAM
- Worker B: Tries to use GPU 0, OOM error
- Worker C: Competes for CPU cores, thrashing

**Solution:** Node Manager coordinates resource allocation across local workers

#### Environment Setup Overhead
**Problem:** ML training requires environment preparation:
1. Download 50GB dataset from S3
2. Extract and preprocess data
3. Run 1000 training tasks
4. Cleanup temporary files

**Current Workaround:** First task downloads dataset (wastes time), all tasks repeat setup

**Solution:** Task Suite with `env_preparation` hook runs once before all tasks

#### Campaign Management
**Problem:** User wants to run "October training campaign":
- Submit 5000 tasks with same configuration
- Monitor progress as a unit
- Cancel entire campaign if early tasks fail
- Know when all tasks complete

**Current Workaround:** Tag-based querying, manual tracking

**Solution:** Task Suite as first-class entity with unified lifecycle

### 3.2 Use Cases

#### Use Case 1: ML Training Campaign
```yaml
Suite: "ResNet50 Hyperparameter Sweep"
Environment Setup:
  - Download ImageNet dataset (50GB)
  - Extract and shard data
  - Warm up GPU
Tasks:
  - 1000 training runs with different hyperparameters
  - Each task: Train for 10 epochs, save checkpoint
Environment Cleanup:
  - Upload best checkpoint to S3
  - Delete temporary shards
  - Clear GPU memory
```

#### Use Case 2: CI/CD Test Suite
```yaml
Suite: "Pull Request #1234 - Test Pipeline"
Environment Setup:
  - Clone repository at commit SHA
  - Build Docker image
  - Start test database
Tasks:
  - Unit tests (100 tasks)
  - Integration tests (50 tasks)
  - E2E tests (20 tasks)
Environment Cleanup:
  - Stop database
  - Collect coverage reports
  - Clean Docker images
```

#### Use Case 3: Batch Data Processing
```yaml
Suite: "Q4 2024 Log Processing"
Environment Setup:
  - Download Q4 logs from S3 (1TB)
  - Create local index
Tasks:
  - Process 10,000 log files
  - Extract metrics, anomalies
Environment Cleanup:
  - Upload aggregated results
  - Delete processed logs
```

---

## 4. Core Concepts

### 4.1 Task Suite

A **Task Suite** (or **Suite** for brevity) is a first-class entity representing a collection of related tasks with shared lifecycle and execution environment.

#### Properties

```rust
// Auto-close timeout is fixed at 3 minutes
const AUTO_CLOSE_TIMEOUT: Duration = Duration::from_secs(180); // Fixed at 3 minutes

pub struct TaskSuite {
    pub uuid: Uuid,                        // Unique identifier
    pub name: Option<String>,              // Optional display name (non-unique)
    pub description: Option<String>,       // Optional description
    pub group_id: i64,                     // Parent group (for permissions)

    // Scheduling attributes
    pub tags: HashSet<String>,             // For manager matching
    pub labels: HashSet<String>,           // For querying
    pub priority: i32,                     // Suite scheduling priority

    // Worker allocation
    pub worker_schedule: WorkerSchedulePlan,

    // Lifecycle hooks (TaskSpec-like)
    pub env_preparation: Option<EnvHookSpec>,
    pub env_cleanup: Option<EnvHookSpec>,

    // State management
    pub state: TaskSuiteState,             // Open/Closed/Complete/Cancelled
    pub last_task_submitted_at: Option<OffsetDateTime>,
    pub total_tasks: i32,                  // Total tasks ever submitted to this suite
    pub pending_tasks: i32,                // Currently pending/active tasks

    // Metadata
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    pub completed_at: Option<OffsetDateTime>,
}

pub struct WorkerSchedulePlan {
    pub worker_count: u32,                 // How many workers to spawn
    pub cpu_binding: Option<CpuBinding>,   // CPU core allocation
    pub task_prefetch_count: u32,          // How many tasks to buffer
}

pub struct CpuBinding {
    pub cores: Vec<usize>,                 // CPU core IDs
    pub strategy: CpuBindingStrategy,
}

pub enum CpuBindingStrategy {
    RoundRobin,    // Distribute workers across cores
    Exclusive,     // Each worker gets dedicated core(s)
    Shared,        // All workers share all cores
}

pub struct EnvHookSpec {
    pub args: Vec<String>,                 // Command to execute
    pub envs: HashMap<String, String>,     // Environment variables
    pub resources: Vec<RemoteResourceDownload>, // S3 files to download
    pub timeout: Duration,
}
```

#### Lifecycle States

```rust
pub enum TaskSuiteState {
    Open = 0,      // Accepting new tasks, actively executing
    Closed = 1,    // No new tasks for timeout, but not complete
    Complete = 2,  // All tasks finished
    Cancelled = 3, // Explicitly cancelled by user
}
```

**State Transitions:**

```
  Create Suite → Open (on first task submission)

  Open → Closed (3-minute inactivity timeout, no new tasks)
  Open → Complete (all tasks finished)
  Open → Cancelled (user cancellation)

  Closed → Open (new task submitted)
  Closed → Complete (all pending tasks finished)
  Closed → Cancelled (user cancellation)

  Complete → Open (new task submitted, suite reopens)
  Complete → Cancelled (user cancellation)

  Cancelled → (terminal state)
```

**Key Behaviors:**
- Suites are **long-lived**: Can cycle through Open → Closed → Complete → Open multiple times
- Auto-close is **temporary**: Allows managers to switch away, but suite can reopen
- Completion is **not permanent**: New tasks can reopen completed suites

#### Context Variables for Hooks

Environment hooks execute with these context variables:
```bash
MITOSIS_TASK_SUITE_UUID=550e8400-e29b-41d4-a716-446655440000
MITOSIS_TASK_SUITE_NAME="ML Training Campaign"
MITOSIS_GROUP_NAME="ml-team"
MITOSIS_WORKER_COUNT=16
MITOSIS_NODE_MANAGER_ID=abcd1234-...
```

### 4.2 Node Manager

A **Node Manager** is a device-level service that:

1. **Manages Worker Pools**: Spawns and monitors local worker processes
2. **Controls Resources**: CPU core binding, memory limits, GPU allocation
3. **Orchestrates Environments**: Executes preparation/cleanup hooks
4. **Proxies Communication**: Relays worker ↔ coordinator messages via WebSocket
5. **Recovers from Failures**: Auto-respawns dead workers, tracks failures

#### Responsibilities

**Resource Management:**
- Prevents multiple managers on same device (machine-level mutex)
- Binds workers to specific CPU cores
- Enforces suite-defined resource limits

**Lifecycle Management:**
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

#### Properties

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

### 4.3 Worker Modes

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

**Key Insight:** Managed workers reuse existing worker code with communication abstraction:

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

---

## 5. Architecture Design

### 5.1 System Architecture

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
│                                                                   │
│  Database: PostgreSQL                                            │
└─────────────────────────────────────────────────────────────────┘
         │                    │                    │
         │ HTTP               │ WebSocket          │ HTTP
         ▼                    ▼                    ▼
┌─────────────────┐  ┌──────────────────────────────────────────┐
│ Independent     │  │          Node Manager                     │
│ Worker          │  │  ┌────────────────────────────────────┐  │
│                 │  │  │ Manager Process                    │  │
│ • Polls tasks   │  │  │ • WebSocket to coordinator        │  │
│ • HTTP to coord │  │  │ • Fetches task suites            │  │
└─────────────────┘  │  │ • Spawns managed workers         │  │
                     │  │ • Env preparation/cleanup        │  │
                     │  │ • IPC proxy to coordinator       │  │
                     │  │ • Auto-recovery on worker crash  │  │
                     │  └────────────────────────────────────┘  │
                     │             │ iceoryx2 IPC               │
                     │             ▼                             │
                     │  ┌────────────────────────────────────┐  │
                     │  │ Managed Worker 1 (Anonymous)      │  │
                     │  │ • IPC to manager                  │  │
                     │  │ • Task execution                  │  │
                     │  │ • Not registered with coordinator │  │
                     │  └────────────────────────────────────┘  │
                     │  ┌────────────────────────────────────┐  │
                     │  │ Managed Worker 2 (Anonymous)      │  │
                     │  └────────────────────────────────────┘  │
                     │  ...                                      │
                     └──────────────────────────────────────────┘
```

### 5.2 Component Interactions

#### Suite Creation and Assignment

```
User                Coordinator           Database              NodeManager
 │                      │                     │                      │
 │ POST /suites         │                     │                      │
 ├─────────────────────>│                     │                      │
 │                      │ INSERT task_suites  │                      │
 │                      ├────────────────────>│                      │
 │                      │<────────────────────┤                      │
 │ {suite_uuid}         │                     │                      │
 │<─────────────────────┤                     │                      │
 │                      │                     │                      │
 │ POST /suites/{uuid}/managers/refresh       │                      │
 ├─────────────────────>│                     │                      │
 │                      │ SELECT eligible mgrs│                      │
 │                      ├────────────────────>│                      │
 │                      │ INSERT suite_mgrs   │                      │
 │                      ├────────────────────>│                      │
 │                      │                     │                      │
 │                      │ WS: SuiteAvailable  │                      │
 │                      ├────────────────────────────────────────────>│
 │                      │                     │                      │
```

#### Task Execution Flow

```
ManagedWorker      NodeManager        Coordinator        Database
     │                  │                  │                │
     │ IPC: FetchTask   │                  │                │
     ├─────────────────>│                  │                │
     │                  │ WS: FetchTask    │                │
     │                  ├─────────────────>│                │
     │                  │                  │ SELECT task    │
     │                  │                  ├───────────────>│
     │                  │                  │ UPDATE assigned│
     │                  │                  ├───────────────>│
     │                  │ WS: TaskResp     │                │
     │                  │<─────────────────┤                │
     │ IPC: TaskResp    │                  │                │
     │<─────────────────┤                  │                │
     │                  │                  │                │
     │ [Execute Task]   │                  │                │
     │                  │                  │                │
     │ IPC: ReportTask  │                  │                │
     ├─────────────────>│                  │                │
     │                  │ WS: ReportTask   │                │
     │                  ├─────────────────>│                │
     │                  │                  │ UPDATE state   │
     │                  │                  ├───────────────>│
     │                  │                  │ MOVE to archive│
     │                  │                  ├───────────────>│
     │                  │ WS: Ack          │                │
     │                  │<─────────────────┤                │
     │ IPC: Ack         │                  │                │
     │<─────────────────┤                  │                │
```

---

## 6. Data Models

### 6.1 New Database Tables

#### `task_suites` Table

```sql
CREATE TABLE task_suites (
    id BIGSERIAL PRIMARY KEY,
    uuid UUID UNIQUE NOT NULL,

    -- Optional human-readable identifiers (non-unique)
    name TEXT,
    description TEXT,

    -- Permissions
    group_id BIGINT NOT NULL REFERENCES groups(id) ON DELETE RESTRICT,
    creator_id BIGINT NOT NULL REFERENCES users(id) ON DELETE RESTRICT,

    -- Scheduling attributes
    tags TEXT[] NOT NULL DEFAULT '{}',
    labels JSONB NOT NULL DEFAULT '[]',  -- array of strings for querying
    priority INTEGER NOT NULL DEFAULT 0,

    -- Worker allocation plan (JSON)
    worker_schedule JSONB NOT NULL,
    -- Example: {"worker_count": 16, "cpu_binding": {"cores": [0,1,2,3], "strategy": "RoundRobin"}, "task_prefetch_count": 32}

    -- Lifecycle hooks (TaskSpec-like, JSON)
    env_preparation JSONB,
    -- Example: {"args": ["./setup.sh"], "envs": {"DATA_DIR": "/mnt/data"}, "resources": [...], "timeout": "5m"}
    env_cleanup JSONB,
    -- Example: {"args": ["./cleanup.sh"], "envs": {}, "resources": [], "timeout": "2m"}

    -- State management
    state INTEGER NOT NULL DEFAULT 0,  -- Open=0, Closed=1, Complete=2, Cancelled=3
    last_task_submitted_at TIMESTAMPTZ,
    total_tasks INTEGER NOT NULL DEFAULT 0,  -- Total tasks ever submitted to this suite
    pending_tasks INTEGER NOT NULL DEFAULT 0,  -- Currently pending/active tasks

    -- Timestamps
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at TIMESTAMPTZ
);

-- Indices for efficient queries
CREATE INDEX idx_task_suites_group_id ON task_suites(group_id);
CREATE INDEX idx_task_suites_creator_id ON task_suites(creator_id);
CREATE INDEX idx_task_suites_state ON task_suites(state);
CREATE INDEX idx_task_suites_tags ON task_suites USING GIN(tags);
CREATE INDEX idx_task_suites_labels ON task_suites USING GIN(labels);

-- Index for auto-close timeout queries (fixed 3-minute timeout)
CREATE INDEX idx_task_suites_auto_close
    ON task_suites(last_task_submitted_at)
    WHERE state = 0;

-- Index for label-based queries
-- Supports queries like: labels @> '["ml-training"]'::jsonb
```

#### `node_managers` Table

```sql
CREATE TABLE node_managers (
    id BIGSERIAL PRIMARY KEY,
    uuid UUID UNIQUE NOT NULL,
    creator_id BIGINT NOT NULL REFERENCES users(id) ON DELETE RESTRICT,

    -- Capabilities
    tags TEXT[] NOT NULL DEFAULT '{}',
    labels JSONB NOT NULL DEFAULT '[]',  -- array of strings for querying

    -- State
    state INTEGER NOT NULL DEFAULT 0,  -- Idle=0, Preparing=1, Executing=2, Cleanup=3, Offline=4
    last_heartbeat TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    -- Current assignment
    assigned_task_suite_id BIGINT REFERENCES task_suites(id) ON DELETE SET NULL,
    lease_expires_at TIMESTAMPTZ,

    -- Metadata
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Indices
CREATE INDEX idx_node_managers_creator_id ON node_managers(creator_id);
CREATE INDEX idx_node_managers_state ON node_managers(state);
CREATE INDEX idx_node_managers_tags ON node_managers USING GIN(tags);
CREATE INDEX idx_node_managers_labels ON node_managers USING GIN(labels);
CREATE INDEX idx_node_managers_assigned_suite ON node_managers(assigned_task_suite_id)
    WHERE assigned_task_suite_id IS NOT NULL;
CREATE INDEX idx_node_managers_heartbeat ON node_managers(last_heartbeat);
```

#### `group_node_manager` Join Table

**Permission Model:** Groups have roles ON managers (same as `group_worker`)

```sql
CREATE TABLE group_node_manager (
    id BIGSERIAL PRIMARY KEY,
    group_id BIGINT NOT NULL REFERENCES groups(id) ON DELETE RESTRICT,
    manager_id BIGINT NOT NULL REFERENCES node_managers(id) ON DELETE CASCADE,
    role INTEGER NOT NULL,  -- 0=Read, 1=Write, 2=Admin (same as GroupWorkerRole)

    UNIQUE(group_id, manager_id)
);

CREATE INDEX idx_group_node_manager_group ON group_node_manager(group_id);
CREATE INDEX idx_group_node_manager_manager ON group_node_manager(manager_id);
```

**Role Definitions:**
- **Read (0)**: Reserved for future use (view manager status)
- **Write (1)**: Group can submit task suites to manager (required for execution)
- **Admin (2)**: Group can manage manager ACL and settings

#### `task_suite_managers` Join Table

Tracks which managers are assigned to which suites (user-specified or auto-matched).

```sql
CREATE TABLE task_suite_managers (
    id BIGSERIAL PRIMARY KEY,
    task_suite_id BIGINT NOT NULL REFERENCES task_suites(id) ON DELETE CASCADE,
    manager_id BIGINT NOT NULL REFERENCES node_managers(id) ON DELETE CASCADE,

    -- Selection method
    selection_type INTEGER NOT NULL,  -- 0=UserSpecified, 1=TagMatched

    -- For tag-matched, record which tags matched
    matched_tags TEXT[],

    -- Audit
    added_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    added_by_user_id BIGINT REFERENCES users(id),

    UNIQUE(task_suite_id, manager_id)
);

CREATE INDEX idx_task_suite_managers_suite ON task_suite_managers(task_suite_id);
CREATE INDEX idx_task_suite_managers_manager ON task_suite_managers(manager_id);
CREATE INDEX idx_task_suite_managers_selection_type ON task_suite_managers(selection_type);
```

#### `task_execution_failures` Table

Tracks task failures per manager to prevent infinite retry loops.

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
    worker_local_id INTEGER,  -- Which worker failed

    -- Metadata
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    UNIQUE(task_uuid, manager_id)
);

CREATE INDEX idx_task_failures_task_uuid ON task_execution_failures(task_uuid);
CREATE INDEX idx_task_failures_manager ON task_execution_failures(manager_id);
CREATE INDEX idx_task_failures_suite ON task_execution_failures(task_suite_id);
```

### 6.2 Update to `active_tasks` Table

```sql
ALTER TABLE active_tasks
ADD COLUMN task_suite_id BIGINT REFERENCES task_suites(id) ON DELETE SET NULL;

CREATE INDEX idx_active_tasks_suite ON active_tasks(task_suite_id)
    WHERE task_suite_id IS NOT NULL;
```

**Behavior:**
- If `task_suite_id` is NULL: Independent task (current behavior)
- If `task_suite_id` is set: Managed task (part of suite)

### 6.3 Database Triggers

#### Auto-Update Suite Task Counts

```sql
CREATE OR REPLACE FUNCTION update_suite_task_counts()
RETURNS TRIGGER AS $$
BEGIN
    IF TG_OP = 'INSERT' AND NEW.task_suite_id IS NOT NULL THEN
        -- New task added to suite
        UPDATE task_suites
        SET total_tasks = total_tasks + 1,
            pending_tasks = pending_tasks + 1,
            last_task_submitted_at = NOW(),
            updated_at = NOW()
        WHERE id = NEW.task_suite_id;

    ELSIF TG_OP = 'UPDATE' AND NEW.task_suite_id IS NOT NULL THEN
        IF OLD.state != NEW.state AND NEW.state IN (3, 4) THEN
            -- Task finished (Finished=3) or cancelled (Cancelled=4)
            UPDATE task_suites
            SET pending_tasks = GREATEST(pending_tasks - 1, 0),
                updated_at = NOW()
            WHERE id = NEW.task_suite_id;
        END IF;

    ELSIF TG_OP = 'DELETE' AND OLD.task_suite_id IS NOT NULL THEN
        -- Task deleted (rare)
        UPDATE task_suites
        SET pending_tasks = GREATEST(pending_tasks - 1, 0),
            updated_at = NOW()
        WHERE id = OLD.task_suite_id;
    END IF;

    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER task_suite_count_trigger
AFTER INSERT OR UPDATE OR DELETE ON active_tasks
FOR EACH ROW
EXECUTE FUNCTION update_suite_task_counts();
```

#### Auto-Transition Suite States

```sql
CREATE OR REPLACE FUNCTION auto_transition_suite_states()
RETURNS void AS $$
BEGIN
    -- Transition OPEN → CLOSED (3-minute inactivity timeout)
    UPDATE task_suites
    SET state = 1,  -- Closed
        updated_at = NOW()
    WHERE state = 0  -- Open
      AND last_task_submitted_at IS NOT NULL
      AND EXTRACT(EPOCH FROM (NOW() - last_task_submitted_at)) > 180  -- Fixed 3-minute timeout
      AND pending_tasks > 0;

    -- Transition OPEN/CLOSED → COMPLETE (all tasks finished)
    UPDATE task_suites
    SET state = 2,  -- Complete
        updated_at = NOW(),
        completed_at = NOW()
    WHERE state IN (0, 1)  -- Open or Closed
      AND pending_tasks = 0;
END;
$$ LANGUAGE plpgsql;

-- Schedule via pg_cron or background coordinator task (every 30 seconds)
```

---

## 7. API Design

### 7.1 Suite Management APIs

#### Create Task Suite

```http
POST /suites
Authorization: Bearer <user-jwt>
Content-Type: application/json

{
  "name": "ML Training Campaign",
  "description": "ResNet50 hyperparameter sweep",
  "group_name": "ml-team",
  "tags": ["gpu", "linux", "cuda:11.8"],
  "labels": ["project:resnet", "phase:training"],
  "priority": 10,
  "worker_schedule": {
    "worker_count": 16,
    "cpu_binding": {
      "cores": [0, 1, 2, 3],
      "strategy": "RoundRobin"
    },
    "task_prefetch_count": 32
  },
  "env_preparation": {
    "args": ["./setup.sh", "--download-dataset"],
    "envs": {"DATA_DIR": "/mnt/data"},
    "resources": [
      {"remote_file": {"Attachment": {"key": "setup.sh"}}, "local_path": "setup.sh"}
    ],
    "timeout": "5m"
  },
  "env_cleanup": {
    "args": ["./cleanup.sh"],
    "envs": {},
    "resources": [],
    "timeout": "2m"
  }
}

Response 201 Created:
{
  "uuid": "550e8400-e29b-41d4-a716-446655440000",
  "state": "Open",
  "assigned_managers": []
}
```

#### Query Suites

```http
GET /suites?group_name=ml-team&labels=project:resnet&state=Open
Authorization: Bearer <user-jwt>

Response 200 OK:
{
  "count": 5,
  "suites": [
    {
      "uuid": "550e8400-...",
      "name": "ML Training Campaign",
      "group_name": "ml-team",
      "tags": ["gpu", "linux", "cuda:11.8"],
      "labels": ["project:resnet", "phase:training"],
      "state": "Open",
      "priority": 10,
      "total_tasks": 500,
      "pending_tasks": 127,
      "created_at": "2025-11-11T10:00:00Z",
      "updated_at": "2025-11-11T10:30:00Z"
    },
    ...
  ]
}
```

#### Get Suite Details

```http
GET /suites/{uuid}
Authorization: Bearer <user-jwt>

Response 200 OK:
{
  "uuid": "550e8400-...",
  "name": "ML Training Campaign",
  "description": "ResNet50 hyperparameter sweep",
  "group_name": "ml-team",
  "creator_username": "alice",
  "tags": ["gpu", "linux", "cuda:11.8"],
  "labels": ["project:resnet"],
  "priority": 10,
  "worker_schedule": { ... },
  "env_preparation": { ... },
  "env_cleanup": { ... },
  "state": "Open",
  "last_task_submitted_at": "2025-11-11T10:29:00Z",
  "total_tasks": 500,
  "pending_tasks": 127,
  "created_at": "2025-11-11T10:00:00Z",
  "updated_at": "2025-11-11T10:30:00Z",
  "completed_at": null,
  "assigned_managers": ["mgr-uuid-1", "mgr-uuid-2"]
}
```

#### Cancel Suite

```http
POST /suites/{uuid}/cancel
Authorization: Bearer <user-jwt>
Content-Type: application/json

{
  "reason": "User requested cancellation",
  "cancel_running_tasks": true
}

Response 200 OK:
{
  "cancelled_task_count": 45,
  "suite_state": "Cancelled"
}
```

### 7.2 Suite Manager Assignment APIs

#### Refresh Tag-Matched Managers

```http
POST /suites/{uuid}/managers/refresh
Authorization: Bearer <user-jwt>

Response 200 OK:
{
  "added_managers": [
    {
      "manager_uuid": "mgr-1",
      "matched_tags": ["gpu", "linux", "cuda:11.8"],
      "selection_type": "TagMatched"
    },
    {
      "manager_uuid": "mgr-2",
      "matched_tags": ["gpu", "linux", "cuda:11.8"],
      "selection_type": "TagMatched"
    }
  ],
  "removed_managers": ["mgr-old"],
  "total_assigned": 2
}
```

**Behavior:**
1. Remove all existing tag-matched managers
2. Find eligible managers: `manager.tags ⊇ suite.tags` AND `suite.group_id` has write role on manager
3. Insert new tag-matched managers

#### Add Managers Explicitly

```http
POST /suites/{uuid}/managers
Authorization: Bearer <user-jwt>
Content-Type: application/json

{
  "manager_uuids": ["mgr-uuid-1", "mgr-uuid-2"]
}

Response 200 OK:
{
  "added_managers": ["mgr-uuid-1", "mgr-uuid-2"],
  "rejected_managers": [],
  "reason": null
}

Response 403 Forbidden (if no permission):
{
  "added_managers": [],
  "rejected_managers": ["mgr-uuid-1"],
  "reason": "Group 'ml-team' does not have Write role on manager 'mgr-uuid-1'"
}
```

#### Remove Managers

```http
DELETE /suites/{uuid}/managers
Authorization: Bearer <user-jwt>
Content-Type: application/json

{
  "manager_uuids": ["mgr-uuid-1"]
}

Response 200 OK:
{
  "removed_count": 1
}
```

### 7.3 Task Submission API Update

```http
POST /tasks
Authorization: Bearer <user-jwt>
Content-Type: application/json

{
  "group_name": "ml-team",
  "suite_uuid": "550e8400-...",  // NEW: Optional suite UUID
  "tags": ["gpu"],
  "labels": ["experiment:run-42"],
  "timeout": "10m",
  "priority": 5,
  "task_spec": {
    "args": ["python", "train.py", "--lr=0.01"],
    "envs": {"CUDA_VISIBLE_DEVICES": "0"},
    "resources": [],
    "terminal_output": false,
    "watch": null
  }
}

Response 201 Created:
{
  "task_id": 12345,
  "uuid": "task-uuid-...",
  "suite_uuid": "550e8400-..."  // Echoed back if provided
}
```

**Behavior:**
- If `suite_uuid` provided: Task assigned to suite, `active_tasks.task_suite_id` set
- If `suite_uuid` omitted: Independent task (current behavior)

### 7.4 Node Manager APIs

#### Register Manager

```http
POST /managers
Content-Type: application/json

{
  "tags": ["gpu", "linux", "x86_64", "cuda:11.8"],
  "labels": ["datacenter:us-west", "machine_id:server-42"],
  "groups": ["ml-team", "ci-team"],
  "lifetime": "30d"  // Optional token expiry (default: 30 days)
}

Response 201 Created:
{
  "manager_uuid": "mgr-550e8400-...",
  "token": "eyJ0eXAiOiJKV1QiLCJhbGc...",
  "websocket_url": "wss://coordinator.example.com/ws/managers"
}
```

#### Manager Heartbeat

```http
POST /managers/heartbeat
Authorization: Bearer <manager-jwt>
Content-Type: application/json

{
  "state": "Executing",
  "assigned_suite_uuid": "suite-550e8400-...",
  "metrics": {
    "active_workers": 16,
    "tasks_completed": 127,
    "tasks_failed": 3
  }
}

Response 204 No Content
```

#### Query Managers

```http
GET /managers?group_name=ml-team&tags=gpu,linux&state=Idle
Authorization: Bearer <user-jwt>

Response 200 OK:
{
  "count": 3,
  "managers": [
    {
      "uuid": "mgr-uuid-1",
      "creator_username": "alice",
      "tags": ["gpu", "linux", "cuda:11.8"],
      "labels": ["datacenter:us-west"],
      "state": "Idle",
      "last_heartbeat": "2025-11-11T10:30:00Z",
      "assigned_suite_uuid": null,
      "created_at": "2025-11-10T08:00:00Z"
    },
    ...
  ]
}
```

#### Shutdown Manager

```http
POST /managers/{uuid}/shutdown
Authorization: Bearer <user-jwt>
Content-Type: application/json

{
  "op": "Graceful"  // or "Force"
}

Response 200 OK:
{
  "state": "ShuttingDown"
}
```

---

## 8. Communication Protocols

### 8.1 WebSocket Protocol (Manager ↔ Coordinator)

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

#### Message Format

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

#### Request-Response Multiplexing

Managers send multiple concurrent requests over single WebSocket:

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

### 8.2 IPC Protocol (Manager ↔ Managed Workers)

#### iceoryx2 Request-Response Services

```rust
// Service: task_fetch
#[derive(Serialize, Deserialize)]
struct FetchTaskRequest {
    worker_local_id: u32,
    request_id: u64,
}

#[derive(Serialize, Deserialize)]
struct FetchTaskResponse {
    task: Option<WorkerTaskResp>,
}

// Service: task_report
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

// Service: heartbeat (optional, manager tracks process health)
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

```rust
// Topic: manager/control
#[derive(Serialize, Deserialize)]
enum ControlMessage {
    Shutdown { graceful: bool },
    CancelTask { task_id: i64, graceful: bool },
    Pause,
    Resume,
}

// Topic: manager/config
#[derive(Serialize, Deserialize)]
struct ConfigUpdate {
    poll_interval: Option<Duration>,
}
```

#### IPC Setup (Manager Side)

```rust
impl NodeManager {
    async fn setup_ipc_services(&mut self) -> Result<()> {
        let service_name_prefix = format!("mitosis_mgr_{}", self.uuid);

        // Request-Response server
        self.task_fetch_service = ServiceBuilder::new(&format!("{}_task_fetch", service_name_prefix))
            .request_response::<FetchTaskRequest, FetchTaskResponse>()
            .create_server()?;

        self.task_report_service = ServiceBuilder::new(&format!("{}_task_report", service_name_prefix))
            .request_response::<ReportTaskRequest, ReportTaskResponse>()
            .create_server()?;

        // Pub-Sub publisher
        self.control_publisher = ServiceBuilder::new(&format!("{}_control", service_name_prefix))
            .publish_subscribe::<ControlMessage>()
            .create_publisher()?;

        Ok(())
    }

    async fn handle_worker_fetch_task(&mut self, req: FetchTaskRequest) -> Result<FetchTaskResponse> {
        // Check local prefetch buffer first
        if let Some(task) = self.local_task_queue.pop_front() {
            return Ok(FetchTaskResponse { task: Some(task) });
        }

        // Fetch from coordinator via WebSocket
        let task = self.coordinator_client.fetch_task(req.worker_local_id).await?;

        // Async refill buffer
        tokio::spawn({
            let manager = self.clone();
            async move {
                manager.maintain_task_buffer().await;
            }
        });

        Ok(FetchTaskResponse { task })
    }
}
```

#### IPC Setup (Worker Side)

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

---

## 9. State Machines and Workflows

### 9.1 Task Suite State Machine

```
                    ┌─────────────────────┐
                    │                     │
           submit   │    new task         │  submit new task
           first ───┼───> submitted       │  (reopens)
           task     │                     │
                    │                     │
                    ▼                     │
               ┌─────────┐                │
          ┌───>│  OPEN   │<───────────────┘
          │    └────┬────┘
          │         │
          │         │ 3-minute inactivity
          │         │ (no new tasks)
          │         │
          │         ▼
          │    ┌─────────┐
          │    │ CLOSED  │
          │    └────┬────┘
          │         │
          │         │ all tasks finished
          │         │
          │         ▼
          │    ┌──────────┐
          └────┤ COMPLETE │
               └────┬─────┘
                    │
                    │ user cancels
                    ▼
               ┌───────────┐
               │ CANCELLED │
               └───────────┘
                 (terminal)
```

**State Transition Rules:**

| From State | Event | To State | Action |
|------------|-------|----------|--------|
| (None) | First task submitted | Open | Set `last_task_submitted_at`, increment `total_tasks` and `pending_tasks` |
| Open | 3-minute inactivity elapsed | Closed | Background job detects timeout |
| Open | All tasks finished | Complete | Trigger sets `completed_at`, manager runs cleanup |
| Open | User cancels | Cancelled | Cancel all pending/running tasks, manager aborts |
| Closed | New task submitted | Open | Reset `last_task_submitted_at` |
| Closed | All tasks finished | Complete | Same as Open → Complete |
| Closed | User cancels | Cancelled | Same as Open → Cancelled |
| Complete | New task submitted | Open | Reopen suite, reset `completed_at` to NULL |
| Complete | User cancels | Cancelled | Mark as cancelled |
| Cancelled | (any) | Cancelled | Terminal state, no transitions |

### 9.2 Node Manager Lifecycle

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

### 9.3 Manager Execution Workflow (Detailed)

```
┌─ Manager Startup ─────────────────────────────────────────┐
│                                                             │
│  1. Load configuration                                      │
│  2. Register with coordinator (POST /managers)             │
│     - Receive JWT token, manager UUID                      │
│  3. Establish WebSocket connection                         │
│  4. Setup iceoryx2 IPC services                            │
│  5. Enter IDLE state                                       │
│                                                             │
└─────────────────────────────────────────────────────────────┘
                          │
                          ▼
┌─ Suite Assignment ────────────────────────────────────────┐
│                                                             │
│  Coordinator sends WS: SuiteAssigned                       │
│    {suite_uuid, suite_spec}                                │
│                                                             │
│  Manager transitions: IDLE → PREPARING                     │
│                                                             │
└─────────────────────────────────────────────────────────────┘
                          │
                          ▼
┌─ Environment Preparation ─────────────────────────────────┐
│                                                             │
│  1. Download resources from S3                             │
│     - suite.env_preparation.resources[]                    │
│                                                             │
│  2. Execute preparation command                            │
│     - Args: suite.env_preparation.args                     │
│     - Envs: suite.env_preparation.envs + context vars      │
│     - Timeout: suite.env_preparation.timeout               │
│                                                             │
│  3. Check exit status                                      │
│     - Exit 0: Success → Continue                           │
│     - Non-zero: Failure → Abort suite, back to IDLE        │
│                                                             │
└─────────────────────────────────────────────────────────────┘
                          │
                          ▼
┌─ Worker Spawning ─────────────────────────────────────────┐
│                                                             │
│  1. Spawn N workers (suite.worker_schedule.worker_count)   │
│     - Launch as subprocess: `mitosis worker --managed ...` │
│     - Pass IPC config: manager_uuid, worker_local_id       │
│                                                             │
│  2. Apply CPU binding (if specified)                       │
│     - RoundRobin: workers cycle through cores              │
│     - Exclusive: each worker gets dedicated cores          │
│     - Shared: all workers share all cores                  │
│                                                             │
│  3. Wait for workers to connect via IPC                    │
│                                                             │
│  4. Transition: PREPARING → EXECUTING                      │
│                                                             │
└─────────────────────────────────────────────────────────────┘
                          │
                          ▼
┌─ Task Execution Loop ─────────────────────────────────────┐
│                                                             │
│  Manager runs concurrently:                                │
│                                                             │
│  Thread 1: IPC Server (handle worker requests)             │
│    - Worker → IPC: FetchTaskRequest                        │
│    - Manager checks local buffer                           │
│    - If empty: Manager → WS: FetchTask                     │
│    - Coordinator → WS: TaskAvailable                       │
│    - Manager → IPC: FetchTaskResponse                      │
│    - Worker executes task                                  │
│    - Worker → IPC: ReportTaskRequest                       │
│    - Manager → WS: ReportTask                              │
│    - Coordinator → WS: TaskReportAck                       │
│    - Manager → IPC: ReportTaskResponse                     │
│                                                             │
│  Thread 2: Worker Health Monitor                           │
│    - Poll process status every 1s                          │
│    - If worker exits:                                      │
│        - Record failure (task_execution_failures)          │
│        - If failure_count >= 3: Abort task, notify coord   │
│        - Else: Respawn worker, retry task                  │
│                                                             │
│  Thread 3: Task Prefetch Buffer Maintenance                │
│    - Keep local buffer filled (prefetch_count tasks)       │
│    - Async fetch from coordinator when < threshold         │
│                                                             │
│  Thread 4: Suite State Monitor                             │
│    - Check if suite.state == Closed AND no pending tasks   │
│    - If true: Transition to CLEANUP                        │
│                                                             │
└─────────────────────────────────────────────────────────────┘
                          │
                          ▼
┌─ Completion Detection ────────────────────────────────────┐
│                                                             │
│  Trigger: Coordinator sends WS: SuiteCompleted             │
│    OR Manager detects: no more tasks, all workers idle     │
│                                                             │
│  Manager transitions: EXECUTING → CLEANUP                  │
│                                                             │
└─────────────────────────────────────────────────────────────┘
                          │
                          ▼
┌─ Environment Cleanup ─────────────────────────────────────┐
│                                                             │
│  1. Send graceful shutdown to all workers                  │
│     - Publish IPC: ControlMessage::Shutdown{graceful:true} │
│     - Wait up to 30s for workers to finish current tasks   │
│                                                             │
│  2. Force kill any remaining workers                       │
│                                                             │
│  3. Execute cleanup command                                │
│     - Args: suite.env_cleanup.args                         │
│     - Envs: suite.env_cleanup.envs + context vars          │
│     - Timeout: suite.env_cleanup.timeout                   │
│                                                             │
│  4. Report suite completion to coordinator                 │
│     - WS: SuiteCompleted {suite_uuid, stats}               │
│                                                             │
│  5. Transition: CLEANUP → IDLE                             │
│                                                             │
└─────────────────────────────────────────────────────────────┘
                          │
                          ▼
                    (Back to IDLE)
                          │
                          ▼
                  Fetch next suite...
```

### 9.4 Task Execution Flow (Managed vs Independent)

#### Independent Worker (Current - Unchanged)

```
Worker                           Coordinator                 Database
  │                                  │                          │
  │ POST /workers (register)         │                          │
  ├─────────────────────────────────>│                          │
  │ {tags, labels, groups}           │ INSERT workers           │
  │                                  ├─────────────────────────>│
  │                                  │ INSERT group_worker      │
  │                                  ├─────────────────────────>│
  │ {worker_id, token}               │                          │
  │<─────────────────────────────────┤                          │
  │                                  │                          │
  │ [Poll loop every 5s]             │                          │
  │ GET /workers/tasks               │                          │
  ├─────────────────────────────────>│                          │
  │                                  │ SELECT eligible task     │
  │                                  ├─────────────────────────>│
  │                                  │ UPDATE assigned_worker   │
  │                                  ├─────────────────────────>│
  │ {task}                           │                          │
  │<─────────────────────────────────┤                          │
  │                                  │                          │
  │ [Execute task]                   │                          │
  │                                  │                          │
  │ POST /workers/tasks (Finish)     │                          │
  ├─────────────────────────────────>│                          │
  │                                  │ UPDATE state=Finished    │
  │                                  ├─────────────────────────>│
  │ OK                               │                          │
  │<─────────────────────────────────┤                          │
  │                                  │                          │
  │ POST /workers/tasks (Commit)     │                          │
  ├─────────────────────────────────>│                          │
  │                                  │ MOVE to archived_tasks   │
  │                                  ├─────────────────────────>│
  │ OK                               │                          │
  │<─────────────────────────────────┤                          │
```

#### Managed Worker (New)

```
ManagedWorker    NodeManager            Coordinator          Database
     │               │                       │                  │
     │ [Spawned by manager]                  │                  │
     │               │                       │                  │
     │ IPC: FetchTask│                       │                  │
     ├──────────────>│                       │                  │
     │               │ [Check local buffer]  │                  │
     │               │ [Empty, fetch from coord]                │
     │               │ WS: FetchTask         │                  │
     │               ├──────────────────────>│                  │
     │               │                       │ SELECT task      │
     │               │                       ├─────────────────>│
     │               │                       │ UPDATE assigned  │
     │               │                       ├─────────────────>│
     │               │ WS: TaskAvailable     │                  │
     │               │<──────────────────────┤                  │
     │ IPC: TaskResp │                       │                  │
     │<──────────────┤                       │                  │
     │               │                       │                  │
     │ [Execute]     │                       │                  │
     │               │                       │                  │
     │ IPC: Report   │                       │                  │
     ├──────────────>│                       │                  │
     │               │ WS: ReportTask        │                  │
     │               ├──────────────────────>│                  │
     │               │                       │ UPDATE state     │
     │               │                       ├─────────────────>│
     │               │                       │ MOVE to archive  │
     │               │                       ├─────────────────>│
     │               │ WS: Ack               │                  │
     │               │<──────────────────────┤                  │
     │ IPC: Ack      │                       │                  │
     │<──────────────┤                       │                  │
```

---

## 10. Error Handling and Fault Tolerance

### 10.1 Error Taxonomy

```rust
#[derive(Debug, thiserror::Error)]
pub enum ManagerError {
    // Network errors (retriable)
    #[error("WebSocket connection error: {0}")]
    WebSocketConnection(#[from] tokio_tungstenite::tungstenite::Error),

    #[error("HTTP request error: {0}")]
    HttpRequest(#[from] reqwest::Error),

    // Database errors (retriable)
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),

    // S3 errors (retriable)
    #[error("S3 operation error: {0}")]
    S3(#[from] aws_sdk_s3::Error),

    // IPC errors (retriable)
    #[error("IPC error: {0}")]
    Ipc(String),

    // Environment errors (non-retriable, abort suite)
    #[error("Environment preparation failed: {0}")]
    EnvPreparation(String),

    #[error("Environment cleanup failed: {0}")]
    EnvCleanup(String),

    // Worker errors (conditionally retriable)
    #[error("Worker spawn failed: {0}")]
    WorkerSpawn(std::io::Error),

    #[error("Worker died unexpectedly: {0}")]
    WorkerDeath(String),

    // Permission errors (non-retriable)
    #[error("Authentication failed")]
    Authentication,

    #[error("Permission denied: {0}")]
    PermissionDenied(String),

    // Task errors
    #[error("Task execution failed after {0} attempts")]
    TaskExecutionFailed(u32),

    #[error("Task timeout after {0:?}")]
    TaskTimeout(Duration),
}

impl ManagerError {
    pub fn is_retriable(&self) -> bool {
        matches!(
            self,
            ManagerError::WebSocketConnection(_)
                | ManagerError::HttpRequest(_)
                | ManagerError::Database(_)
                | ManagerError::S3(_)
                | ManagerError::Ipc(_)
        )
    }

    pub fn should_abort_suite(&self) -> bool {
        matches!(
            self,
            ManagerError::EnvPreparation(_)
                | ManagerError::EnvCleanup(_)
                | ManagerError::PermissionDenied(_)
        )
    }
}
```

### 10.2 Worker Death Handling

#### Exit Status Analysis

```rust
use nix::sys::wait::{waitpid, WaitStatus};
use nix::sys::signal::Signal;

#[derive(Debug, Clone)]
enum WorkerExitReason {
    Success,
    ExitCode(i32),
    Signal(Signal),
    Unknown,
}

impl WorkerExitReason {
    fn should_abort(&self, failure_count: u32) -> bool {
        match self {
            // Hardware issues - abort faster
            WorkerExitReason::Signal(Signal::SIGSEGV) |  // Segmentation fault
            WorkerExitReason::Signal(Signal::SIGILL) |   // Illegal instruction
            WorkerExitReason::Signal(Signal::SIGBUS) |   // Bus error
            WorkerExitReason::Signal(Signal::SIGFPE) => { // FP exception
                failure_count >= 2
            }

            // Resource issues - abort after 3 attempts
            WorkerExitReason::Signal(Signal::SIGKILL) |  // OOM killer
            WorkerExitReason::Signal(Signal::SIGABRT) => {
                failure_count >= 3
            }

            // Exit codes - retry 3 times
            WorkerExitReason::ExitCode(_) => {
                failure_count >= 3
            }

            // Graceful termination - never abort
            WorkerExitReason::Success |
            WorkerExitReason::Signal(Signal::SIGTERM) |
            WorkerExitReason::Signal(Signal::SIGINT) => false,

            _ => failure_count >= 3,
        }
    }

    fn error_message(&self) -> String {
        match self {
            WorkerExitReason::Success => "Worker exited successfully".into(),
            WorkerExitReason::ExitCode(code) => format!("Exit code {}", code),
            WorkerExitReason::Signal(sig) => format!("Signal: {:?}", sig),
            WorkerExitReason::Unknown => "Unknown exit reason".into(),
        }
    }
}
```

#### Failure Tracking and Retry Logic

```rust
impl NodeManager {
    async fn handle_worker_exit(
        &mut self,
        worker: &WorkerProcess,
        reason: WorkerExitReason,
    ) -> Result<()> {
        if let Some(task_uuid) = worker.current_task_uuid {
            // Record failure in database
            let failure_count = self.record_task_failure(
                task_uuid,
                worker.local_id,
                &reason.error_message(),
            ).await?;

            // Decide: abort or retry?
            if reason.should_abort(failure_count) {
                tracing::warn!(
                    task_uuid = ?task_uuid,
                    worker_id = worker.local_id,
                    reason = ?reason,
                    failure_count = failure_count,
                    "Aborting task after repeated failures"
                );

                // Return task to coordinator (abort)
                self.coordinator_client.abort_task(
                    task_uuid,
                    format!("Worker failed {} times: {}", failure_count, reason.error_message()),
                ).await?;

                // Clean up failure tracking
                self.failure_tracker.remove(&task_uuid);
            } else {
                tracing::info!(
                    task_uuid = ?task_uuid,
                    worker_id = worker.local_id,
                    reason = ?reason,
                    failure_count = failure_count,
                    "Retrying task after worker failure"
                );

                // Respawn worker, task will retry
                self.respawn_worker(worker.local_id).await?;
            }
        } else {
            // Worker died while idle, just respawn
            self.respawn_worker(worker.local_id).await?;
        }

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
            .or_insert_with(|| TaskFailureInfo::default());

        info.count += 1;
        info.last_failure_at = OffsetDateTime::now_utc();
        info.error_messages.push(error_message.to_string());
        info.failed_worker_ids.push(worker_local_id);

        // Persist to database via coordinator
        self.coordinator_client.report_task_failure(
            task_uuid,
            self.uuid,
            info.count,
            error_message,
            worker_local_id,
        ).await?;

        Ok(info.count)
    }
}
```

#### Coordinator-Side Failure Tracking

```rust
impl Coordinator {
    async fn record_task_failure(
        &self,
        task_uuid: Uuid,
        manager_uuid: Uuid,
        failure_count: u32,
        error_message: &str,
        worker_local_id: u32,
    ) -> Result<()> {
        // Get task and manager IDs
        let task = self.db.get_task_by_uuid(task_uuid).await?;
        let manager = self.db.get_manager_by_uuid(manager_uuid).await?;

        // Upsert failure record
        sqlx::query!(
            "INSERT INTO task_execution_failures
             (task_id, task_uuid, task_suite_id, manager_id, failure_count,
              last_failure_at, error_messages, worker_local_id)
             VALUES ($1, $2, $3, $4, $5, NOW(), ARRAY[$6], $7)
             ON CONFLICT (task_uuid, manager_id)
             DO UPDATE SET
                 failure_count = $5,
                 last_failure_at = NOW(),
                 error_messages = task_execution_failures.error_messages || $6,
                 updated_at = NOW()",
            task.id,
            task_uuid,
            task.task_suite_id,
            manager.id,
            failure_count as i32,
            error_message,
            worker_local_id as i32,
        )
        .execute(&self.db)
        .await?;

        Ok(())
    }

    async fn handle_abort_task(
        &self,
        task_uuid: Uuid,
        manager_uuid: Uuid,
        reason: &str,
    ) -> Result<()> {
        // Return task to queue, exclude this manager
        let task = self.db.get_task_by_uuid(task_uuid).await?;
        let manager = self.db.get_manager_by_uuid(manager_uuid).await?;

        // Unassign task
        sqlx::query!(
            "UPDATE active_tasks
             SET assigned_worker = NULL,
                 state = 1,  -- Ready
                 updated_at = NOW()
             WHERE uuid = $1",
            task_uuid
        )
        .execute(&self.db)
        .await?;

        tracing::info!(
            task_uuid = ?task_uuid,
            manager_uuid = ?manager_uuid,
            reason = reason,
            "Task aborted by manager, returned to queue"
        );

        // Task will be reassigned to different manager
        // (coordinator checks task_execution_failures to exclude failed managers)

        Ok(())
    }
}
```

### 10.3 Manager Disconnection Handling

#### Heartbeat Timeout Detection

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
                 SET state = 4,  -- Offline
                     updated_at = NOW()
                 WHERE id = $1",
                manager.id
            )
            .execute(&self.db)
            .await?;

            // Reclaim assigned suite
            if let Some(suite_id) = manager.assigned_task_suite_id {
                self.reclaim_suite_from_manager(suite_id, manager.id).await?;
            }

            // Close WebSocket connection
            self.ws_manager.close_connection(manager.uuid).await?;
        }
    }

    async fn reclaim_suite_from_manager(
        &self,
        suite_id: i64,
        manager_id: i64,
    ) -> Result<()> {
        // Get all non-committed tasks prefetched by this manager
        let prefetched_tasks = sqlx::query!(
            "SELECT uuid FROM active_tasks
             WHERE task_suite_id = $1
               AND state = 2  -- Running
               AND assigned_worker = $2",  // Actually assigned_manager in this context
            suite_id,
            manager_id
        )
        .fetch_all(&self.db)
        .await?;

        // Return tasks to queue
        for task in prefetched_tasks {
            sqlx::query!(
                "UPDATE active_tasks
                 SET state = 1,  -- Ready
                     assigned_worker = NULL,
                     updated_at = NOW()
                 WHERE uuid = $1",
                task.uuid
            )
            .execute(&self.db)
            .await?;

            tracing::info!(
                task_uuid = ?task.uuid,
                manager_id = manager_id,
                "Reclaimed task from disconnected manager"
            );
        }

        // Reopen suite if it was closed
        sqlx::query!(
            "UPDATE task_suites
             SET state = 0,  -- Open
                 updated_at = NOW()
             WHERE id = $1
               AND state = 1",  -- Closed
            suite_id
        )
        .execute(&self.db)
        .await?;

        tracing::info!(
            suite_id = suite_id,
            manager_id = manager_id,
            tasks_reclaimed = prefetched_tasks.len(),
            "Reclaimed suite from disconnected manager"
        );

        Ok(())
    }
}
```

#### Manager Reconnection

```rust
impl NodeManager {
    async fn handle_reconnection(&mut self) -> Result<()> {
        tracing::info!("Reconnected to coordinator, resuming state");

        // Query coordinator for current assignment
        let assignment = self.coordinator_client
            .get_manager_assignment(self.uuid)
            .await?;

        match assignment {
            Some(suite) if suite.uuid == self.assigned_suite_uuid => {
                tracing::info!(
                    suite_uuid = ?suite.uuid,
                    "Resuming execution of assigned suite"
                );

                // Continue execution
                // Workers are still running, just reconnect proxy
            }

            Some(suite) => {
                tracing::warn!(
                    old_suite = ?self.assigned_suite_uuid,
                    new_suite = ?suite.uuid,
                    "Coordinator assigned different suite during disconnect"
                );

                // Abort current suite, start new one
                self.abort_current_suite().await?;
                self.start_suite(suite).await?;
            }

            None if self.assigned_suite_uuid.is_some() => {
                tracing::warn!(
                    suite_uuid = ?self.assigned_suite_uuid,
                    "Suite was reclaimed during disconnect, aborting local execution"
                );

                self.abort_current_suite().await?;
            }

            None => {
                tracing::info!("No suite assigned, entering idle state");
                self.state = NodeManagerState::Idle;
            }
        }

        Ok(())
    }
}
```

### 10.4 Idempotent Task Commit

**Same logic as current workers** - first to commit wins:

```rust
impl Coordinator {
    async fn commit_task_result(
        &self,
        task_uuid: Uuid,
        result: TaskResultSpec,
    ) -> Result<bool> {
        // Idempotent: only commit if state != Finished
        let updated = sqlx::query!(
            "UPDATE active_tasks
             SET state = 3,  -- Finished
                 result = $1,
                 updated_at = NOW()
             WHERE uuid = $2
               AND state != 3
             RETURNING id",
            serde_json::to_value(&result)?,
            task_uuid
        )
        .fetch_optional(&self.db)
        .await?;

        if updated.is_none() {
            tracing::info!(
                task_uuid = ?task_uuid,
                "Task already committed by another worker/manager (idempotent)"
            );
            return Ok(false);
        }

        // Move to archived_tasks
        self.archive_task(task_uuid).await?;

        // Clean up failure tracking
        sqlx::query!(
            "DELETE FROM task_execution_failures
             WHERE task_uuid = $1",
            task_uuid
        )
        .execute(&self.db)
        .await?;

        Ok(true)
    }
}
```

---

## 11. Security and Permissions

### 11.1 Permission Model

**Consistent with existing `group_worker` model:**

```
Groups have roles ON managers (via group_node_manager table):

Task Suite → belongs to → Group A
Group A → has Write role on → Manager M1
Therefore: Suite can execute on M1
```

#### Permission Check for Suite Assignment

```rust
impl Coordinator {
    async fn is_manager_eligible_for_suite(
        &self,
        manager_id: i64,
        suite: &TaskSuite,
    ) -> Result<bool> {
        // 1. Tag matching: suite.tags ⊆ manager.tags (set containment)
        let manager = self.db.get_manager(manager_id).await?;

        let suite_tags: HashSet<_> = suite.tags.iter().cloned().collect();
        let manager_tags: HashSet<_> = manager.tags.iter().cloned().collect();

        if !suite_tags.is_subset(&manager_tags) {
            return Ok(false);
        }

        // 2. Permission check: suite.group_id must have Write role on manager
        let has_permission = sqlx::query_scalar!(
            "SELECT EXISTS(
                 SELECT 1 FROM group_node_manager
                 WHERE group_id = $1
                   AND manager_id = $2
                   AND role >= 1  -- Write or Admin
             )",
            suite.group_id,
            manager_id
        )
        .fetch_one(&self.db)
        .await?
        .unwrap_or(false);

        if !has_permission {
            return Ok(false);
        }

        // 3. Availability: not executing another suite (or lease expired)
        if let Some(assigned_suite_id) = manager.assigned_task_suite_id {
            if assigned_suite_id != suite.id {
                // Check lease expiry
                if let Some(lease_expires_at) = manager.lease_expires_at {
                    if lease_expires_at > OffsetDateTime::now_utc() {
                        return Ok(false);  // Still leased to other suite
                    }
                } else {
                    return Ok(false);  // Assigned but no lease (shouldn't happen)
                }
            }
        }

        Ok(true)
    }
}
```

### 11.2 JWT Token Management

#### Token Expiry and Refresh

**Default token lifetime: 30 days (configurable)**

```rust
impl Coordinator {
    fn create_manager_token(
        &self,
        manager_uuid: Uuid,
        lifetime: Option<Duration>,
    ) -> Result<String> {
        let lifetime = lifetime.unwrap_or(Duration::from_secs(30 * 24 * 3600));  // 30 days

        let claims = Claims {
            sub: manager_uuid.to_string(),
            exp: (OffsetDateTime::now_utc() + lifetime).unix_timestamp(),
            iat: OffsetDateTime::now_utc().unix_timestamp(),
            manager: true,
        };

        let token = encode(&Header::new(Algorithm::EdDSA), &claims, &self.signing_key)?;
        Ok(token)
    }
}

impl NodeManager {
    async fn ensure_valid_token(&mut self) -> Result<()> {
        let token_expiry = self.decode_token_expiry(&self.jwt)?;
        let now = OffsetDateTime::now_utc();

        // Refresh if expires within 1 day
        if token_expiry - now < Duration::from_secs(24 * 3600) {
            tracing::info!("JWT token expiring soon, refreshing");

            let new_token = self.coordinator_client
                .refresh_manager_token(self.uuid)
                .await?;

            self.jwt = new_token;

            // Reconnect WebSocket with new token
            self.reconnect_websocket().await?;
        }

        Ok(())
    }
}
```

#### Token Refresh API

```http
POST /managers/{uuid}/refresh-token
Authorization: Bearer <old-manager-jwt>

Response 200 OK:
{
  "token": "eyJ0eXAiOiJKV1QiLCJhbGc..."
}
```

### 11.3 Authentication Flow

```
1. Manager Registration:
   POST /managers {tags, groups}
   → Coordinator validates user auth
   → Creates manager record
   → Generates JWT (30-day expiry)
   → Returns {manager_uuid, token}

2. WebSocket Connection:
   WS /ws/managers
   Authorization: Bearer <manager-jwt>
   → Coordinator validates JWT
   → Extracts manager_uuid from token
   → Establishes persistent connection

3. Task Operations (via Manager):
   Managed Worker → Manager (IPC)
   → Manager → Coordinator (WebSocket with manager JWT)
   → Coordinator authenticates manager
   → Manager proxies response → Worker (IPC)

4. Token Refresh (before expiry):
   Manager detects token expires in < 24h
   → POST /managers/{uuid}/refresh-token
   → Coordinator validates old token
   → Issues new token (30-day expiry)
   → Manager reconnects WebSocket
```

---

## 12. Performance and Scalability

### 12.1 Throughput Optimization

#### Task Pre-fetching

**Problem:** Worker waits for network round-trip on every task fetch

**Solution:** Manager maintains local buffer of prefetched tasks

```rust
impl NodeManager {
    async fn maintain_task_buffer(&mut self) -> Result<()> {
        let prefetch_count = self.current_suite
            .as_ref()
            .map(|s| s.worker_schedule.task_prefetch_count)
            .unwrap_or(16);

        while self.local_task_queue.len() < prefetch_count as usize {
            match self.coordinator_client.fetch_task_from_suite(self.assigned_suite_uuid).await {
                Ok(Some(task)) => {
                    self.local_task_queue.push_back(task);
                }
                Ok(None) => {
                    tracing::debug!("No more tasks available in suite");
                    break;
                }
                Err(e) => {
                    tracing::error!("Failed to prefetch task: {}", e);
                    break;
                }
            }
        }

        Ok(())
    }

    async fn handle_worker_fetch_task(&mut self, req: FetchTaskRequest) -> Result<FetchTaskResponse> {
        // Serve from local buffer (fast path)
        if let Some(task) = self.local_task_queue.pop_front() {
            // Async refill buffer in background
            tokio::spawn({
                let manager = self.clone();
                async move {
                    let _ = manager.maintain_task_buffer().await;
                }
            });

            return Ok(FetchTaskResponse { task: Some(task) });
        }

        // Buffer empty, fetch directly (slow path)
        let task = self.coordinator_client.fetch_task_from_suite(self.assigned_suite_uuid).await?;
        Ok(FetchTaskResponse { task })
    }
}
```

**Performance Impact:**

| Scenario | Without Prefetch | With Prefetch (32 tasks) |
|----------|------------------|--------------------------|
| Worker idle → task available | 50ms (WS round-trip) | 1ms (local buffer) |
| 16 workers, 1000 tasks | 50s overhead | 1.6s overhead |
| Throughput improvement | 1x | ~30x for task fetch latency |

#### WebSocket Pipelined Multiplexing

**Problem:** Serializing requests blocks workers

**Solution:** Concurrent pipelined requests over single WebSocket

```rust
// Example: 16 workers making simultaneous requests
Worker1 → Manager → Coordinator (req_id=1, FetchTask)
Worker2 → Manager → Coordinator (req_id=2, FetchTask)
Worker3 → Manager → Coordinator (req_id=3, ReportTask)
...
Worker16 → Manager → Coordinator (req_id=16, FetchTask)

// Responses arrive out-of-order (OK!)
Coordinator → Manager (req_id=5, Task)
Coordinator → Manager (req_id=1, Task)
Coordinator → Manager (req_id=3, Ack)
...

Manager routes responses by request_id to correct worker
```

**Performance Comparison:**

| Strategy | 16 Workers, 100ms Task Execution | Time for 16 Tasks |
|----------|----------------------------------|-------------------|
| Serialized (1 request at a time) | 16 × (10ms WS + 100ms exec) = 1760ms | 1.76s |
| Pipelined (concurrent requests) | max(16 × 10ms, 100ms) = 160ms | 0.16s |
| **Speedup** | | **11x** |

### 12.2 Database Optimization

#### Connection Pooling

```rust
pub struct CoordinatorConfig {
    pub db_pool_size: u32,           // Default: 50
    pub db_max_connections: u32,     // Default: 100
    pub db_connection_timeout: Duration,  // Default: 30s
}

let pool = PgPoolOptions::new()
    .max_connections(config.db_max_connections)
    .acquire_timeout(config.db_connection_timeout)
    .idle_timeout(Duration::from_secs(300))
    .connect(&database_url)
    .await?;
```

**With PgBouncer:**

```ini
[databases]
mitosis = host=localhost port=5432 dbname=mitosis

[pgbouncer]
pool_mode = transaction
max_client_conn = 1000
default_pool_size = 50
server_lifetime = 3600
server_idle_timeout = 600
```

#### Batch Updates

```rust
// Instead of:
for task_id in task_ids {
    sqlx::query!("UPDATE active_tasks SET state = $1 WHERE id = $2", state, task_id)
        .execute(&db)
        .await?;
}

// Do:
sqlx::query!(
    "UPDATE active_tasks
     SET state = $1, updated_at = NOW()
     WHERE id = ANY($2)",
    state,
    &task_ids
)
.execute(&db)
.await?;
```

#### Indices Optimization

Already covered in [Section 6.1](#61-new-database-tables), key indices:

```sql
-- High-frequency queries
CREATE INDEX idx_task_suites_state ON task_suites(state);
CREATE INDEX idx_node_managers_state ON node_managers(state);
CREATE INDEX idx_active_tasks_suite ON active_tasks(task_suite_id);

-- Tag/label queries
CREATE INDEX idx_task_suites_tags ON task_suites USING GIN(tags);
CREATE INDEX idx_task_suites_labels ON task_suites USING GIN(labels);
CREATE INDEX idx_node_managers_tags ON node_managers USING GIN(tags);
```

### 12.3 Scalability Targets

#### Throughput Targets

| Metric | Target | Notes |
|--------|--------|-------|
| Tasks per second (per worker) | 0.1 - 100 | Task-dependent |
| Managers per coordinator | 1,000 - 10,000 | Limited by WebSocket connections |
| Workers per manager | 1 - 256 | Limited by CPU cores, file descriptors |
| Suites per group | 1,000+ | No hard limit |
| Tasks per suite | 100,000+ | Limited by database size |
| Concurrent suites | 1,000+ | Limited by manager availability |

#### Latency Targets

| Operation | P50 | P95 | P99 |
|-----------|-----|-----|-----|
| Task fetch (HTTP, independent) | 50ms | 200ms | 500ms |
| Task fetch (WebSocket + prefetch) | 1ms | 10ms | 50ms |
| Task commit | 20ms | 100ms | 200ms |
| Suite creation | 100ms | 300ms | 500ms |
| Manager registration | 100ms | 300ms | 500ms |
| Env preparation | 1s - 5min | (workload-dependent) | |
| Env cleanup | 1s - 2min | (workload-dependent) | |

#### Resource Requirements

**Coordinator:**
- CPU: 4-8 cores
- Memory: 8-16 GB
- Connections: 1000+ WebSocket + 50 DB connections
- Database: PostgreSQL (100 GB+)

**Node Manager:**
- CPU: 1 core + worker cores
- Memory: 1 GB + worker memory
- File descriptors: 1000+ (for iceoryx2 + workers)
- Disk: Depends on env preparation

**Managed Worker:**
- CPU: 0.5 - 8 cores (as scheduled)
- Memory: 1 - 64 GB (task-dependent)
- Disk: 10 GB+ (for artifacts)

---

## 13. Migration Strategy

### 13.1 Backward Compatibility

**Independent workers continue to work unchanged:**

- Existing workers not affected
- No changes to worker registration, task fetch, task report APIs
- Existing tasks submitted without `suite_uuid` work as before
- Database schema changes are additive (new tables, new columns with defaults)

### 13.2 Phased Rollout

#### Phase 1: Database Migration (Week 1)

```bash
# Apply migrations
./migrations/000_add_task_suites.sql
./migrations/001_add_node_managers.sql
./migrations/002_add_suite_managers.sql
./migrations/003_add_failure_tracking.sql
./migrations/004_add_triggers.sql

# Verify
psql -d mitosis -c "SELECT * FROM task_suites LIMIT 1;"
psql -d mitosis -c "SELECT * FROM node_managers LIMIT 1;"
```

#### Phase 2: Coordinator Update (Week 2)

**Deploy new coordinator with:**
- WebSocket manager for Node Managers
- Suite APIs (`POST /suites`, etc.)
- Manager APIs (`POST /managers`, etc.)
- Background task for suite state transitions

**Testing:**
- Deploy to staging environment
- Verify independent workers still work
- Create test suites manually via API
- Monitor metrics (latency, throughput)

#### Phase 3: Node Manager Implementation (Week 3-4)

**Implement Node Manager:**
- Registration and WebSocket connection
- IPC services (iceoryx2)
- Worker spawning and monitoring
- Env preparation/cleanup hooks
- Failure tracking and retry logic

**Testing:**
- Single manager with 1 worker on test suite
- Single manager with 16 workers on test suite
- Multiple managers competing for same suite
- Worker crash and auto-recovery
- Network disconnection and reconnection

#### Phase 4: Managed Worker Mode (Week 5)

**Update worker code:**
- Add IPC communication implementation
- Add `--managed` flag for managed mode
- Test with manager IPC

**Testing:**
- End-to-end suite execution (prep → execute → cleanup)
- Compare independent vs managed worker performance
- Stress test: 10 managers × 16 workers × 1000 tasks

#### Phase 5: Production Rollout (Week 6+)

**Gradual rollout:**
1. Deploy coordinator with feature flag (suites disabled)
2. Deploy node managers to 10% of machines
3. Create test suites, monitor errors
4. Increase to 50% of machines
5. Full rollout
6. Document and announce feature

### 13.3 Rollback Plan

**If critical issues found:**

1. Disable suite creation via feature flag
2. Drain existing suites (let them complete)
3. Shut down node managers
4. Revert coordinator to previous version
5. Database rollback (if needed):
   ```sql
   DROP TABLE task_execution_failures;
   DROP TABLE task_suite_managers;
   DROP TABLE group_node_manager;
   DROP TABLE node_managers;
   ALTER TABLE active_tasks DROP COLUMN task_suite_id;
   DROP TABLE task_suites;
   ```

**Data preservation:**
- Export suite data before rollback: `pg_dump -t task_suites > backup.sql`
- Archive manager logs for post-mortem analysis

---

## 14. Implementation Plan

### 14.1 Implementation Phases

#### Phase 1: Database Schema (1 week)

**Tasks:**
- [ ] Create migration files
  - [ ] `task_suites` table
  - [ ] `node_managers` table
  - [ ] `group_node_manager` table
  - [ ] `task_suite_managers` table
  - [ ] `task_execution_failures` table
  - [ ] `active_tasks` update (add `task_suite_id`)
- [ ] Add database triggers
  - [ ] Auto-update suite task counts (total_tasks and pending_tasks)
  - [ ] Auto-transition suite states (background task)
- [ ] Add SeaORM entity models
  - [ ] `task_suites.rs`
  - [ ] `node_managers.rs`
  - [ ] `group_node_manager.rs`
  - [ ] `task_suite_managers.rs`
  - [ ] `task_execution_failures.rs`
- [ ] Add enums
  - [ ] `TaskSuiteState`
  - [ ] `NodeManagerState`
  - [ ] `SelectionType`

**Testing:**
- [ ] Integration tests for all CRUD operations
- [ ] Test triggers (suite state transitions)
- [ ] Test foreign key constraints

#### Phase 2: API Schema and Coordinator Endpoints (1 week)

**Tasks:**
- [ ] Define Rust types in `schema.rs`
  - [ ] `CreateTaskSuiteReq/Resp`
  - [ ] `TaskSuiteQueryReq/Resp`
  - [ ] `RegisterManagerReq/Resp`
  - [ ] `ManagerQueryReq/Resp`
  - [ ] `WorkerSchedulePlan`
  - [ ] `EnvHookSpec`
  - [ ] WebSocket message types
- [ ] Implement suite APIs
  - [ ] `POST /suites` (create suite)
  - [ ] `GET /suites` (query suites)
  - [ ] `GET /suites/{uuid}` (get suite details)
  - [ ] `POST /suites/{uuid}/cancel` (cancel suite)
  - [ ] `POST /suites/{uuid}/managers/refresh` (refresh tag-matched managers)
  - [ ] `POST /suites/{uuid}/managers` (add managers)
  - [ ] `DELETE /suites/{uuid}/managers` (remove managers)
- [ ] Implement manager APIs
  - [ ] `POST /managers` (register manager)
  - [ ] `POST /managers/heartbeat` (manager heartbeat)
  - [ ] `GET /managers` (query managers)
  - [ ] `POST /managers/{uuid}/shutdown` (shutdown manager)
  - [ ] `POST /managers/{uuid}/refresh-token` (refresh JWT)
- [ ] Update task submission API
  - [ ] Add `suite_uuid` field to `SubmitTaskReq`
  - [ ] Update task insertion logic

**Testing:**
- [ ] Unit tests for all endpoints
- [ ] Permission checks (group roles)
- [ ] Tag matching algorithm
- [ ] Suite state transitions

#### Phase 3: WebSocket Manager (1 week)

**Tasks:**
- [ ] Implement WebSocket server
  - [ ] Connection upgrade handler
  - [ ] JWT authentication on connect
  - [ ] Connection registry (manager_uuid → WebSocket)
  - [ ] Message routing (serialize/deserialize)
  - [ ] Request-response multiplexing
  - [ ] Broadcast to all managers
- [ ] Implement coordinator message handlers
  - [ ] `ManagerMessage::Heartbeat`
  - [ ] `ManagerMessage::FetchTask`
  - [ ] `ManagerMessage::ReportTask`
  - [ ] `ManagerMessage::ReportFailure`
  - [ ] `ManagerMessage::AbortTask`
  - [ ] `ManagerMessage::SuiteCompleted`
- [ ] Implement coordinator → manager messages
  - [ ] `CoordinatorMessage::SuiteAssigned`
  - [ ] `CoordinatorMessage::TaskAvailable`
  - [ ] `CoordinatorMessage::TaskReportAck`
  - [ ] `CoordinatorMessage::CancelTask`
  - [ ] `CoordinatorMessage::CancelSuite`
  - [ ] `CoordinatorMessage::Shutdown`
- [ ] Implement manager disconnection handling
  - [ ] Heartbeat timeout detection
  - [ ] Reclaim suite from manager
  - [ ] Return prefetched tasks to queue

**Testing:**
- [ ] WebSocket connection/disconnection
- [ ] Message serialization/deserialization
- [ ] Concurrent requests from multiple managers
- [ ] Manager heartbeat timeout
- [ ] Reconnection after network failure

#### Phase 4: Node Manager Core (2 weeks)

**Tasks:**
- [ ] Create `mitosis-manager` binary
  - [ ] CLI argument parsing
  - [ ] Configuration loading
  - [ ] Logging setup
- [ ] Implement registration
  - [ ] `POST /managers` HTTP call
  - [ ] JWT token storage
- [ ] Implement WebSocket client
  - [ ] Connection establishment
  - [ ] Message send/receive
  - [ ] Request-response multiplexing
  - [ ] Reconnection logic with exponential backoff
  - [ ] Token refresh
- [ ] Implement state machine
  - [ ] `NodeManagerState` enum
  - [ ] State transitions
- [ ] Implement suite assignment
  - [ ] Receive `SuiteAssigned` message
  - [ ] Parse suite spec
  - [ ] Transition to Preparing state

**Testing:**
- [ ] Manager registration
- [ ] WebSocket connection stability
- [ ] Token refresh mechanism
- [ ] Reconnection after coordinator restart

#### Phase 5: Environment Hooks (1 week)

**Tasks:**
- [ ] Implement env preparation
  - [ ] Download resources from S3
  - [ ] Set environment variables
  - [ ] Execute preparation command
  - [ ] Timeout handling
  - [ ] Error handling (abort on failure)
- [ ] Implement env cleanup
  - [ ] Execute cleanup command
  - [ ] Timeout handling
  - [ ] Error handling (log but don't fail)

**Testing:**
- [ ] Successful preparation/cleanup
- [ ] Preparation timeout
- [ ] Preparation failure (abort suite)
- [ ] Cleanup failure (log error, continue)
- [ ] S3 resource download

#### Phase 6: Worker Spawning and IPC (2 weeks)

**Tasks:**
- [ ] Setup iceoryx2 services
  - [ ] Request-response services (task fetch, task report)
  - [ ] Pub-sub topics (control, config)
- [ ] Implement worker spawning
  - [ ] Spawn N workers as subprocesses
  - [ ] CPU core binding (RoundRobin, Exclusive, Shared)
  - [ ] Pass IPC config to workers
  - [ ] Wait for workers to connect
- [ ] Implement worker monitoring
  - [ ] Poll process status
  - [ ] Detect exit (waitpid)
  - [ ] Classify exit reason (signal vs exit code)
  - [ ] Auto-respawn on crash
- [ ] Implement IPC request handlers
  - [ ] `FetchTaskRequest` → fetch from local buffer or coordinator
  - [ ] `ReportTaskRequest` → proxy to coordinator
  - [ ] `HeartbeatRequest` (optional)
- [ ] Implement task pre-fetching
  - [ ] Maintain local buffer
  - [ ] Async refill in background
- [ ] Implement failure tracking
  - [ ] Track failures per task
  - [ ] Abort after 3 failures
  - [ ] Report to coordinator

**Testing:**
- [ ] Spawn 1 worker
- [ ] Spawn 16 workers
- [ ] Spawn 256 workers (max)
- [ ] CPU binding strategies
- [ ] Worker crash and auto-recovery
- [ ] Worker death with task in progress
- [ ] Task pre-fetching buffer
- [ ] IPC message passing

#### Phase 7: Managed Worker Mode (1 week)

**Tasks:**
- [ ] Update worker binary
  - [ ] Add `--managed` CLI flag
  - [ ] Parse manager UUID and worker local ID
- [ ] Implement IPC communication
  - [ ] `IpcWorkerComm` struct
  - [ ] `fetch_task()` via IPC
  - [ ] `report_task()` via IPC
  - [ ] `send_heartbeat()` via IPC (optional)
- [ ] Implement control message handling
  - [ ] Subscribe to `manager/control` topic
  - [ ] Handle `Shutdown` message
  - [ ] Handle `CancelTask` message

**Testing:**
- [ ] Managed worker connects to manager via IPC
- [ ] Managed worker fetches task
- [ ] Managed worker reports task
- [ ] Managed worker receives shutdown
- [ ] Managed worker receives cancel task

#### Phase 8: Integration and E2E Testing (1 week)

**Tasks:**
- [ ] End-to-end suite execution
  - [ ] Create suite
  - [ ] Assign managers
  - [ ] Submit tasks
  - [ ] Execute (prep → workers → cleanup)
  - [ ] Verify completion
- [ ] Multi-manager scenarios
  - [ ] 10 managers competing for same suite
  - [ ] Tag-based auto-matching
  - [ ] User-specified manager assignment
- [ ] Failure scenarios
  - [ ] Worker crash during task
  - [ ] Manager disconnection
  - [ ] Coordinator restart
  - [ ] Environment preparation failure
  - [ ] Database connection loss
- [ ] Performance testing
  - [ ] 1 manager × 16 workers × 10,000 tasks
  - [ ] 10 managers × 16 workers × 100,000 tasks
  - [ ] Measure latency, throughput

**Testing:**
- [ ] All scenarios pass
- [ ] No data loss
- [ ] Performance meets targets

#### Phase 9: Documentation and Deployment (1 week)

**Tasks:**
- [ ] User documentation
  - [ ] Suite creation guide
  - [ ] Manager deployment guide
  - [ ] Environment hooks examples
  - [ ] Troubleshooting guide
- [ ] API documentation
  - [ ] OpenAPI spec update
  - [ ] Code examples
- [ ] Internal documentation
  - [ ] Architecture diagrams
  - [ ] State machine diagrams
  - [ ] Deployment runbook
- [ ] Monitoring and alerts
  - [ ] Add Prometheus metrics
  - [ ] Add Grafana dashboards
  - [ ] Define alert thresholds
- [ ] Deployment
  - [ ] Staging rollout
  - [ ] Production rollout (phased)
  - [ ] Monitor errors

**Deliverables:**
- [ ] User guide published
- [ ] API docs updated
- [ ] Monitoring dashboards live
- [ ] Feature announced

### 14.2 Estimated Timeline

| Phase | Duration | Dependencies |
|-------|----------|--------------|
| 1. Database Schema | 1 week | None |
| 2. API Schema and Coordinator Endpoints | 1 week | Phase 1 |
| 3. WebSocket Manager | 1 week | Phase 2 |
| 4. Node Manager Core | 2 weeks | Phase 3 |
| 5. Environment Hooks | 1 week | Phase 4 |
| 6. Worker Spawning and IPC | 2 weeks | Phase 5 |
| 7. Managed Worker Mode | 1 week | Phase 6 |
| 8. Integration and E2E Testing | 1 week | Phase 7 |
| 9. Documentation and Deployment | 1 week | Phase 8 |
| **Total** | **11 weeks** | |

**With 2 engineers:** ~6 weeks (parallel work on coordinator + manager)

---

## 15. Appendices

### 15.1 Glossary

| Term | Definition |
|------|------------|
| **Task Suite** | A collection of related tasks with shared lifecycle and execution environment |
| **Node Manager** | A device-level service that manages worker pools and orchestrates task execution |
| **Managed Worker** | A worker spawned by a Node Manager, communicating via IPC (anonymous to coordinator) |
| **Independent Worker** | A worker registered directly with coordinator, communicating via HTTP (current mode) |
| **Environment Hooks** | Scripts executed before/after suite execution (env_preparation, env_cleanup) |
| **Tag Matching** | Algorithm to match suites to managers based on tag sets (set containment) |
| **Task Pre-fetching** | Manager fetches multiple tasks ahead of time to reduce latency |
| **Lease** | Time-bound assignment of suite to manager (can expire) |
| **Auto-close Timeout** | Fixed 3-minute inactivity timeout after which suite transitions from Open to Closed |
| **Worker Local ID** | Integer ID (0, 1, 2, ...) identifying managed worker within a manager |

### 15.2 Comparison: Independent vs Managed Workers

| Aspect | Independent Worker | Managed Worker |
|--------|-------------------|----------------|
| **Registration** | Registers with coordinator | Spawned by manager (no registration) |
| **Communication** | HTTP to coordinator | IPC to manager → WebSocket to coordinator |
| **Authentication** | Own JWT token | Manager's JWT token (proxied) |
| **Task Fetch** | Polls HTTP API every 5s | Requests via IPC (immediate) |
| **Database Visibility** | Visible in `workers` table | Not visible (anonymous) |
| **Lifecycle** | User-controlled (starts/stops manually) | Tied to task suite execution |
| **Resource Management** | Self-managed (no coordination) | Manager-coordinated (CPU binding, etc.) |
| **Failure Handling** | Coordinator detects heartbeat timeout | Manager detects crash, auto-respawns |
| **Environment Setup** | N/A (each worker independent) | Shared preparation/cleanup hooks |
| **Use Case** | General-purpose, long-running workers | Batch jobs, ML campaigns, CI/CD |

### 15.3 FAQ

**Q: Can I mix independent and managed workers?**
A: Yes! Independent workers continue to work unchanged. You can have both running simultaneously.

**Q: What happens if manager crashes during suite execution?**
A: Coordinator detects heartbeat timeout, marks manager as offline, reclaims all prefetched tasks, and reopens the suite for other managers.

**Q: Can multiple managers execute the same suite simultaneously?**
A: Yes! Multiple managers can work on the same suite in parallel, each fetching and executing tasks from the shared queue.

**Q: What if no managers match a suite's tags?**
A: The suite stays in queue. User can either: (1) add managers with matching tags, (2) explicitly assign managers by UUID, or (3) update suite tags.

**Q: Can I reuse a suite after it completes?**
A: Yes! Submitting new tasks to a completed suite will reopen it (state: Complete → Open).

**Q: How do I cancel all running tasks in a suite?**
A: `POST /suites/{uuid}/cancel` with `cancel_running_tasks: true`. Coordinator sends cancellation to all assigned managers via WebSocket.

**Q: What's the maximum number of workers per manager?**
A: No hard limit in code, but practical limits: CPU cores (for binding), file descriptors (for iceoryx2), memory. Recommended: ≤ 256 workers.

**Q: Can I update suite tags after creation?**
A: Not in v1. Tags are immutable after creation. You can create a new suite with updated tags.

**Q: Do environment hooks run every time a task executes?**
A: No! Hooks run once per suite assignment:
- `env_preparation`: Before first task
- `env_cleanup`: After last task (suite completion)

**Q: What if env_preparation times out?**
A: Manager aborts the suite, returns to idle, and tries next eligible suite. Suite stays in queue for other managers.

**Q: Can I see which manager is executing my suite?**
A: Yes! `GET /suites/{uuid}` returns `assigned_managers` array.

**Q: How do I migrate existing workflows to use suites?**
A: Start by grouping related tasks under a suite. Example:
```bash
# Old: Submit 1000 independent tasks
for i in {1..1000}; do
  curl -X POST /tasks -d '{"group_name": "ml", "task_spec": {...}}'
done

# New: Create suite, then submit tasks to it
SUITE_UUID=$(curl -X POST /suites -d '{"name": "Training", "group_name": "ml", ...}' | jq -r .uuid)
for i in {1..1000}; do
  curl -X POST /tasks -d '{"group_name": "ml", "suite_uuid": "'$SUITE_UUID'", "task_spec": {...}}'
done
```

### 15.4 References

**Design Documents:**
- `overview.md` - Original design proposal
- `design-refinements.md` - Detailed refinements and analysis

**Related RFCs:**
- (None yet - this is the first RFC for this feature)

**External Dependencies:**
- [iceoryx2](https://github.com/eclipse-iceoryx/iceoryx2) - Zero-copy IPC framework
- [tokio-tungstenite](https://github.com/snapview/tokio-tungstenite) - WebSocket implementation
- [SeaORM](https://www.sea-ql.org/SeaORM/) - Async ORM for PostgreSQL

**Standards:**
- [WebSocket Protocol (RFC 6455)](https://datatracker.ietf.org/doc/html/rfc6455)
- [JWT (RFC 7519)](https://datatracker.ietf.org/doc/html/rfc7519)

---

## Changelog

| Version | Date | Changes |
|---------|------|---------|
| 1.0.0 | 2025-11-11 | Initial RFC incorporating all design discussions |

---

**END OF RFC**
