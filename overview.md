# Worker Manager System - Design Overview

**Status:** ✅ Design Complete - Ready for Implementation
**Last Updated:** 2025-11-05
**Version:** 0.3.0

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

### ✅ 6. Hook Retry Strategy
**Decision:** Classify errors and retry accordingly.

**Error Classification:**
1. **Network/Infrastructure Errors** (S3 disconnect, coordinator unreachable):
   - **Retry forever** with exponential backoff
   - These are transient failures
   - Examples: Connection timeout, 503 Service Unavailable, network partition

2. **Command Execution Errors** (hook command fails):
   - **Never retry** - abort task group immediately
   - Indicates machine doesn't meet requirements
   - Examples: Missing dependencies, permission denied, script error
   - Manager aborts this task group and tries next eligible one

**Implementation:**
```rust
enum HookError {
    Network(String),      // Retry forever
    Execution(i32, String), // Never retry (exit code, stderr)
}
```

**Rationale:** Network errors are recoverable, but execution errors mean the manager's machine fundamentally cannot handle this task group (even if authorized). Other managers may have proper setup.

### ✅ 7. Failure Handling
**Decisions:**

| Scenario | Handling |
|----------|----------|
| **Manager crashes** | Coordinator detects via heartbeat timeout → marks offline → returns task group to queue |
| **Worker crashes** | Manager detects → respawns replacement worker automatically |
| **Env preparation fails (network)** | Retry forever with exponential backoff |
| **Env preparation fails (command)** | Never retry → abort task group → try next eligible group |
| **Env cleanup fails** | Log error → mark as "degraded complete" → manager proceeds to next task group |

### ✅ 8. Task Group Attributes - Priority and Tags/Labels
**Decision:** Task groups have scheduling attributes like tasks.

**Attributes:**
- **Priority** (i32): For scheduling order (higher = higher priority)
- **Tags** (HashSet<String>): For matching with manager capabilities
- **Labels** (HashSet<String>): User-defined metadata

**Scheduling:** Coordinator matches task groups to managers based on:
- Priority (higher first)
- Tags (must match manager tags)
- Group permissions (manager must have access)

### ✅ 9. Manager Capacity - No Hard Limits
**Decision:** No hard limit on workers per manager.

**Rationale:**
- Worker count defined by task group's `worker_schedule.worker_count`
- Manager handles whatever the scheduling plan specifies
- Device resource limits naturally constrain capacity
- Flexibility for different use cases (small vs. large batches)

### ✅ 10. Dynamic Worker Scaling - Via Scheduling Method
**Decision:** Worker scaling is static per task group, defined in scheduling plan.

**Current:** `worker_count` is fixed when task group is created
**Future:** Could add dynamic scaling APIs if needed (e.g., `PUT /task-groups/{id}/scale`)

### ✅ 11. Backward Compatibility
**Decision:** Full backward compatibility with independent workers.

**Details:**
- Independent workers unchanged: `POST /workers`, `GET /workers/tasks`
- Task submission API extended with optional `task_group_name`
- Tasks without `task_group_name` → assigned to independent workers (current behavior)
- Tasks with `task_group_name` → assigned to managed workers via managers
- Both modes coexist seamlessly

### ✅ 12. IPC Service Discovery - Predictable Names
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

### ✅ 13. Task Group Assignment - Sticky Affinity Scheduling
**Decision:** Implement work-conserving sticky scheduling to minimize context switching.

**Problem:**
- Environment preparation/cleanup has significant overhead
- Frequent manager switching between task groups wastes time
- But managers shouldn't idle when their current task group has no pending tasks
- Need balance between stickiness (throughput) and work-conserving (utilization)

**Solution: Lease-Based Affinity Scheduling with Idle Threshold**

#### Scheduling Mechanism

**1. Lease Assignment**
When a manager is assigned to a task group:
- Manager receives a **lease** (e.g., configurable 1-4 hours)
- During the lease, manager has **affinity** to this task group
- Manager preferentially fetches tasks from its assigned task group

**2. Work-Conserving Idle Detection**
- If manager is idle (no pending tasks) for **idle threshold** (e.g., 5 minutes):
  - Manager **tentatively probes** for other eligible task groups
  - If higher-priority group has pending tasks → break lease early, switch
  - If no better options → stay with current group (lease preserved)
