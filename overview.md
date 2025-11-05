# Worker Manager System - Design Overview

**Status:** Design Phase
**Last Updated:** 2025-11-05

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
- **Worker Scheduling Plan**: Defines how many workers to spawn, their configuration
- **Environment Preparation**: Script/procedure to setup the environment before tasks
- **Environment Cleanup**: Script/procedure to teardown after completion
- **State**: Lifecycle state (Open → Closed → Complete)

**Properties:**
- Tasks are submitted **into** a task group (not just to a regular group)
- Task groups have completion criteria (TBD - see [Open Questions](#open-questions))
- Task groups belong to regular Groups (inherit permissions)

### 2. Worker Manager
A **Worker Manager** is a new service component that:

**Responsibilities:**
- Runs on a physical device/server
- Manages local worker processes according to task group scheduling plans
- Controls device-level resources
- Executes environment preparation/cleanup
- Proxies communication between managed workers and coordinator

**Scheduling Behavior:**
- Managers execute **one task group at a time** (exclusive execution)
- After completing a task group, manager picks the next eligible one
- Similar to worker-task scheduling, but at task group granularity

**Authentication:**
- Managers register with coordinator (similar to workers)
- Managers belong to groups with permissions
- Only execute task groups they're authorized for

### 3. Worker Modes

#### Independent Mode (Current - Unchanged)
- Worker registers directly with coordinator
- Polls coordinator HTTP API for tasks
- No local manager involvement

#### Managed Mode (New)
- Worker launched by local Worker Manager
- Communicates with Manager via **iceoryx2 IPC** (not HTTP)
- Manager proxies requests to coordinator on worker's behalf
- Worker lifecycle tied to task group execution

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

    -- Scheduling plan (JSON)
    worker_schedule JSONB NOT NULL,  -- {worker_count, tags, labels, resources}

    -- Lifecycle hooks (JSON)
    env_preparation JSONB,  -- Script/command to run before workers start
    env_cleanup JSONB,      -- Script/command to run after completion

    -- State management
    state INTEGER NOT NULL DEFAULT 0,  -- Open=0, Closed=1, Complete=2

    -- Timestamps
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    completed_at TIMESTAMPTZ,

    -- Assigned manager
    assigned_manager_id BIGINT REFERENCES worker_managers(id),

    UNIQUE(group_id, name)
);
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
    pub worker_schedule: WorkerSchedulePlan,
    pub env_preparation: Option<EnvHook>,
    pub env_cleanup: Option<EnvHook>,
}

#[derive(Serialize, Deserialize)]
pub struct WorkerSchedulePlan {
    pub worker_count: u32,
    pub tags: HashSet<String>,
    pub labels: HashSet<String>,
    pub resources: HashMap<String, String>,  // e.g., {"gpu": "1", "memory": "16GB"}
}

