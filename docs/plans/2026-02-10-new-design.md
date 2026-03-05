# Design: Task Suite & Agent System

## Context

The mitosis distributed task execution system currently uses independent workers that register with the coordinator, poll for tasks via HTTP, and execute them individually. This has limitations:

- **Memory inefficiency**: `TaskDispatcher` duplicates every task across all eligible workers' in-memory queues — O(workers x tasks)
- **No batch job abstraction**: Related tasks can't share environment setup/cleanup
- **No device-level management**: No coordination across workers on the same machine
- **Polling overhead**: Workers constantly poll even when idle

This design introduces two new first-class concepts:
1. **Task Suite**: A collection of related tasks with shared metadata, hooks, and lifecycle
2. **Agent**: A per-machine service that fetches suites, prepares environments, spawns workers, and executes tasks

Independent workers continue to work unchanged for backward compatibility.

---

## Core Concepts

### Task Suite

A suite groups related tasks with:
- **Shared metadata**: tags, group, worker_schedule, exec_hooks (auto-inherited by tasks)
- **Per-task overrides**: labels, spec (args/envs/resources), priority, timeout
- **Lifecycle hooks**: provision (env setup), cleanup (env teardown), background (sidecar process)
- **State machine**: Open → Closed → Complete (or Cancelled)

### Agent

One agent per physical machine (enforced locally via file lock, not server-side):
- Registers with coordinator, gets JWT token
- Maintains optional WebSocket for real-time notifications
- Sends periodic heartbeats via HTTP
- Fetches suites, runs hooks, spawns workers, reports results
- Workers communicate with agent via iceoryx2 IPC (hidden from coordinator)
- Identified by `machine_code` (from `/etc/machine-id` or generated, stored in `~/.config/mitosis/machine-id`)

### Two-Layer Scheduling

- **High-layer**: Which suite should an agent execute? Suites ordered by `suite.priority` in a priority queue. Agent fetches best available suite when idle. Coordinator pushes `SuiteAvailable` via WS when new suites are submitted.
- **Low-layer**: Which task within a suite to dispatch? Tasks ordered by `task.priority` within each suite's buffer. Agents batch-fetch tasks from coordinator.

---

## Database Schema

### New Tables

#### `machines`
```
id              BIGSERIAL PRIMARY KEY
machine_code    TEXT UNIQUE NOT NULL       -- from /etc/machine-id or generated UUID
hostname        TEXT                        -- last known hostname
metadata        JSONB                       -- OS, hardware info
first_seen_at   TIMESTAMPTZ NOT NULL DEFAULT NOW()
last_seen_at    TIMESTAMPTZ NOT NULL DEFAULT NOW()
```

#### `agents`
```
id                      BIGSERIAL PRIMARY KEY
uuid                    UUID UNIQUE NOT NULL
creator_id              BIGINT NOT NULL REFERENCES users(id)
machine_id              BIGINT REFERENCES machines(id)
tags                    TEXT[] NOT NULL DEFAULT '{}'
labels                  TEXT[] NOT NULL DEFAULT '{}'
state                   INTEGER NOT NULL DEFAULT 0    -- AgentState enum
last_heartbeat          TIMESTAMPTZ NOT NULL DEFAULT NOW()
assigned_task_suite_id  BIGINT REFERENCES task_suites(id)
created_at              TIMESTAMPTZ NOT NULL DEFAULT NOW()
updated_at              TIMESTAMPTZ NOT NULL DEFAULT NOW()
```

#### `task_suites`
```
id                      BIGSERIAL PRIMARY KEY
uuid                    UUID UNIQUE NOT NULL
name                    TEXT
description             TEXT
group_id                BIGINT NOT NULL REFERENCES groups(id)
creator_id              BIGINT NOT NULL REFERENCES users(id)
tags                    TEXT[] NOT NULL DEFAULT '{}'
labels                  TEXT[] NOT NULL DEFAULT '{}'
priority                INTEGER NOT NULL DEFAULT 0
worker_schedule         JSONB NOT NULL          -- WorkerSchedulePlan
exec_hooks              JSONB                   -- ExecHooks {provision, cleanup, background}
state                   INTEGER NOT NULL DEFAULT 0  -- TaskSuiteState enum
last_task_submitted_at  TIMESTAMPTZ
total_tasks             INTEGER NOT NULL DEFAULT 0
pending_tasks           INTEGER NOT NULL DEFAULT 0
created_at              TIMESTAMPTZ NOT NULL DEFAULT NOW()
updated_at              TIMESTAMPTZ NOT NULL DEFAULT NOW()
completed_at            TIMESTAMPTZ
```