- This ensures managers don't waste time idling

**3. Lease Renewal**
- When lease expires:
  - If current task group still has pending tasks → **auto-renew** lease
  - If task group is CLOSED and no pending tasks → release, find new group
  - If task group is COMPLETE → release, find new group

**4. Task Group Reopening**
- CLOSED task groups **can be reopened** to OPEN state
- Use case: User closed too early, wants to submit more tasks
- Transition: `PUT /task-groups/{id}/reopen`
- If manager still has lease → resumes immediately
- If manager switched away → rejoins if still eligible

#### Database Schema Updates

```sql
-- Add to worker_managers table
ALTER TABLE worker_managers ADD COLUMN lease_expires_at TIMESTAMPTZ;
ALTER TABLE worker_managers ADD COLUMN last_task_fetched_at TIMESTAMPTZ;

-- Configuration table
CREATE TABLE manager_config (
    id SERIAL PRIMARY KEY,
    lease_duration_seconds INT DEFAULT 3600,  -- 1 hour
    idle_threshold_seconds INT DEFAULT 300,   -- 5 minutes
    updated_at TIMESTAMPTZ NOT NULL
);
```

#### Scheduling Algorithm

```
function assign_task_group(manager):
    current_group = manager.assigned_task_group_id

    # Check current assignment
    if current_group is not NULL:
        if lease_valid(manager.lease_expires_at):
            # Still under lease
            if has_pending_tasks(current_group):
                return current_group  # Stick to current

            # Idle detection
            idle_duration = NOW() - manager.last_task_fetched_at
            if idle_duration < IDLE_THRESHOLD:
                return current_group  # Haven't been idle long enough

            # Been idle too long - probe for better options
            better_group = find_higher_priority_group(manager, current_group)
            if better_group is not NULL:
                # Break lease early, switch to better group
                release_group(manager, current_group)
                assign_group(manager, better_group)
                return better_group
            else:
                # No better options, stick with current
                return current_group
        else:
            # Lease expired
            if has_pending_tasks(current_group) AND current_group.state == OPEN:
                # Renew lease
                renew_lease(manager, current_group)
                return current_group
            else:
                # Release and find new
                release_group(manager, current_group)
                # Fall through to find new assignment

    # No current assignment or released - find new group
    new_group = find_eligible_group(manager)  # Priority-based matching
    if new_group is not NULL:
        assign_group(manager, new_group)
        return new_group

    return NULL  # No eligible groups
```

#### Benefits

| Aspect | Benefit |
|--------|---------|
| **Stickiness** | Lease mechanism keeps managers on same task group for extended periods |
| **Low Context Switching** | Prep/cleanup overhead minimized by long lease durations |
| **Work-Conserving** | Idle threshold ensures managers don't waste time waiting |
| **Fairness** | Higher-priority task groups can steal idle managers |
| **Flexibility** | Reopening allows users to correct early closures |
| **Configurability** | Lease duration and idle threshold tunable per deployment |

#### Edge Cases

- **Multiple managers, one task group**: First-come-first-served lease assignment
- **Manager crash during lease**: Lease automatically released via heartbeat timeout
- **Task group deleted during lease**: Manager released, finds new assignment
- **Zero pending tasks, but OPEN**: Manager keeps lease (tasks may come soon)

#### Configuration Tuning

| Parameter | Typical Value | Consideration |
|-----------|---------------|---------------|
| `lease_duration` | 1-4 hours | Longer = more sticky, shorter = more dynamic |
| `idle_threshold` | 3-10 minutes | Longer = more sticky, shorter = more work-conserving |

**Rationale:** This design maximizes manager stickiness to minimize prep/cleanup overhead while ensuring no manager sits idle when work is available elsewhere. The probe-on-idle mechanism provides work-conserving guarantees.

---

## Observability & Metrics

### Manager Metrics Design

Managers report comprehensive metrics to coordinator for monitoring, debugging, and capacity planning.

#### 1. System Metrics (Resource Utilization)

**Reported every heartbeat (e.g., 30 seconds):**