#[derive(Serialize, Deserialize)]
pub enum EnvHook {
    Script { content: String },
    Command { args: Vec<String> },
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
   - `GET /managers/{manager_id}/tasks` - Fetch task for a managed worker
   - `POST /managers/{manager_id}/tasks` - Report task result

### Manager ↔ Worker (iceoryx2 IPC)

#### Request-Response Services

1. **Task Fetching Service**
   ```rust
   // Worker sends request
   struct FetchTaskRequest {
       worker_id: Uuid,
   }

   // Manager responds
   struct FetchTaskResponse {
       task: Option<WorkerTaskResp>,
   }
   ```

2. **Task Reporting Service**
   ```rust
   struct ReportTaskRequest {
       worker_id: Uuid,
       task_id: i64,
       op: ReportTaskOp,
   }

   struct ReportTaskResponse {
       success: bool,
       url: Option<String>,  // For upload operations
   }
   ```

3. **Heartbeat Service**
   ```rust
   struct HeartbeatRequest {
       worker_id: Uuid,
   }

   struct HeartbeatResponse {
       ack: bool,
   }
   ```

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
   - Manager executes `env_preparation` hook
   - If fails, report error, release task group
   - If succeeds, proceed to worker spawning

4. **Worker Spawning**
   - Manager spawns N workers according to `worker_schedule`
   - Workers launched with IPC mode (not HTTP mode)
   - Manager sets up iceoryx2 services (request-response + pub-sub)

5. **Task Execution Loop**
   - Workers request tasks via IPC
   - Manager proxies requests to coordinator
   - Workers execute tasks, report results via IPC
   - Manager proxies results to coordinator

6. **Completion Detection**
   - **OPTION A**: User explicitly closes task group → state = CLOSED
   - **OPTION B**: Timeout-based (no new tasks for X minutes)
   - **OPTION C**: Task count-based (all N tasks completed)
   - When CLOSED + all tasks finished → state = COMPLETE

7. **Environment Cleanup**
   - Manager executes `env_cleanup` hook
   - Shuts down all managed workers (graceful shutdown via pub-sub)
   - Reports completion to coordinator

8. **Next Assignment**
   - Manager returns to idle state
   - Polls for next eligible task group
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

## Open Questions

### 1. Task Group Completion Criteria
**Question:** How should we determine when a task group is "complete"?

**Options:**
- **A. Explicit Close**: User calls API to close task group, no new tasks accepted
  - Pros: Clear user intent
  - Cons: Requires user action, may forget to close

- **B. Timeout-Based**: Auto-close after N minutes of no new task submissions
  - Pros: Automatic, no user action needed
  - Cons: May close prematurely if user slow to submit

- **C. Task Count**: User specifies expected task count upfront
  - Pros: Deterministic completion
  - Cons: Less flexible, need to know count ahead

- **D. Hybrid**: Explicit close OR timeout (whichever first)
  - Pros: Flexible
  - Cons: More complex

**Recommendation:** Start with **Option D (Hybrid)** - support explicit close API, plus configurable timeout as fallback.

### 2. Manager-Coordinator Communication for Task Proxying
**Question:** When manager proxies worker task requests, should it:

**Options:**
- **A. Use manager's JWT**: Manager authenticates on behalf of workers
  - Pros: Simpler, fewer tokens
  - Cons: Manager impersonates workers, audit trail lost

- **B. Issue worker JWTs**: Manager requests individual JWT for each spawned worker
  - Pros: Better audit trail, worker identity preserved
  - Cons: More complex token management

**Recommendation:** **Option B** - Manager requests worker tokens during spawning, better for auditing.

### 3. Environment Preparation/Cleanup Execution
**Question:** Where and how do env hooks execute?

**Options:**
- **A. Manager Process**: Hooks run as subprocess of manager
  - Pros: Simple, manager controls environment
  - Cons: Manager needs execution permissions

- **B. Dedicated Hook Executor**: Separate service executes hooks
  - Pros: Isolation, better security
  - Cons: More complex architecture

**Recommendation:** **Option A** for initial implementation, migrate to B if needed.

### 4. Multiple Managers for Same Task Group
**Question:** Can multiple managers execute the same task group concurrently?

**Options:**
- **A. Exclusive**: One manager at a time (like current worker-task model)
  - Pros: Simpler, no coordination needed
  - Cons: Less parallelism

- **B. Concurrent**: Multiple managers can work on same task group
  - Pros: Higher throughput
  - Cons: Need coordination, may conflict in env prep/cleanup

**Recommendation:** **Option A** initially - exclusive assignment. Task group scheduling should be sufficient for most use cases.

### 5. Worker Identity in Managed Mode
**Question:** Should managed workers register with coordinator?

**Options:**
- **A. No Registration**: Workers are anonymous to coordinator, only manager visible
  - Pros: Simpler, fewer DB entries
  - Cons: No visibility into individual worker status

- **B. Transparent Registration**: Manager registers workers on their behalf
  - Pros: Full visibility, can query worker status
  - Cons: More DB overhead

**Recommendation:** **Option B** - Manager registers workers with special flag (e.g., `managed_by: manager_id`), enables better monitoring.

### 6. Failure Handling
**Question:** What happens if manager crashes during task group execution?

**Scenarios:**
- Manager crashes → managed workers orphaned
- Worker crashes → manager detects via missing heartbeat
- Env preparation fails → abort task group
- Env cleanup fails → log error, mark task group as degraded?

**Recommendation:** Need to design failure recovery:
- Manager heartbeat timeout → coordinator marks manager offline, task group returned to queue
- Orphaned workers → coordinator detects via heartbeat timeout, cleans up
- Failed hooks → configurable retry policy, eventual failure marks task group as failed

### 7. Backward Compatibility and Migration
**Question:** How to ensure existing independent workers continue to work?

**Approach:**
- Independent workers continue using existing HTTP endpoints (`POST /workers`, `GET /workers/tasks`)
- Task submission API extended with optional `task_group_name` field
- Tasks without `task_group_name` → assigned to independent workers (current behavior)
- Tasks with `task_group_name` → assigned to managed workers via managers

### 8. IPC Service Discovery
**Question:** How do workers discover manager's iceoryx2 services?

**Options:**
- **A. Environment Variables**: Manager passes IPC service names via env vars
  - Pros: Simple
  - Cons: Environment pollution

- **B. Config File**: Manager writes config file, workers read
  - Pros: Clean, structured
  - Cons: File I/O, coordination

- **C. Hardcoded Names**: Use predictable service names (e.g., `manager_{manager_id}/task_fetch`)
  - Pros: No coordination needed
  - Cons: Less flexible

**Recommendation:** **Option C** with manager_id in service names - predictable and zero-config.

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

**Immediate Actions:**
1. Review and discuss open questions above
2. Make decisions on:
   - Task group completion criteria
   - Manager-worker token strategy
   - Failure handling approach
3. Validate IPC service schema design
4. Decide on implementation priorities (which phases first?)

**Questions for Discussion:**
- Are there any additional use cases we should consider?
- Should we support dynamic worker scaling (add/remove workers mid-execution)?
- Do we need manager-to-manager communication for future HA scenarios?
- Should task groups support priority levels like tasks?
- How should we handle resource limits (max managers per device, max workers per manager)?

---

**Document Version:** 0.1.0
**Contributors:** Claude (Design), [Your Name]
**Feedback:** Please review and provide input on open questions and design decisions.