#### `group_agent` (join table)
```
id        BIGSERIAL PRIMARY KEY
group_id  BIGINT NOT NULL REFERENCES groups(id)
agent_id  BIGINT NOT NULL REFERENCES agents(id)
role      INTEGER NOT NULL              -- GroupAgentRole: Read=0, Write=1, Admin=2
UNIQUE(group_id, agent_id)
```

#### `task_suite_agent` (join table)
```
id              BIGSERIAL PRIMARY KEY
task_suite_id   BIGINT NOT NULL REFERENCES task_suites(id)
agent_id        BIGINT NOT NULL REFERENCES agents(id)
selection_type  INTEGER NOT NULL          -- UserSpecified=0, TagMatched=1
matched_tags    TEXT[]
creator_id      BIGINT REFERENCES users(id)
created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
UNIQUE(task_suite_id, agent_id)
```

#### `suite_hook_executions` (replaces `suite_execution_failures`)
```
id              BIGSERIAL PRIMARY KEY
task_suite_id   BIGINT NOT NULL REFERENCES task_suites(id)
agent_id        BIGINT NOT NULL REFERENCES agents(id)
hook_type       INTEGER NOT NULL          -- Provision=0, Cleanup=1, Background=2
spec            JSONB NOT NULL            -- ExecSpec (same format as task specs)
state           INTEGER NOT NULL          -- Running=0, Completed=1, Failed=2, Cancelled=3
result          JSONB                     -- TaskResultSpec {exit_status, msg}
started_at      TIMESTAMPTZ
completed_at    TIMESTAMPTZ
created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
```

### Modified Tables

#### `active_tasks` — add columns:
```
task_suite_id   BIGINT REFERENCES task_suites(id) ON DELETE SET NULL
agent_id        BIGINT REFERENCES agents(id) ON DELETE SET NULL
```

#### `archived_tasks` — add columns:
```
task_suite_id   BIGINT    -- snapshot, no FK
agent_id        BIGINT    -- snapshot, no FK
```

---

## State Machines

### TaskSuiteState
```
Open=0      Accepting new tasks, agents can fetch tasks
Closed=1    Not accepting new tasks, agents still fetch remaining
Complete=2  All tasks done (pending_tasks=0). Can reopen if new task submitted.
Cancelled=3 Terminal state.
```

Transitions:
- Create suite → Open
- Open + 3-min inactivity (no new tasks) → Closed
- Open/Closed + pending_tasks reaches 0 → Complete
- Open/Closed/Complete + new task submitted → Open (reopens)
- Any non-cancelled + user cancels → Cancelled (terminal)

### AgentState
```
Idle=0        Waiting for suite. First action: try to fetch a suite.
Provision=1   Running provision hook
Executing=2   Workers running tasks
Cleanup=3     Running cleanup hook
Offline=4     Heartbeat timeout (coordinator-set)
```

Transitions:
- Register → Idle
- Idle + accept suite → Provision
- Provision + hook success → Executing
- Provision + hook failure → Idle (abort suite, try next)
- Executing + suite complete/closed+drained → Cleanup
- Cleanup + done → Idle (immediately try fetch next suite)
- Any state + heartbeat timeout → Offline

---

## Hybrid Task Dispatch: `SuiteTaskDispatcher`

Coordinator-side in-memory structure (separate from existing `TaskDispatcher` for independent workers):

```
SuiteTaskDispatcher:
  suites: HashMap<SuiteId, SuiteTaskBuffer>

SuiteTaskBuffer:
  buffer: PriorityQueue<TaskId, Priority>    -- task.priority ordering
  max_capacity: dynamic                       -- 3 * max(batch_size across agents)
  refill_threshold: dynamic                   -- max(batch_size from any single agent)
```