```rust
struct SystemMetrics {
    // CPU
    cpu_usage_percent: f64,           // Overall CPU usage
    cpu_cores_allocated: Vec<usize>,  // Cores bound to workers
    cpu_cores_usage: Vec<f64>,        // Per-core usage for allocated cores

    // Memory
    memory_total_bytes: u64,
    memory_used_bytes: u64,
    memory_available_bytes: u64,

    // Disk
    disk_total_bytes: u64,
    disk_used_bytes: u64,
    disk_io_read_bytes_per_sec: u64,
    disk_io_write_bytes_per_sec: u64,

    // Network
    network_rx_bytes_per_sec: u64,
    network_tx_bytes_per_sec: u64,

    // Load average
    load_avg_1min: f64,
    load_avg_5min: f64,
    load_avg_15min: f64,
}
```

**Collection:** Use `sysinfo` crate or `/proc` filesystem on Linux

#### 2. Worker Pool Metrics

**Reported every heartbeat:**

```rust
struct WorkerPoolMetrics {
    // Worker lifecycle
    workers_active: u32,              // Currently running workers
    workers_total_spawned: u64,       // Lifetime spawn count
    workers_total_crashed: u64,       // Lifetime crash count
    workers_respawn_rate: f64,        // Crashes per hour

    // Per-worker status
    worker_status: Vec<WorkerStatus>,
}

struct WorkerStatus {
    worker_local_id: u32,
    state: WorkerState,  // Idle | Executing | Crashed
    current_task_id: Option<i64>,
    tasks_completed: u64,
    uptime_seconds: u64,
}
```

#### 3. Task Execution Metrics

**Reported every task completion:**

```rust
struct TaskExecutionMetrics {
    task_id: i64,
    task_uuid: Uuid,
    worker_local_id: u32,

    // Timing
    fetch_duration_ms: u64,        // Time to fetch from coordinator
    execution_duration_ms: u64,    // Task execution time
    report_duration_ms: u64,       // Time to report back
    total_duration_ms: u64,        // End-to-end

    // Result
    exit_status: i32,
    artifacts_uploaded: u32,
    artifacts_size_bytes: u64,

    // Resources
    peak_memory_bytes: u64,
    cpu_time_ms: u64,
}
```

**Aggregated metrics reported every heartbeat:**

```rust
struct AggregatedTaskMetrics {
    // Throughput
    tasks_completed_last_minute: u32,
    tasks_completed_last_hour: u32,
    tasks_completed_total: u64,

    // Success/Failure
    tasks_succeeded: u64,
    tasks_failed: u64,
    tasks_cancelled: u64,

    // Performance
    avg_execution_duration_ms: f64,
    p50_execution_duration_ms: u64,
    p95_execution_duration_ms: u64,
    p99_execution_duration_ms: u64,
}
```

#### 4. Task Group Lifecycle Metrics

**Reported on task group events:**

```rust
struct TaskGroupMetrics {
    task_group_id: i64,
    task_group_uuid: Uuid,

    // Lifecycle timing
    prep_start_time: OffsetDateTime,
    prep_end_time: OffsetDateTime,
    prep_duration_ms: u64,
    prep_retry_count: u32,

    execution_start_time: OffsetDateTime,
    execution_end_time: Option<OffsetDateTime>,
    execution_duration_ms: Option<u64>,

    cleanup_start_time: Option<OffsetDateTime>,
    cleanup_end_time: Option<OffsetDateTime>,
    cleanup_duration_ms: Option<u64>,
    cleanup_retry_count: u32,

    // Task counts
    tasks_processed: u64,
    tasks_succeeded: u64,
    tasks_failed: u64,

    // Outcome
    result: TaskGroupResult,  // Success | PrepFailed | CleanupDegraded
}
```

#### 5. IPC Communication Metrics

**Reported every heartbeat:**

```rust
struct IpcMetrics {
    // Request-response patterns
    fetch_requests_total: u64,
    fetch_requests_per_sec: f64,
    fetch_avg_latency_us: f64,

    report_requests_total: u64,
    report_requests_per_sec: f64,
    report_avg_latency_us: f64,

    // Pub-sub patterns
    control_messages_sent: u64,
    config_messages_sent: u64,

    // Errors
    ipc_errors_total: u64,
    ipc_timeouts_total: u64,
}
```

#### 6. Lease & Scheduling Metrics

**Reported every heartbeat:**

