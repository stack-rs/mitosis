# Worker Manager System - Design Overview

**Status:** Design Phase - Decisions Made
**Last Updated:** 2025-11-05
**Version:** 0.2.0

## Table of Contents
1. [Current Architecture Summary](#current-architecture-summary)
2. [Motivation](#motivation)
3. [New Concepts](#new-concepts)
4. [Architecture Design](#architecture-design)
5. [Data Models](#data-models)
6. [Communication Protocols](#communication-protocols)
7. [Workflow](#workflow)
8. [Open Questions](#open-questions)
9. [Implementation Plan](#implementation-plan)

---

## Current Architecture Summary

### Existing Components

#### Workers (Independent Mode - Current)
- **Registration**: Workers register with coordinator via `POST /workers`
- **Authentication**: Receive JWT token for subsequent requests
- **Task Polling**: Periodically fetch tasks via `GET /workers/tasks` (default 5s interval)
- **Execution**: Execute tasks locally, upload results to S3
- **Reporting**: Report status via `POST /workers/tasks` (Finish/Cancel/Commit/Upload)
- **Heartbeat**: Send heartbeats via `POST /workers/heartbeat` (default 600s timeout)
- **Tags/Labels**: Workers have tags for task matching, belong to groups

#### Coordinator
- **TaskDispatcher**: In-memory priority queues per worker (HashMap<WorkerId → PriorityQueue<TaskId>>)
- **HeartbeatQueue**: Priority queue tracking worker timeout
- **Task Distribution**: Batch assigns tasks to eligible workers based on tags and group membership
- **Authentication**: JWT-based (EdDSA) with group-based access control
- **Database**: PostgreSQL with Users, Groups, Workers, ActiveTasks, ArchivedTasks tables

#### Current Task Flow
1. User submits task to a **Group** with tags/labels/priority
2. Task enters coordinator's queue
3. Workers with matching tags poll and fetch tasks
4. Worker executes task, uploads artifacts to S3
5. Worker commits result, task moves to ArchivedTasks

### Current Limitations
- **No device-level resource management**: Workers compete for resources
- **No environment orchestration**: Cannot prepare/cleanup shared environments
- **Single-task granularity**: No grouping of related tasks with lifecycle
- **Polling overhead**: Workers constantly poll even when idle

---

## Motivation

### Problems to Solve
1. **Resource Contention**: Multiple workers on same device may conflict for resources (GPU, memory, disk)
2. **Environment Management**: Need to prepare and cleanup environments at the task group level
3. **Batch Job Orchestration**: Want to run a set of related tasks with lifecycle management
4. **Manager-Level Scheduling**: Schedule work at the "campaign" level, not individual tasks

### Use Cases
- **ML Training Campaigns**: Prepare dataset, run multiple training jobs, cleanup artifacts
- **CI/CD Pipelines**: Setup environment, run test suite, tear down
- **Batch Processing**: Process a batch of data with shared resources

---

## New Concepts

### 1. Task Group
A **Task Group** is a new first-class entity that groups related tasks and defines their execution environment.

**Components:**
- **Worker Scheduling Plan**: Defines how many workers to spawn, CPU binding, and other resources
- **Environment Preparation**: TaskSpec-like definition for environment setup
- **Environment Cleanup**: TaskSpec-like definition for environment teardown
- **State**: Lifecycle state (Open → Closed → Complete)
- **Priority**: Integer priority for manager scheduling (similar to task priority)
- **Tags/Labels**: For matching with eligible managers

**Properties:**
- Tasks are submitted **into** a task group (not just to a regular group)
- Task groups have **hybrid completion**: explicit close API OR configurable timeout
- Task groups belong to regular Groups (inherit permissions)
- Efficient timeout via database triggers or event-based mechanism (not polling)

### 2. Worker Manager
A **Worker Manager** is a new service component that:

**Responsibilities:**
- Runs on a physical device/server
- Manages local worker processes according to task group scheduling plans (acts as a managed worker pool)
- Controls device-level resources (CPU core binding)
- Executes environment preparation/cleanup
- Proxies communication between managed workers and coordinator
- **Auto-recovery**: Re-spawns workers if they die accidentally

**Scheduling Behavior:**
- Managers execute **one task group at a time** (exclusive execution)
- After completing a task group, manager picks the next eligible one
- Task group assignment uses **priority-based scheduling** (similar to task-worker matching)
- Matches based on task group tags/labels and manager capabilities
- No hard limit on workers per manager (defined by scheduling plan)

**Authentication:**
- Managers register with coordinator (receive JWT token)
- **Only managers authenticate** with coordinator (not individual workers)
- Managers belong to groups with permissions
- Only execute task groups they're authorized for

### 3. Worker Modes

#### Independent Mode (Current - Unchanged)
- Worker registers directly with coordinator
- Polls coordinator HTTP API for tasks
- No local manager involvement

#### Managed Mode (New)
- Worker spawned by local Worker Manager as local process
- **Anonymous to coordinator** (not registered, invisible in DB)
- Communicates with Manager via **iceoryx2 IPC** (not HTTP)
- Manager proxies requests to coordinator using **manager's JWT**
- Worker lifecycle tied to task group execution
- Code and mechanisms reused from independent workers
- If worker dies, manager auto-respawns replacement

---

## Architecture Design

### Component Diagram

```
┌─────────────────────────────────────────────────────────────┐
│                        Coordinator                           │
│  ┌─────────────┐  ┌──────────────┐  ┌──────────────────┐   │
│  │TaskDispatcher│  │ManagerQueue  │  │ HeartbeatQueue   │   │
│  │(Workers)     │  │(Task Groups) │  │(Workers+Managers)│   │
│  └─────────────┘  └──────────────┘  └──────────────────┘   │
│                         HTTP API                             │
└─────────────────────────────────────────────────────────────┘
         │                    │                    │
         │ HTTP               │ HTTP               │ HTTP
         ▼                    ▼                    ▼
┌─────────────────┐  ┌──────────────────────────────────────┐
│ Independent     │  │        Worker Manager                 │
│ Worker          │  │  ┌────────────────────────────────┐  │
│                 │  │  │ Manager Process                │  │
│ • Polls tasks   │  │  │ • Fetches task groups          │  │
│ • HTTP to coord │  │  │ • Spawns managed workers       │  │
└─────────────────┘  │  │ • Env preparation/cleanup      │  │
                     │  │ • IPC proxy to coordinator     │  │
                     │  └────────────────────────────────┘  │
                     │             │ iceoryx2 IPC           │
                     │             ▼                         │
                     │  ┌────────────────────────────────┐  │
                     │  │ Managed Worker 1               │  │
                     │  │ • IPC to manager (not HTTP)    │  │
                     │  │ • Task execution               │  │
                     │  └────────────────────────────────┘  │
                     │  ┌────────────────────────────────┐  │
                     │  │ Managed Worker 2               │  │
                     │  └────────────────────────────────┘  │
                     └──────────────────────────────────────┘
```

### IPC Architecture (iceoryx2)

Based on iceoryx2 capabilities, we'll use **both** communication patterns:

#### 1. Request-Response Pattern
**Use Case**: Worker ↔ Manager communication (task fetch, heartbeat, report)

**Endpoints:**
- `manager/task/fetch` - Worker requests next task from manager
- `manager/task/report` - Worker reports task status/result
- `manager/heartbeat` - Worker sends heartbeat to manager

**Flow:**
```
Worker                    Manager                  Coordinator
  │                         │                          │
  │ REQUEST: fetch_task     │                          │
  ├────────────────────────>│                          │
  │                         │ HTTP: GET /workers/tasks │
  │                         ├─────────────────────────>│
  │                         │<─────────────────────────┤
  │ RESPONSE: TaskSpec      │                          │
  │<────────────────────────┤                          │
  │                         │                          │
```

#### 2. Publish-Subscribe Pattern
**Use Case**: Manager → Workers broadcast (shutdown signals, config updates)

**Topics:**
- `manager/control` - Control commands (shutdown, pause, resume)
- `manager/config` - Configuration updates

**Flow:**
```
Manager                   Workers (Subscribers)
  │                         │
  │ PUB: shutdown           │
  ├────────────────────────>├──> Worker 1
  │                         ├──> Worker 2
  │                         └──> Worker N
```

---

## Data Models

### New Database Tables

#### 1. `task_groups` Table
```sql
CREATE TABLE task_groups (
    id BIGSERIAL PRIMARY KEY,
    uuid UUID UNIQUE NOT NULL,
    name TEXT NOT NULL,
    group_id BIGINT NOT NULL REFERENCES groups(id),  -- Parent group for permissions
    creator_id BIGINT NOT NULL REFERENCES users(id),

    -- Scheduling attributes
    tags TEXT[] NOT NULL,   -- For matching with managers
    labels TEXT[] NOT NULL, -- User-defined labels
    priority INTEGER NOT NULL,  -- Scheduling priority

    -- Scheduling plan (JSON)
    worker_schedule JSONB NOT NULL,  -- {worker_count, cpu_binding: {cores: [0,1,2]}}

    -- Lifecycle hooks (TaskSpec-like format, JSON)
    env_preparation JSONB,  -- {args, envs, resources, timeout}
    env_cleanup JSONB,      -- {args, envs, resources, timeout}

    -- State management
    state INTEGER NOT NULL DEFAULT 0,  -- Open=0, Closed=1, Complete=2
    auto_close_timeout BIGINT,  -- Timeout in seconds for auto-close (NULL = no timeout)
    last_task_submitted_at TIMESTAMPTZ,  -- For timeout calculation

    -- Timestamps
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    completed_at TIMESTAMPTZ,

    -- Assigned manager
    assigned_manager_id BIGINT REFERENCES worker_managers(id),

    UNIQUE(group_id, name)
);

-- Index for efficient timeout queries
CREATE INDEX idx_task_groups_auto_close
    ON task_groups(last_task_submitted_at)
    WHERE state = 0 AND auto_close_timeout IS NOT NULL;
```

#### 2. `worker_managers` Table
```sql
CREATE TABLE worker_managers (
    id BIGSERIAL PRIMARY KEY,
    manager_id UUID UNIQUE NOT NULL,
    creator_id BIGINT NOT NULL REFERENCES users(id),

    -- Manager properties
    tags TEXT[] NOT NULL,
    labels TEXT[] NOT NULL,

    -- State
    state INTEGER NOT NULL DEFAULT 0,  -- Idle=0, Executing=1, Offline=2
    last_heartbeat TIMESTAMPTZ NOT NULL,

    -- Current assignment
    assigned_task_group_id BIGINT REFERENCES task_groups(id),

    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,

    FOREIGN KEY (creator_id) REFERENCES users(id)
);
```

#### 3. `group_manager` Join Table
```sql
CREATE TABLE group_manager (
    id BIGSERIAL PRIMARY KEY,
    group_id BIGINT NOT NULL REFERENCES groups(id),
    manager_id BIGINT NOT NULL REFERENCES worker_managers(id),
    role INTEGER NOT NULL,  -- Same roles as GroupWorkerRole

    UNIQUE(group_id, manager_id)
);
```

#### 4. Update `active_tasks` Table
Add foreign key to task_group:
```sql
ALTER TABLE active_tasks
ADD COLUMN task_group_id BIGINT REFERENCES task_groups(id);
```

### API Schema Updates

#### New Request/Response Types

```rust
// Task Group Creation
#[derive(Serialize, Deserialize)]
pub struct CreateTaskGroupReq {
    pub name: String,
    pub group_name: String,  // Parent group
    pub tags: HashSet<String>,  // For manager matching
    pub labels: HashSet<String>,
    pub priority: i32,
    pub worker_schedule: WorkerSchedulePlan,
    pub env_preparation: Option<EnvHookSpec>,
    pub env_cleanup: Option<EnvHookSpec>,
    #[serde(default)]
    #[serde(with = "humantime_serde")]
    pub auto_close_timeout: Option<Duration>,  // Auto-close if no tasks for this duration
}

#[derive(Serialize, Deserialize)]
pub struct WorkerSchedulePlan {
    pub worker_count: u32,
    pub cpu_binding: Option<CpuBinding>,
    // Future: memory limits, GPU assignment, etc.
}

#[derive(Serialize, Deserialize)]
pub struct CpuBinding {
    pub cores: Vec<usize>,  // CPU core IDs to bind workers to
    pub strategy: CpuBindingStrategy,
}

#[derive(Serialize, Deserialize)]
pub enum CpuBindingStrategy {
    RoundRobin,  // Distribute workers across cores
    Exclusive,   // Each worker gets dedicated core(s)
    Shared,      // All workers share all cores
}

// Environment hook specification (similar to TaskSpec)
#[derive(Serialize, Deserialize)]
pub struct EnvHookSpec {
    pub args: Vec<String>,  // Command to execute
    #[serde(default)]
    pub envs: HashMap<String, String>,
    #[serde(default)]
    pub resources: Vec<RemoteResourceDownload>,  // Can download files from S3
    #[serde(with = "humantime_serde")]
    pub timeout: Duration,
    // Context available via environment variables:
    // - MITOSIS_TASK_GROUP_UUID
    // - MITOSIS_TASK_GROUP_NAME
    // - MITOSIS_GROUP_NAME
    // - MITOSIS_WORKER_COUNT
}

// Task submission update
#[derive(Serialize, Deserialize)]
pub struct SubmitTaskReq {
    pub group_name: String,
    pub task_group_name: Option<String>,  // NEW: optional task group
    pub tags: HashSet<String>,
    pub labels: HashSet<String>,
    pub timeout: Duration,
    pub priority: i32,
    pub task_spec: TaskSpec,
}

// Manager registration
#[derive(Serialize, Deserialize)]
pub struct RegisterManagerReq {
    pub tags: HashSet<String>,
    pub labels: HashSet<String>,
    pub groups: HashSet<String>,
    pub lifetime: Option<Duration>,
}

#[derive(Serialize, Deserialize)]
pub struct RegisterManagerResp {
    pub manager_id: Uuid,
    pub token: String,
}
```

---

## Communication Protocols

### Manager ↔ Coordinator (HTTP)

**New Endpoints:**

1. **Manager Registration**
   - `POST /managers`
   - Request: `RegisterManagerReq`
   - Response: `RegisterManagerResp` (includes JWT token)

2. **Fetch Task Group**
   - `GET /managers/task-groups`
   - Response: `ManagerTaskGroupResp` (task group details, worker schedule)

3. **Report Task Group Status**
   - `POST /managers/task-groups`
   - Request: `ReportTaskGroupReq { op: Started | Completed | Failed }`

4. **Manager Heartbeat**
   - `POST /managers/heartbeat`
   - Similar to worker heartbeat

5. **Proxy Task Operations** (on behalf of managed workers)
   - `GET /managers/tasks` - Fetch task for a managed worker (uses manager JWT)
   - `POST /managers/tasks` - Report task result (uses manager JWT)
   - Note: Workers are anonymous, manager authenticates for all operations

### Manager ↔ Worker (iceoryx2 IPC)

#### Request-Response Services

1. **Task Fetching Service**
   ```rust
   // Worker sends request
   struct FetchTaskRequest {
       worker_local_id: u32,  // Local worker ID (0, 1, 2, ...), not UUID
   }

   // Manager responds
   struct FetchTaskResponse {
       task: Option<WorkerTaskResp>,
   }
   ```

2. **Task Reporting Service**
   ```rust
   struct ReportTaskRequest {
       worker_local_id: u32,
       task_id: i64,
       op: ReportTaskOp,
   }

   struct ReportTaskResponse {
       success: bool,
       url: Option<String>,  // For upload operations
   }
   ```

3. **Heartbeat Service** (Manager ↔ Workers, optional)
   ```rust
   struct HeartbeatRequest {
       worker_local_id: u32,
   }

   struct HeartbeatResponse {
       ack: bool,
   }
   ```
   Note: Manager tracks worker process health, not forwarded to coordinator

#### Pub-Sub Topics

1. **Control Topic** (`manager/control`)
   ```rust
   enum ControlMessage {
       Shutdown { graceful: bool },
       Pause,
       Resume,
   }
   ```

2. **Configuration Topic** (`manager/config`)
   ```rust
   struct ConfigUpdate {
       poll_interval: Option<Duration>,
       // Other dynamic config
   }
   ```

---

## Workflow

### Task Group Lifecycle

```
┌──────────┐
│  OPEN    │ ← Tasks can be submitted
└────┬─────┘
     │ User closes task group
     ▼
┌──────────┐
│ CLOSED   │ ← No new tasks, existing tasks can complete
└────┬─────┘
     │ All tasks finished OR timeout OR manual complete
     ▼
┌──────────┐
│ COMPLETE │ ← Manager released, cleanup executed
└──────────┘
```

### Manager Execution Flow

1. **Manager Startup**
   - Manager process starts on device
   - Registers with coordinator (`POST /managers`)
   - Receives JWT token, manager UUID
   - Enters idle state

2. **Task Group Assignment**
   - Manager polls for eligible task groups (`GET /managers/task-groups`)
   - Coordinator checks:
     - Manager's groups/tags match task group requirements
     - Manager is not currently executing another task group
     - Task group is in OPEN or CLOSED state
   - If match found, assign task group to manager

3. **Environment Preparation**
   - Manager executes `env_preparation` hook (TaskSpec-like)
   - Downloads required resources from S3
   - Executes preparation command with context env vars
   - **If fails**: Manager aborts task group, reports failure, tries next eligible group
   - **If succeeds**: Proceed to worker spawning

4. **Worker Spawning**
   - Manager spawns N workers according to `worker_schedule.worker_count`
   - **CPU Binding**: Applies CPU core affinity based on `cpu_binding` strategy
   - Workers launched as **anonymous local processes** (no coordinator registration)
   - Workers configured for IPC mode (not HTTP mode)
   - Manager sets up iceoryx2 services (request-response + pub-sub)
   - **Auto-recovery**: If worker crashes, manager detects and respawns replacement

5. **Task Execution Loop**
   - Workers request tasks via IPC (`FetchTaskRequest`)
   - Manager proxies to coordinator using **manager's JWT** (`GET /managers/tasks`)
   - Workers execute tasks locally
   - Workers report results via IPC (`ReportTaskRequest`)
   - Manager proxies to coordinator (`POST /managers/tasks`)
   - Manager tracks which worker is executing which task

6. **Completion Detection (Hybrid)**
   - **Explicit Close**: User calls `POST /task-groups/{id}/close` → state = CLOSED
   - **Auto-Close**: Efficient timeout mechanism (see below) → state = CLOSED
   - **Completion**: When CLOSED + all tasks finished → state = COMPLETE

   **Efficient Timeout Mechanism** (no polling):
   - On task submission: Update `task_groups.last_task_submitted_at`
   - Background coordinator task uses indexed query:
     ```sql
     SELECT id FROM task_groups
     WHERE state = 0  -- OPEN
       AND auto_close_timeout IS NOT NULL
       AND NOW() - last_task_submitted_at > (auto_close_timeout * INTERVAL '1 second')
     LIMIT 100;
     ```
   - Runs every minute, marks expired task groups as CLOSED
   - Alternative: PostgreSQL `pg_cron` or database trigger for even lower overhead

7. **Environment Cleanup**
   - Manager executes `env_cleanup` hook (TaskSpec-like)
   - Downloads required resources from S3
   - Executes cleanup command with context env vars
   - **If fails**: Log error, mark task group as "degraded complete" (still complete)
   - Shuts down all managed workers (graceful shutdown via pub-sub control topic)
   - Reports completion to coordinator

8. **Next Assignment**
   - Manager returns to idle state
   - Polls for next eligible task group (priority-based matching)
   - Repeat from step 2

### Worker Lifecycle (Managed Mode)

1. **Startup**
   - Spawned by manager with IPC config (not HTTP)
   - Connects to manager's iceoryx2 services
   - Subscribes to control topic

2. **Task Loop**
   - Send `FetchTaskRequest` via IPC
   - Receive `FetchTaskResponse` from manager
   - Execute task
   - Send `ReportTaskRequest` via IPC
   - Manager proxies to coordinator

3. **Shutdown**
   - Receive shutdown message on control topic
   - Graceful: finish current task, then exit
   - Force: immediate exit

---

## Design Decisions

This section documents the key design decisions made for the Worker Manager system.

### ✅ 1. Task Group Completion - Hybrid Approach
**Decision:** Implement hybrid completion mechanism with efficient timeout.

**Details:**
- **Explicit Close API**: `POST /task-groups/{id}/close` sets state to CLOSED
- **Auto-Close Timeout**: Configurable per task group (e.g., 30 minutes of inactivity)
- **Efficient Implementation**: Indexed database query (no polling loop)
  - Update `last_task_submitted_at` on every task submission
  - Background task runs every minute with indexed query
  - Query only task groups with non-null `auto_close_timeout`
- **Completion**: When CLOSED and all tasks finished → state = COMPLETE

### ✅ 2. Worker Anonymity - No Registration
**Decision:** Managed workers are anonymous to coordinator.

**Rationale:**
- Manager acts as a managed worker pool
- Workers are local processes, logically identical
- Manager handles worker crashes and respawning
- Simpler architecture, fewer database entries

**Details:**
- Workers use local IDs (0, 1, 2, ...) for manager tracking
- No worker UUIDs, no JWT tokens, no DB registration
- Manager proxies all worker requests using **manager's JWT**

### ✅ 3. Authentication - Manager Only
**Decision:** Only managers authenticate with coordinator.

**Details:**
- Manager receives JWT token on registration
- Manager uses own token for all proxied worker requests
- Workers are just local processes reusing existing code/mechanisms
- Simpler token management, suitable for managed worker pool model

### ✅ 4. Environment Hooks - TaskSpec-like Format
**Decision:** Hooks use TaskSpec-like structure for consistency.

**Format:**
```rust
struct EnvHookSpec {
    args: Vec<String>,           // Command to execute
    envs: HashMap<String, String>, // Environment variables
    resources: Vec<RemoteResourceDownload>, // S3 resources
    timeout: Duration,           // Execution timeout
}
```

**Context:** Hooks receive task group context via environment variables:
- `MITOSIS_TASK_GROUP_UUID`
- `MITOSIS_TASK_GROUP_NAME`
- `MITOSIS_GROUP_NAME`
- `MITOSIS_WORKER_COUNT`

**Execution:** Manager runs hooks as subprocesses

### ✅ 5. Resource Specification - CPU Binding Focus
**Decision:** Initial implementation focuses on CPU core binding.

**Details:**
```rust
struct CpuBinding {
    cores: Vec<usize>,  // CPU core IDs
    strategy: CpuBindingStrategy, // RoundRobin | Exclusive | Shared
}
```

**Strategies:**
- **RoundRobin**: Distribute workers across cores evenly
- **Exclusive**: Each worker gets dedicated core(s)
- **Shared**: All workers share all specified cores

**Future:** Can extend to memory limits, GPU assignment, etc.

### ✅ 6. Failure Handling
**Decisions:**

| Scenario | Handling |
|----------|----------|
| **Manager crashes** | Coordinator detects via heartbeat timeout → marks offline → returns task group to queue |
| **Worker crashes** | Manager detects → respawns replacement worker automatically |
| **Env preparation fails** | Manager aborts task group → reports failure → tries next eligible group |
| **Env cleanup fails** | Log error → mark as "degraded complete" → manager proceeds to next task group |

### ✅ 7. Task Group Attributes - Priority and Tags/Labels
**Decision:** Task groups have scheduling attributes like tasks.

**Attributes:**
- **Priority** (i32): For scheduling order (higher = higher priority)
- **Tags** (HashSet<String>): For matching with manager capabilities
- **Labels** (HashSet<String>): User-defined metadata

**Scheduling:** Coordinator matches task groups to managers based on:
- Priority (higher first)
- Tags (must match manager tags)
- Group permissions (manager must have access)

### ✅ 8. Manager Capacity - No Hard Limits
**Decision:** No hard limit on workers per manager.

**Rationale:**
- Worker count defined by task group's `worker_schedule.worker_count`
- Manager handles whatever the scheduling plan specifies
- Device resource limits naturally constrain capacity
- Flexibility for different use cases (small vs. large batches)

### ✅ 9. Dynamic Worker Scaling - Via Scheduling Method
**Decision:** Worker scaling is static per task group, defined in scheduling plan.

**Current:** `worker_count` is fixed when task group is created
**Future:** Could add dynamic scaling APIs if needed (e.g., `PUT /task-groups/{id}/scale`)

### ✅ 10. Backward Compatibility
**Decision:** Full backward compatibility with independent workers.

**Details:**
- Independent workers unchanged: `POST /workers`, `GET /workers/tasks`
- Task submission API extended with optional `task_group_name`
- Tasks without `task_group_name` → assigned to independent workers (current behavior)
- Tasks with `task_group_name` → assigned to managed workers via managers
- Both modes coexist seamlessly

### ✅ 11. IPC Service Discovery - Predictable Names
**Decision:** Use predictable iceoryx2 service names based on manager ID.

**Naming Convention:**
- Task Fetch: `manager_{manager_id}/fetch_task`
- Task Report: `manager_{manager_id}/report_task`
- Heartbeat: `manager_{manager_id}/heartbeat`
- Control Topic: `manager_{manager_id}/control`
- Config Topic: `manager_{manager_id}/config`

**Benefits:**
- Zero configuration needed
- Workers launched with manager ID in environment
- Deterministic, no coordination required

### ✅ 12. Task Group Assignment - Exclusive
**Decision:** One manager executes one task group at a time (exclusive).

**Rationale:**
- Simpler coordination
- Prevents conflicts in env preparation/cleanup
- Similar to current worker-task assignment model
- Sufficient parallelism for most use cases

**Future:** Could add concurrent execution if needed (requires careful coordination)

---

## Remaining Open Questions

### 1. Hook Retry Policy
**Question:** Should env hooks support automatic retries?

**Options:**
- No retries (fail fast)
- Configurable retry count with backoff
- Retry only on specific error codes

**Impact:** Affects reliability but adds complexity. Need to decide retry strategy.

### 2. Task Group State Transitions
**Question:** Should we allow reopening a CLOSED task group?

**Options:**
- A. Allow reopen (CLOSED → OPEN)
- B. Disallow (CLOSED is terminal before COMPLETE)

**Consideration:** Flexibility vs. simplicity

### 3. Manager Health Metrics
**Question:** What metrics should managers report?

**Candidates:**
- Worker spawn count / crash count
- Task throughput
- Resource utilization (CPU, memory)
- Hook execution times

**Impact:** Monitoring and debugging capabilities

### 4. Multi-Manager Coordination (Future)
**Question:** If we support concurrent task group execution by multiple managers, how do we coordinate?

**Challenges:**
- Env prep/cleanup coordination
- Task distribution across managers
- Failure handling with partial completion

**Status:** Deferred - start with exclusive assignment

---

## Implementation Plan

### Phase 1: Core Data Models and APIs
**Goal:** Add database tables and basic CRUD APIs

**Tasks:**
1. Create migration for `task_groups`, `worker_managers`, `group_manager` tables
2. Add SeaORM entity models
3. Implement Task Group CRUD APIs:
   - `POST /task-groups` - Create task group
   - `GET /task-groups/{id}` - Get task group details
   - `PUT /task-groups/{id}` - Update task group
   - `POST /task-groups/{id}/close` - Close task group
   - `DELETE /task-groups/{id}` - Delete task group
4. Implement Manager CRUD APIs:
   - `POST /managers` - Register manager
   - `GET /managers/{id}` - Get manager details
   - `POST /managers/heartbeat` - Manager heartbeat
5. Update task submission API to support `task_group_name`

### Phase 2: Manager Service Implementation
**Goal:** Implement Worker Manager as standalone binary

**Tasks:**
1. Create `netmito-manager` binary crate
2. Implement manager registration and heartbeat
3. Implement task group polling (`GET /managers/task-groups`)
4. Implement worker spawning logic
5. Implement env preparation/cleanup execution
6. Add manager state machine (Idle → Executing → Cleanup → Idle)

### Phase 3: IPC Integration (iceoryx2)
**Goal:** Implement Manager-Worker IPC communication

**Tasks:**
1. Add `iceoryx2` dependency to workspace
2. Design IPC service schema (request-response + pub-sub)
3. Implement manager-side IPC services:
   - Request-response: task fetch, task report, heartbeat
   - Pub-sub: control, config
4. Implement worker-side IPC clients
5. Add worker mode detection (HTTP vs IPC)
6. Implement IPC → HTTP proxying in manager

### Phase 4: Coordinator Extensions
**Goal:** Add task group scheduling and manager queue

**Tasks:**
1. Implement `ManagerQueue` (similar to `TaskDispatcher`)
2. Implement task group assignment logic
3. Implement manager heartbeat monitoring
4. Add task group completion detection
5. Update task distribution to consider `task_group_id`

### Phase 5: Testing and Refinement
**Goal:** End-to-end testing and optimization

**Tasks:**
1. Integration tests for task group lifecycle
2. IPC performance benchmarks
3. Failure scenario testing (manager crash, worker crash, network partition)
4. Documentation and examples
5. Migration guide for existing deployments

---

## Next Steps

### Phase 1: Finalize Remaining Decisions
**Immediate Actions:**
1. ✅ Review and decide on remaining open questions:
   - Hook retry policy
   - Task group state transitions (allow reopen?)
   - Manager health metrics
2. Validate and refine IPC service schema design
3. Review and approve implementation plan phases

### Phase 2: Begin Implementation
**Priority Order:**
1. **Phase 1** (Core Data Models): Database schema, migrations, basic CRUD APIs
2. **Phase 4** (Coordinator Extensions): Task group scheduling, manager queue
3. **Phase 2** (Manager Service): Standalone manager binary
4. **Phase 3** (IPC Integration): iceoryx2 communication
5. **Phase 5** (Testing): End-to-end validation

**Rationale:** Start with database foundation, then coordinator logic, then manager implementation.

### Phase 3: Technical Exploration
**Tasks:**
- Prototype iceoryx2 request-response and pub-sub patterns
- Test CPU core binding mechanisms (taskset, sched_setaffinity)
- Benchmark IPC vs HTTP performance
- Design manager state machine implementation

### Discussion Topics for Next Session
1. **Hook Retry Strategy**: Should hooks retry automatically? How many times?
2. **Task Group Reopening**: Allow CLOSED → OPEN transition?
3. **Metrics & Observability**: What should we expose for monitoring?
4. **Edge Cases**: What happens when manager runs out of disk space? Network partitions?
5. **Testing Strategy**: Unit tests, integration tests, chaos testing?

### Additional Considerations
- **Documentation**: User guide for creating task groups
- **Migration Path**: How do existing users adopt this feature?
- **Performance**: Expected overhead of IPC vs HTTP
- **Security**: Any additional security considerations for managed workers?

---

**Document Version:** 0.2.0
**Last Review:** 2025-11-05
**Status:** Design decisions finalized, ready for implementation planning
**Contributors:** User, Claude
**Next Review:** After remaining open questions are resolved