**Buffer sizing**: When agents accept a suite, their batch_size = worker_count * 2 (from WorkerSchedulePlan). The buffer adapts:
- `refill_threshold = max(batch_size)` across all agents assigned to this suite
- `max_capacity = 3 * refill_threshold`

**Dispatch flow (agent requests N tasks)**:
1. Pop N highest-priority task_ids from suite's buffer
2. For each: atomic DB update `SET state=Running, agent_id=? WHERE id=? AND state=Ready`
3. If update succeeds → include in response with full task spec
4. If update fails (cancelled/already taken) → skip, try next from buffer
5. If buffer < refill_threshold → async refill from DB:
   `SELECT id, priority FROM active_tasks WHERE task_suite_id=? AND state=Ready ORDER BY priority DESC LIMIT max_capacity`

**Key properties**:
- Memory: O(active_suites * buffer_size) — dramatically less than O(workers * tasks)
- Correctness: atomic DB update prevents double-dispatch across multiple agents
- Stale entries: cancelled tasks in buffer are caught by the atomic state check — no eager invalidation needed
- Task IDs only in buffer; full spec loaded from DB at dispatch time

**High-layer suite ordering**: Suites themselves are ordered by `suite.priority` in a separate priority queue for agent assignment.

---

## API Design

### Suite APIs (user-authenticated)

| Method | Path | Description |
|--------|------|-------------|
| POST | /suites | Create suite |
| POST | /suites/query | Query suites with filters |
| GET | /suites/{uuid} | Get suite details + assigned agents |
| DELETE | /suites/{uuid} | Cancel suite |
| POST | /suites/{uuid}/close | Close suite (stop accepting tasks) |
| POST | /suites/{uuid}/agents/refresh | Auto-discover agents by tag matching |
| POST | /suites/{uuid}/agents | Manually add agents |
| DELETE | /suites/{uuid}/agents | Remove agents |

### Agent APIs (user-authenticated)

| Method | Path | Description |
|--------|------|-------------|
| POST | /agents | Register agent (with machine_code, tags, groups) |
| POST | /agents/query | Query agents with filters |
| DELETE | /agents/{uuid} | Shutdown agent (Graceful/Force) |

### Agent APIs (agent-authenticated)

| Method | Path | Description |
|--------|------|-------------|
| POST | /agents/heartbeat | Heartbeat with state, receives missed WS notifications |
| GET | /agents/suite | Fetch best available suite (by suite.priority) |
| POST | /agents/suite/accept | Accept suite → Provision state |
| POST | /agents/suite/start | Provision done → Executing state |
| POST | /agents/suite/complete | Execution done → Cleanup state |
| POST | /agents/suite/cleanup | Cleanup done → Idle state |
| POST | /agents/tasks/fetch | Batch fetch tasks {suite_uuid, count: worker_count*2} |
| POST | /agents/tasks/{uuid}/report | Report task result (Finish/Upload/Commit/Cancel) |

### Task APIs (modified)

| Method | Path | Change |
|--------|------|--------|
| POST | /tasks | Add optional `suite_uuid`. If provided: tags/group auto-inherited from suite. |

### WebSocket

| Path | Description |
|------|-------------|
| GET | /ws/agents | Agent notification channel (binary via speedy) |

---

## Task Inheritance from Suite

When submitting a task with `suite_uuid`:

| Field | Behavior |
|-------|----------|
| tags | Auto-inherited from suite (client must NOT specify) |
| group_id | Auto-inherited from suite |
| priority | Suite default; client can override per-task |
| timeout | Suite default from exec_hooks; client can override |
| labels | Client specifies per-task (not inherited) |
| spec | Client specifies per-task (args, envs, resources) |

---

## WebSocket Notifications (Coordinator → Agent)