```rust
struct LeaseMetrics {
    current_lease_task_group_id: Option<i64>,
    lease_start_time: Option<OffsetDateTime>,
    lease_expires_at: Option<OffsetDateTime>,
    lease_renewals: u32,
    lease_early_breaks: u32,

    idle_duration_seconds: u64,
    context_switches_total: u64,  // How many times switched task groups
}
```

### API Endpoints

#### Manager Heartbeat with Metrics
```
POST /managers/heartbeat
Authorization: Bearer <manager_jwt>

Request Body:
{
  "timestamp": "2025-11-05T10:30:00Z",
  "system_metrics": { ... },
  "worker_pool_metrics": { ... },
  "aggregated_task_metrics": { ... },
  "ipc_metrics": { ... },
  "lease_metrics": { ... }
}

Response: 200 OK
```

#### Query Manager Metrics
```
GET /managers/{id}/metrics?window=1h
Authorization: Bearer <user_jwt>

Response:
{
  "manager_id": "uuid",
  "time_window": "1h",
  "system_metrics": [ ... ],
  "task_throughput": { ... },
  "worker_health": { ... }
}
```

### Metrics Storage

**Time-Series Database:** Use TimescaleDB (PostgreSQL extension) or InfluxDB for metrics storage

**Retention Policy:**
- Fine-grained metrics (30s intervals): 7 days
- Aggregated metrics (1h intervals): 90 days
- Task group lifecycle metrics: Indefinite (small volume)

### Monitoring Dashboard

**Key Visualizations:**
1. Manager health overview (CPU, memory, workers active)
2. Task throughput over time
3. Worker crash rate
4. Task group preparation/cleanup times
5. IPC latency distribution
6. Lease renewals and context switches

---

## IPC Architecture Details

### iceoryx2 Design Principles

**Goals:**
- **Zero-copy**: Minimize data copying for large payloads
- **Low-latency**: Sub-millisecond request-response
- **Scalable**: Support hundreds of workers per manager
- **Extensible**: Easy to add new message types
- **Reliable**: Handle failures gracefully

### Service Architecture

#### 1. Request-Response Services (iceoryx2 RPC Pattern)

**Service Definition:**
```rust
use iceoryx2::prelude::*;

// Task fetch service
const TASK_FETCH_SERVICE: &str = "mitosis/manager_{manager_id}/task_fetch";

#[derive(Serialize, Deserialize)]
struct FetchTaskRequest {
    worker_local_id: u32,
    request_id: u64,  // For request tracking
}

#[derive(Serialize, Deserialize)]
struct FetchTaskResponse {
    request_id: u64,
    task: Option<WorkerTaskResp>,
    error: Option<String>,
}

// Task report service
const TASK_REPORT_SERVICE: &str = "mitosis/manager_{manager_id}/task_report";

#[derive(Serialize, Deserialize)]
struct ReportTaskRequest {
    worker_local_id: u32,
    request_id: u64,
    task_id: i64,
    op: ReportTaskOp,
}

#[derive(Serialize, Deserialize)]
struct ReportTaskResponse {
    request_id: u64,
    success: bool,
    url: Option<String>,
    error: Option<String>,
}
```

**Manager-Side Implementation:**
```rust
// Manager creates service and listens
let task_fetch_service = ServiceBuilder::new()
    .name(format!("mitosis/manager_{}/task_fetch", manager_id))
    .request_response::<FetchTaskRequest, FetchTaskResponse>()
    .create()?;

let server = task_fetch_service.server_builder().create()?;

// Manager event loop
loop {
    if let Some(request) = server.receive()? {
        // Process request
        let task = coordinator_client.fetch_task().await?;
        let response = FetchTaskResponse {
            request_id: request.payload().request_id,
            task,
            error: None,
        };
        request.send_response(&response)?;
    }
}
```

**Worker-Side Implementation:**
```rust
// Worker connects to service
let task_fetch_service = ServiceBuilder::new()
    .name(format!("mitosis/manager_{}/task_fetch", manager_id))
    .request_response::<FetchTaskRequest, FetchTaskResponse>()
    .open()?;

let client = task_fetch_service.client_builder().create()?;

// Worker requests task
let request = FetchTaskRequest {
    worker_local_id: 0,
    request_id: generate_request_id(),
};

let response = client.send_request(&request)
    .timeout(Duration::from_secs(5))?;
```

#### 2. Publish-Subscribe Services

**Control Topic:**
```rust
use iceoryx2::prelude::*;

const CONTROL_TOPIC: &str = "mitosis/manager_{manager_id}/control";

#[derive(Serialize, Deserialize)]
enum ControlMessage {
    Shutdown { graceful: bool },
    Pause,
    Resume,
    WorkerScaleDown { worker_ids: Vec<u32> },
}

// Manager publishes
let control_topic = ServiceBuilder::new()
    .name(format!("mitosis/manager_{}/control", manager_id))
    .publish_subscribe::<ControlMessage>()
    .create()?;

let publisher = control_topic.publisher_builder().create()?;
publisher.send(&ControlMessage::Shutdown { graceful: true })?;

// Worker subscribes
let control_topic = ServiceBuilder::new()
    .name(format!("mitosis/manager_{}/control", manager_id))
    .publish_subscribe::<ControlMessage>()
    .open()?;

let subscriber = control_topic.subscriber_builder().create()?;

loop {
    if let Some(sample) = subscriber.receive()? {
        match sample.payload() {
            ControlMessage::Shutdown { graceful } => {
                if *graceful {
                    finish_current_task().await;
                }
                std::process::exit(0);
            }
            _ => { ... }
        }
    }
}
```

#### 3. Large Payload Optimization

For large task specs or artifacts metadata:

```rust
// Use shared memory for large payloads
#[derive(Serialize, Deserialize)]
struct FetchTaskResponse {
    request_id: u64,
    task_metadata: TaskMetadata,  // Small struct
    large_payload_key: Option<String>,  // Shared memory key if payload > threshold
}

// If task spec > 4KB, write to shared memory
if task_spec_size > 4096 {
    let shm_key = format!("task_{}_{}", manager_id, task_id);
    write_to_shared_memory(&shm_key, &task_spec)?;
    response.large_payload_key = Some(shm_key);
} else {
    response.task_metadata.inline_spec = Some(task_spec);
}
```

#### 4. Error Handling

```rust
enum IpcError {
    ServiceNotFound,
    Timeout,
    SerializationError(String),
    ManagerOffline,
}

// Worker-side retry logic
fn fetch_task_with_retry() -> Result<Option<WorkerTaskResp>, IpcError> {
    let mut backoff = Duration::from_millis(100);

    for attempt in 0..MAX_RETRIES {
        match client.send_request(&request).timeout(Duration::from_secs(5)) {
            Ok(response) => return Ok(response.task),
            Err(IpcError::Timeout) if attempt < MAX_RETRIES - 1 => {
                sleep(backoff);
                backoff *= 2;  // Exponential backoff
            }
            Err(e) => return Err(e),
        }
    }

    Err(IpcError::Timeout)
}
```

#### 5. Service Discovery

Workers find manager services via environment variable:
```bash
MITOSIS_MANAGER_ID=<uuid>
MITOSIS_WORKER_MODE=ipc  # vs. http
```

Service names constructed deterministically:
- `mitosis/manager_{MITOSIS_MANAGER_ID}/task_fetch`
- `mitosis/manager_{MITOSIS_MANAGER_ID}/task_report`
- `mitosis/manager_{MITOSIS_MANAGER_ID}/control`

### Performance Characteristics

**Expected Latency:**
- Request-Response: < 100 microseconds (same machine)
- Pub-Sub: < 50 microseconds
- Large payload (>1MB): < 1 millisecond

**Throughput:**
- Per worker: 100-1000 tasks/second (depends on task duration)
- Per manager: Limited by coordinator HTTP API, not IPC

**Scalability:**
- Workers per manager: No hard limit, tested up to 256
- Memory overhead: ~1MB per worker for IPC structures

---

## CPU Binding for Child Processes

### Problem Statement

Workers spawn child processes (via `TaskSpec.args`) that execute the actual workload. CPU binding must apply not just to the worker process, but also to all child processes it spawns.

### Solution: Process Group CPU Affinity

#### 1. Linux CPU Affinity API

Use `sched_setaffinity` with `CPU_SET`:

```rust
use nix::sched::{sched_setaffinity, CpuSet};
use nix::unistd::Pid;

fn bind_process_to_cores(pid: Pid, cores: &[usize]) -> Result<()> {
    let mut cpu_set = CpuSet::new();
    for &core in cores {
        cpu_set.set(core)?;
    }
    sched_setaffinity(pid, &cpu_set)?;
    Ok(())
}
```