| Notification | Trigger | Purpose |
|-------------|---------|---------|
| SuiteAvailable | New suite submitted with matching tags | Tell idle agent to fetch |
| SuiteCancelled | User cancels suite | Tell agent to abort |
| TasksCancelled | User cancels specific tasks | Tell agent to abort tasks |
| PreemptSuite | (Future) Higher-priority suite | Reserved for future preemption |
| Shutdown | Admin shuts down agent | Graceful/force shutdown |
| Ping | Periodic | Keepalive |
| CounterSync | Coordinator restart | Sync notification counter |

**Delivery guarantee**: WS push with ACK. Unacked notifications replayed in heartbeat response.

---

## Cancellation Flow

```
User cancels task → Coordinator: DB state → Cancelled
                  → WS push: TasksCancelled {task_uuids}
                  → Agent ACKs via WS message
                  → Agent forwards to worker via IPC

If no ACK by next heartbeat:
  → Coordinator includes notification in heartbeat response
  → Agent processes and ACKs
```

---

## S3 Artifact Upload (Agent Proxy)

Workers are hidden from coordinator, so S3 presigned URLs are proxied:
```
Worker → IPC: RequestUploadUrl {task_uuid, filename}
Agent  → HTTP: POST /agents/tasks/{uuid}/report {op: Upload {filename}}
Coordinator → returns presigned S3 URL
Agent  → IPC: UploadUrl {url}
Worker → uploads directly to S3
```

---

## Agent Offline / Task Reclamation

When agent heartbeat times out:
1. Mark agent state → Offline
2. All `active_tasks` with `agent_id = ?` AND `state = Running` → transition back to `Ready`, clear `agent_id`
3. Re-add reclaimed task IDs to SuiteTaskBuffer
4. Suite state unaffected (stays Open/Closed)

---

## Backward Compatibility

- **Independent workers**: Unchanged. Use existing `TaskDispatcher` (per-worker queues), HTTP polling, direct registration.
- **Suite tasks**: Use new `SuiteTaskDispatcher` (per-suite buffers), agent-mediated.
- **Task submission**: `suite_uuid` is optional. Without it, task goes to independent worker path.
- **Two dispatchers coexist**: `TaskDispatcher` for independent workers, `SuiteTaskDispatcher` for suite tasks.

---

## Implementation Order (Coordinator-First)

### Phase 1: Schema & Entities
- Create/update migration files for: machines, agents, task_suites, group_agent, task_suite_agent, suite_hook_executions
- Update active_tasks/archived_tasks with agent_id column
- Define all entity models and state enums

### Phase 2: Suite Service & APIs
- Suite CRUD (create, query, get, cancel, close)
- Suite agent assignment (add, remove, refresh)
- Auto-close inactive suites (background task)
- Suite task counter management (increment on submit, decrement on complete)

### Phase 3: Agent Service & APIs
- Agent registration with machine_code
- Agent heartbeat with consistency checks
- Suite fetch/accept/start/complete/cleanup lifecycle
- Agent offline detection and task reclamation

### Phase 4: SuiteTaskDispatcher
- Per-suite priority queue buffers
- Batch fetch API for agents
- Dynamic buffer sizing based on assigned agents
- DB refill on demand
- Task report API (Finish/Upload/Commit/Cancel)

### Phase 5: WebSocket Notifications
- Agent WS router and connection management
- Notification types and ACK protocol
- Heartbeat fallback for missed notifications

### Phase 6: Task Submission Integration
- Modify task submission to support suite_uuid
- Auto-inherit tags/group from suite
- Suite task counter increment on submit

### Phase 7: Agent Client (Fake Implementation)
- Agent binary with full lifecycle state machine
- HTTP + WS communication with coordinator
- Stub/fake: env hooks, worker spawning, task execution
- Real: registration, heartbeat, suite fetch/accept/complete, task fetch/report

---

## Verification

- Unit tests for state machine transitions
- Integration tests: create suite → register agent → agent fetches suite → agent fetches tasks → agent reports results → suite completes
- Test concurrent agents fetching from same suite (no double-dispatch)
- Test agent offline → task reclamation
- Test suite auto-close timeout
- Test WS notification delivery + heartbeat fallback
- `cargo check` / `cargo build` after each phase