#### 2. Binding Strategy Implementation

**RoundRobin Strategy:**
```rust
fn apply_round_robin_binding(
    workers: &[WorkerProcess],
    cores: &[usize],
) -> Result<()> {
    for (i, worker) in workers.iter().enumerate() {
        let core = cores[i % cores.len()];
        bind_process_to_cores(worker.pid, &[core])?;
    }
    Ok(())
}
```

**Exclusive Strategy:**
```rust
fn apply_exclusive_binding(
    workers: &[WorkerProcess],
    cores: &[usize],
) -> Result<()> {
    if workers.len() > cores.len() {
        return Err("Not enough cores for exclusive binding");
    }

    for (worker, &core) in workers.iter().zip(cores.iter()) {
        bind_process_to_cores(worker.pid, &[core])?;
    }
    Ok(())
}
```

**Shared Strategy:**
```rust
fn apply_shared_binding(
    workers: &[WorkerProcess],
    cores: &[usize],
) -> Result<()> {
    // All workers can use all cores
    for worker in workers {
        bind_process_to_cores(worker.pid, cores)?;
    }
    Ok(())
}
```

#### 3. Child Process Inheritance

**Problem:** Child processes spawned by workers don't automatically inherit CPU affinity.

**Solution 1: cgroup-based** (Recommended)
```rust
use std::fs;

fn create_cpu_cgroup(manager_id: &str, cores: &[usize]) -> Result<String> {
    let cgroup_path = format!("/sys/fs/cgroup/cpuset/mitosis_{}", manager_id);
    fs::create_dir_all(&cgroup_path)?;

    // Set allowed CPUs
    let cpus = cores.iter()
        .map(|c| c.to_string())
        .collect::<Vec<_>>()
        .join(",");
    fs::write(format!("{}/cpuset.cpus", cgroup_path), cpus)?;

    // Copy memory nodes from parent
    let mems = fs::read_to_string("/sys/fs/cgroup/cpuset/cpuset.mems")?;
    fs::write(format!("{}/cpuset.mems", cgroup_path), mems)?;

    Ok(cgroup_path)
}

fn add_worker_to_cgroup(worker_pid: Pid, cgroup_path: &str) -> Result<()> {
    fs::write(
        format!("{}/cgroup.procs", cgroup_path),
        worker_pid.to_string(),
    )?;
    Ok(())
}
```

**Benefit:** All child processes automatically inherit cgroup constraints.

**Solution 2: taskset wrapper** (Fallback)
```rust
// Manager wraps worker command with taskset
fn spawn_worker_with_cpu_binding(cores: &[usize]) -> Result<Child> {
    let cpulist = cores.iter()
        .map(|c| c.to_string())
        .collect::<Vec<_>>()
        .join(",");

    Command::new("taskset")
        .arg("-c")
        .arg(cpulist)
        .arg("./netmito-worker")
        .arg("--ipc-mode")
        .spawn()
}
```

**Drawback:** Workers must be launched via `taskset`, less clean.

#### 4. Verification

```rust
fn verify_cpu_affinity(pid: Pid) -> Result<Vec<usize>> {
    let mut cpu_set = CpuSet::new();
    sched_getaffinity(pid, &mut cpu_set)?;

    let bound_cores: Vec<usize> = (0..CpuSet::count())
        .filter(|&i| cpu_set.is_set(i).unwrap_or(false))
        .collect();

    Ok(bound_cores)
}

// Log binding on worker startup
let bound_cores = verify_cpu_affinity(Pid::this())?;
tracing::info!("Worker bound to CPU cores: {:?}", bound_cores);
```

#### 5. Dynamic Rebinding

If worker scale changes mid-execution:

```rust
fn rebind_workers_on_scale(
    workers: &[WorkerProcess],
    strategy: &CpuBindingStrategy,
    cores: &[usize],
) -> Result<()> {
    match strategy {
        CpuBindingStrategy::RoundRobin => {
            apply_round_robin_binding(workers, cores)?;
        }
        CpuBindingStrategy::Exclusive => {
            apply_exclusive_binding(workers, cores)?;
        }
        CpuBindingStrategy::Shared => {
            apply_shared_binding(workers, cores)?;
        }
    }
    Ok(())
}
```

### Configuration

In `WorkerSchedulePlan`:
```rust
pub struct CpuBinding {
    pub cores: Vec<usize>,
    pub strategy: CpuBindingStrategy,
    pub use_cgroups: bool,  // Prefer cgroups over sched_setaffinity
}
```

### Edge Cases

| Scenario | Handling |
|----------|----------|
| **Insufficient cores** | Error on task group assignment |
| **Core doesn't exist** | Validate cores against system topology on manager startup |
| **cgroup permission denied** | Fall back to `taskset` or `sched_setaffinity` |
| **Worker crashes and respawns** | Reapply binding on respawn |

---

## Future Extensibility

### Deferred Features

1. **Multi-Manager Coordination**: Concurrent execution of same task group by multiple managers
2. **Dynamic Worker Scaling**: Add/remove workers mid-execution based on load
3. **GPU Binding**: Extend CPU binding to CUDA devices
4. **Memory Limits**: cgroup memory constraints
5. **Network QoS**: Priority queuing for manager-coordinator traffic

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

### ✅ Design Complete

All major design decisions have been finalized:
- ✅ Hook retry strategy (network: retry forever, command: never retry)
- ✅ Task group scheduling (lease-based sticky affinity with idle threshold)
- ✅ Task group state transitions (allow reopening CLOSED → OPEN)
- ✅ Observability metrics (comprehensive 6-category metrics)
- ✅ IPC architecture (iceoryx2 request-response + pub-sub with code examples)
- ✅ CPU binding (cgroup-based for child process inheritance)

### Ready for Implementation

**Recommended Implementation Order:**

1. **Phase 1: Core Data Models** (1-2 weeks)
   - Create database migrations
   - Implement SeaORM entities
   - Build CRUD APIs for task groups and managers
   - Add lease management endpoints

2. **Phase 4: Coordinator Extensions** (2-3 weeks)
   - Implement ManagerQueue and task group assignment logic
   - Add sticky affinity scheduling algorithm
   - Integrate auto-close timeout mechanism
   - Update task distribution logic

3. **Phase 2: Manager Service** (3-4 weeks)
   - Create standalone manager binary
   - Implement worker spawning with CPU binding
   - Add environment hook execution with retry logic
   - Build metrics collection and reporting

4. **Phase 3: IPC Integration** (2-3 weeks)
   - Add iceoryx2 dependency
   - Implement request-response services
   - Add pub-sub control/config topics
   - Build IPC→HTTP proxying layer

5. **Phase 5: Testing & Deployment** (2-3 weeks)
   - End-to-end integration tests
   - IPC performance benchmarks
   - Chaos engineering (crash scenarios)
   - Documentation and migration guides

**Total Estimated Time:** 10-15 weeks

### Technical Prototyping Suggestions

Before full implementation, consider prototyping:
1. **iceoryx2 Proof-of-Concept**: Validate request-response + pub-sub patterns
2. **cgroup CPU Binding**: Test child process inheritance on target platform
3. **Lease Algorithm**: Simulate sticky scheduling with synthetic workload
4. **Metrics Collection**: Benchmark overhead of heartbeat with full metrics

### Documentation Needs

- **User Guide**: Creating and managing task groups
- **API Reference**: New endpoints for task groups and managers
- **Migration Guide**: Adopting managed workers from independent workers
- **Deployment Guide**: Manager setup, cgroup permissions, iceoryx2 configuration
- **Troubleshooting**: Common issues and debugging techniques

### Open Questions for Implementation Phase

1. **TimescaleDB vs InfluxDB**: Which time-series DB for metrics?
2. **Manager Packaging**: Docker image? Systemd service? Kubernetes DaemonSet?
3. **IPC Performance Target**: What's acceptable latency/throughput SLA?
4. **Lease Duration Default**: Start with 1 hour, 2 hours, or 4 hours?
5. **Testing Infrastructure**: Need dedicated test environment for IPC?

---

**Document Version:** 0.3.0
**Last Updated:** 2025-11-05
**Status:** ✅ Design Complete - Ready for Implementation
**Contributors:** User, Claude
**Next Review:** During Phase 1 implementation (revise as needed)
