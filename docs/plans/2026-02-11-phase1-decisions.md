# Phase 1: Schema & Entities — Design Decisions & Progress

## Date: 2026-02-11
## Branch: `impl-task-suite`

---

## Design Decisions (Agreed with User)

### 1. `machines` Table — KEEP, but simplified
- Keep the table per the original plan
- Remove `hostname` column — hostname can be part of `metadata` JSONB
- Schema:
  ```
  machines:
    id              BIGSERIAL PRIMARY KEY
    machine_code    TEXT UNIQUE NOT NULL
    metadata        JSONB
    first_seen_at   TIMESTAMPTZ NOT NULL DEFAULT NOW()
    last_seen_at    TIMESTAMPTZ NOT NULL DEFAULT NOW()
  ```

### 2. `agents` Table — Add `machine_id` FK
- Add `machine_id BIGINT REFERENCES machines(id)` to agents table
- Nullable (agent can register before machine is tracked, or for backward compat)

### 3. `suite_hook_executions` Replaces `suite_execution_failures`
- The old design tracked **aggregated failure history** (failure_count, error_messages)
- The new design tracks **individual hook execution attempts** with lifecycle state
- New schema per plan:
  ```
  suite_hook_executions:
    id              BIGSERIAL PRIMARY KEY
    task_suite_id   BIGINT NOT NULL FK(task_suites)
    agent_id        BIGINT NOT NULL FK(agents)
    hook_type       INTEGER NOT NULL          -- HookType: Provision=0, Cleanup=1, Background=2
    spec            JSONB NOT NULL            -- ExecSpec (same format as task specs)
    state           INTEGER NOT NULL          -- HookExecState: Running=0, Completed=1, Failed=2, Cancelled=3
    result          JSONB                     -- TaskResultSpec {exit_status, msg}
    started_at      TIMESTAMPTZ
    completed_at    TIMESTAMPTZ
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
  ```
- New enums: `HookType` (3 values), `HookExecState` (4 values)
- Old enum `SuiteFailureType` is removed

### 4. `runner_id` Replaces `assigned_worker` + `reporter_uuid`
- **Both** `active_tasks` and `archived_tasks` get a new `runner_id: Option<Uuid>` column
- This replaces:
  - `active_tasks.assigned_worker` (was `Option<i64>` — a workers table ID)
  - `archived_tasks.assigned_worker` (was `Option<i64>`)
  - `archived_tasks.reporter_uuid` (was `Option<Uuid>`)
- Semantics:
  - For independent tasks (no suite): `runner_id` = worker UUID
  - For suite tasks: `runner_id` = agent UUID
  - NULL when task is not assigned / not being executed
  - Set when task transitions to Running state
- Migration strategy:
  - For `active_tasks`: look up worker UUID from `workers` table via `assigned_worker` ID; use `00000000-0000-0000-0000-000000000000` if not found
  - For `archived_tasks`: prefer `reporter_uuid` if set; else look up worker UUID via `assigned_worker`; use all-zeros UUID if neither found; set NULL if both `assigned_worker` IS NULL AND `reporter_uuid` IS NULL
  - Drop `assigned_worker` column from both tables
  - Drop `reporter_uuid` column from `archived_tasks`
  - Drop index `idx_archived_tasks-reporter_uuid`

### 5. `group_agent` Table Name Bug — FIX
- Entity file has `table_name = "group_node_agent"` but migration creates `group_agent`
- Fix: change entity to `table_name = "group_agent"`

### 6. Phase 4 APIs — DEFERRED
- `/agents/tasks/fetch` and `/agents/tasks/{uuid}/report` are Phase 4 (SuiteTaskDispatcher)
- Not implementing now

### 7. WebSocket `todo!()` — DEFERRED to Phase 5

---

## Implementation Tasks (Phase 1)

### Task 1: Create `machines` table
- [x] New migration: `m20251117_095000_create_machines.rs`
- [x] New entity: `entity/machines.rs`
- [x] Register in `entity/mod.rs` and `migration/mod.rs`

### Task 2: Add `machine_id` to `agents` table
- [x] Modify migration `m20251117_110000_create_agents.rs` — add `machine_id` column + FK
- [x] Update entity `entity/agents.rs` — add `machine_id: Option<i64>` + relation to machines

### Task 3: Replace `suite_execution_failures` with `suite_hook_executions`
- [x] Rewrite migration `m20251117_130000_create_task_execution_failures.rs` → create `suite_hook_executions`
- [x] Rewrite entity `entity/suite_execution_failures.rs` → `entity/suite_hook_executions.rs`
- [x] Add `HookType` and `HookExecState` enums to `entity/state.rs`
- [x] Remove old `SuiteFailureType` enum
- [x] Update `entity/mod.rs`

### Task 4: Replace `assigned_worker` + `reporter_uuid` with `runner_id`
- [x] Extend migration `m20251117_140000_alter_active_tasks.rs` — add runner_id, migrate data, drop assigned_worker
- [x] Extend migration `m20251117_140001_alter_archived_tasks.rs` — add runner_id, migrate data, drop assigned_worker + reporter_uuid
- [x] Update entities: `active_tasks.rs`, `archived_tasks.rs`
- [x] Update `From<active_tasks::Model> for archived_tasks::Model`

### Task 5: Fix `group_agent` table name
- [x] Change `entity/group_agent.rs` line 8: `"group_node_agent"` → `"group_agent"`

### Task 6: Update entity/mod.rs
- [x] Add `pub mod machines;`
- [x] Rename `suite_execution_failures` → `suite_hook_executions`

---

## Progress Log

### Status: PHASE 1 COMPLETE (2026-02-11)

All Phase 1 tasks completed. `cargo check` passes with 0 warnings.

#### Completed Tasks

1. **Create `machines` table** — Done
   - New file: `migration/m20251117_095000_create_machines.rs`
   - New file: `entity/machines.rs`
   - Registered in `migration/mod.rs`

2. **Add `machine_id` to agents** — Done
   - Modified: `migration/m20251117_110000_create_agents.rs` (added MachineId column + FK)
   - Modified: `entity/agents.rs` (added machine_id field + Machines relation)

3. **Replace `suite_execution_failures` with `suite_hook_executions`** — Done
   - Rewrote: `migration/m20251117_130000_create_task_execution_failures.rs` → creates `suite_hook_executions`
   - New file: `entity/suite_hook_executions.rs` (replaces old `suite_execution_failures.rs`)
   - Added `HookType` and `HookExecState` enums to `entity/state.rs`
   - Removed `SuiteFailureType` enum

4. **Replace `assigned_worker` + `reporter_uuid` with `runner_id`** — Done
   - Extended: `migration/m20251117_140000_alter_active_tasks.rs` (adds runner_id UUID, migrates from assigned_worker via workers table lookup, drops assigned_worker)
   - Extended: `migration/m20251117_140001_alter_archived_tasks.rs` (adds runner_id UUID, migrates from reporter_uuid or assigned_worker, drops both old columns + index)
   - Updated entities: `active_tasks.rs`, `archived_tasks.rs`
   - Updated `From<active_tasks::Model>` conversion
   - Updated all service code: `service/task.rs`, `service/suite.rs`, `service/worker/mod.rs`, `service/s3.rs`
   - Updated schema types: `schema/task.rs`, `schema/artifact.rs`
   - Updated client code: `client/interactive.rs`
   - Updated config: `config/client/tasks.rs`, `config/client/artifacts.rs`
   - Updated API: `api/workers.rs` (pass worker_uuid to fetch_task)

5. **Fix `group_agent` table name** — Done
   - Changed `entity/group_agent.rs`: `"group_node_agent"` → `"group_agent"`

6. **Update `entity/mod.rs`** — Done
   - Added `pub mod machines;`
   - Renamed `suite_execution_failures` → `suite_hook_executions`

#### Files Changed (Phase 1)

**New files:**
- `netmito/src/migration/m20251117_095000_create_machines.rs`
- `netmito/src/entity/machines.rs`
- `netmito/src/entity/suite_hook_executions.rs`

**Deleted files:**
- `netmito/src/entity/suite_execution_failures.rs`

**Modified migration files:**
- `netmito/src/migration/mod.rs`
- `netmito/src/migration/m20251117_110000_create_agents.rs`
- `netmito/src/migration/m20251117_130000_create_task_execution_failures.rs`
- `netmito/src/migration/m20251117_140000_alter_active_tasks.rs`
- `netmito/src/migration/m20251117_140001_alter_archived_tasks.rs`

**Modified entity files:**
- `netmito/src/entity/mod.rs`
- `netmito/src/entity/agents.rs`
- `netmito/src/entity/active_tasks.rs`
- `netmito/src/entity/archived_tasks.rs`
- `netmito/src/entity/group_agent.rs`
- `netmito/src/entity/state.rs`

**Modified service/API files:**
- `netmito/src/service/task.rs`
- `netmito/src/service/suite.rs`
- `netmito/src/service/worker/mod.rs`
- `netmito/src/service/s3.rs`
- `netmito/src/api/workers.rs`

**Modified schema files:**
- `netmito/src/schema/task.rs`
- `netmito/src/schema/artifact.rs`

**Modified client/config files:**
- `netmito/src/client/interactive.rs`
- `netmito/src/config/client/tasks.rs`
- `netmito/src/config/client/artifacts.rs`

---

## Phase 2 & 3 Gap-Fill (2026-02-26) — COMPLETE

### Design Decisions

**Tag/group inheritance**: Server ignores client-provided tags and uses `suite.tags` when
`suite_uuid` is provided. Group is identified from `suite.group_id`, not `group_name` param.

**Agent offline detection**: Priority queue pattern (same as worker `HeartbeatQueue`).
New `AgentHeartbeatQueue` in `service/agent_heartbeat.rs`.

**WS new session**: No notification to replay — just debug log.

**Auto-close timeout**: Now configurable via `suite_auto_close_timeout` in coordinator config
(default 180s / 3 min).

### Completed

1. **WS `todo!()` fix** — `ws/connection.rs`: New sessions log and skip replay (nothing to replay).

2. **Tag/group inheritance** — `service/task.rs`: Suite path now uses `suite.tags.clone()` and
   removes `group_name` filter from the `Group` update query.

3. **Configurable auto-close timeout**:
   - `auto_close_inactive_suites` now takes `timeout_secs: i64` param
   - `CoordinatorConfig` has `suite_auto_close_timeout` (default 180s)
   - `CoordinatorConfigCli` has `--suite-auto-close-timeout`
   - `MitoCoordinator` stores and passes it to the background task

4. **`notify_all_agents_of_restart`** — Now sends `AgentNotification::CounterSync { counter: 0, boot_id: pool.boot_uuid }` to all DB agents via WS router on coordinator startup.

5. **Agent heartbeat priority queue + task reclamation**:
   - New file: `service/agent_heartbeat.rs` — `AgentHeartbeatQueue` + `AgentHeartbeatOp`
   - On timeout: marks agent Offline, reclaims Running tasks (runner_id=agent_uuid → state=Ready, runner_id=NULL)
   - `InfraPool` has `agent_heartbeat_queue_tx: crossfire::MTx<AgentHeartbeatOp>`
   - `CoordinatorConfig` has `agent_heartbeat_timeout` (default 300s)
   - `CoordinatorConfigCli` has `--agent-heartbeat-timeout`
   - `agent_heartbeat` service sends `Heartbeat(agent_id)` op
   - `user_register_agent` sends initial `Heartbeat(agent_id)` op
   - `user_remove_agent_by_uuid` sends `Remove(agent_id)` op

### Files Changed

**New files:**
- `netmito/src/service/agent_heartbeat.rs`

**Modified files:**
- `netmito/src/ws/connection.rs` (WS new session todo fix)
- `netmito/src/service/task.rs` (tag/group inheritance)
- `netmito/src/service/suite.rs` (configurable auto-close timeout)
- `netmito/src/service/agent.rs` (heartbeat ops + CounterSync)
- `netmito/src/service/mod.rs` (register agent_heartbeat module)
- `netmito/src/config/coordinator.rs` (new config fields, InfraPool field, build methods)
- `netmito/src/coordinator.rs` (wire agent heartbeat queue)

---

## Phase 4: SuiteTaskDispatcher + Agent Task APIs (2026-02-27) — COMPLETE

### Design Decisions

**SuiteTaskDispatcher architecture**: Actor pattern (same as `TaskDispatcher`, `HeartbeatQueue`,
`AgentHeartbeatQueue`). Per-suite `PriorityQueue<task_id, priority>` in-memory buffer. HashMap
keyed by suite_id. Ops sent via unbounded crossfire channel.

**Buffer lifecycle**: Lazy initialization — buffer is created on first `AddTask` or `FetchTasks`
(coordinator restart case). Dropped on `DropBuffer` (suite completion).

**Refill strategy**: Eager — when buffer drops below `refill_threshold` (default 16) after a
fetch, a background `tokio::spawn` queries DB for up to `max_capacity` (default 64) Ready tasks
in priority order and sends `AddRefillTasks` to the dispatcher. `is_refilling` flag prevents
duplicate concurrent refills. Always follows with `RefillDone` to clear the flag.

**No RemoveTask op**: Stale cancelled entries in the buffer are silently dropped during fetch:
the atomic DB claim (`WHERE state=Ready AND runner_id IS NULL`) simply returns 0 rows_affected
for cancelled tasks.

**UpdateCapacity**: When an agent accepts a suite, the coordinator sends `UpdateCapacity` with
`batch_size = worker_count * task_prefetch_count` from the suite's `WorkerSchedulePlan`.
The dispatcher uses this to set `max_capacity` and `refill_threshold` for that suite's buffer.

**Suite completion**: On `Commit`, `pending_tasks` is decremented using `exec_with_returning`.
If the returned row has `pending_tasks == 0` and state is `Open` or `Closed`, a conditional
`UPDATE WHERE pending_tasks=0 AND state IN (Open, Closed)` transitions to `Complete` (idempotent
under concurrent commits). Then `DropBuffer` is sent to free the in-memory buffer.

**Cancel op for agents**: Agent cancellation (`ReportTaskOp::Cancel`) behaves like worker
cancellation: marks the task as `Cancelled`. If the task was already `Cancelled` (user-initiated),
the call is a no-op. The agent then follows up with `Commit` to archive it with a result.
Agent-abort/preemption (suite shutdown, heartbeat timeout) is handled by the coordinator:
it resets `state=Ready, runner_id=NULL` and re-adds the task IDs to the dispatcher buffer.

**Submit op**: `ReportTaskOp::Submit` is **rejected** for agent-executed tasks. Returns
`InvalidRequest` with a message explaining this is deferred to a future phase (task chaining).

**Task reclamation on timeout**: `agent_heartbeat.rs` now uses `exec_with_returning` instead of
`exec` when reclaiming Running tasks. The returned models are iterated to send `AddTask` ops to
the suite dispatcher for each task that has `task_suite_id` set.

**agent_id in tables**: NOT needed — `runner_id` (UUID) is sufficient to identify both worker and
agent on active/archived tasks (Phase 1 decision confirmed). No `agent_id` column was added.

**ReportAgentTaskReq simplification**: `task_id` and `task_uuid` fields removed from schema.
The task UUID is provided as a URL path parameter (`/agents/tasks/{uuid}/report`).

### New Files

- `netmito/src/service/suite_task_dispatcher.rs` — `SuiteTaskDispatcher` actor
  - `SuiteTaskBuffer { buffer, max_capacity, refill_threshold, is_refilling }`
  - `SuiteDispatcherOp`: `AddTask`, `AddRefillTasks`, `RefillDone`, `FetchTasks`, `UpdateCapacity`, `DropBuffer`
  - `FetchResult { task_ids, needs_refill, max_capacity }`
- `netmito/src/service/agent_task.rs` — agent task fetch/report service functions
  - `agent_fetch_tasks(agent_id, agent_uuid, pool, suite_uuid, max_count)` → `FetchTasksResp`
  - `agent_report_task(agent_uuid, task_uuid, op, pool)` → `Option<String>` (presigned URL for Upload)
  - `commit_suite_task(pool, suite_id, now)` — decrement counter + optional suite completion
  - `spawn_suite_refill(pool, suite_id, max_capacity)` — background DB refill

### Modified Files

- `netmito/src/service/mod.rs` — added `pub mod agent_task;` and `pub mod suite_task_dispatcher;`
- `netmito/src/config/coordinator.rs` — added `suite_task_dispatcher_tx` to `InfraPool`,
  `build_suite_task_dispatcher()` method, updated `build_infra_pool()` signature
- `netmito/src/coordinator.rs` — added `suite_task_dispatcher` to `MitoCoordinator`, wired channel,
  spawned actor in `run()`
- `netmito/src/service/task.rs` — after suite task insertion, sends `AddTask` to dispatcher
- `netmito/src/api/agents.rs` — added `POST /tasks/fetch` and `POST /tasks/{uuid}/report` routes
  (agent-auth middleware), handler functions call `agent_task::*`
- `netmito/src/service/agent.rs` — `agent_accept_suite`: sends `UpdateCapacity` after accepting;
  added import for `SuiteDispatcherOp`
- `netmito/src/service/agent_heartbeat.rs` — `handle_timeout`: uses `exec_with_returning` to get
  reclaimed tasks; re-adds them via `AddTask` ops; added import for `SuiteDispatcherOp`
- `netmito/src/schema/agent.rs` — simplified `ReportAgentTaskReq` (removed `task_id`, `task_uuid`)

---

---

## Phase 5: WebSocket Keepalive + CounterSync (2026-02-28) — COMPLETE

### Design Decisions

**Ping keepalive — transport-level, per-connection**: axum 0.8 does not have a built-in
automatic ping mechanism (confirmed: it only auto-*responds* to incoming pings). Implemented
as a `tokio::time::interval` inside `handle_agent_socket()`. Each established WS connection
independently sends a `Message::Ping(vec![].into())` frame at the configured interval. The
agent's WS library auto-responds with a `Pong`. This keeps intermediate proxies/load-balancers
from dropping idle connections. Application-level `AgentNotification::Ping` is reserved for
future explicit latency measurement.

**Ping interval — configurable**: `ws_ping_interval: std::time::Duration` added to
`CoordinatorConfig` (default 30s) and stored in `InfraPool` for access from the handler.

**CounterSync via `notify()`**: Uses the existing `AgentWsRouter::notify()` path (buffered,
assigned counter ID). This means even if the agent is not currently WS-connected, it receives
the `CounterSync` via its next heartbeat response. The agent resets its internal counter to
`coordinator_counter` and notes the `boot_id` (different boot_id = coordinator restart).

### Files Changed

- `netmito/src/config/coordinator.rs`
  - Added `ws_ping_interval` field to `CoordinatorConfig` (default 30s, `humantime_serde`)
  - Added `--ws-ping-interval` CLI option to `CoordinatorConfigCli`
  - Added `ws_ping_interval: std::time::Duration` to `InfraPool`
  - `build_infra_pool()` now stores `self.ws_ping_interval` in the pool

- `netmito/src/ws/handler.rs`
  - `handle_agent_socket`: added `tokio::time::interval(pool.ws_ping_interval)` with
    `MissedTickBehavior::Skip`; initial tick consumed immediately (no ping on connect)
  - New select arm: sends `Message::Ping(vec![].into())` on each tick; logs warn and breaks
    on send failure (treating a failed ping as connection dead)

- `netmito/src/service/agent.rs`
  - `agent_heartbeat()` Check 4: removed TODO comment; now calls `AgentWsRouter::notify()`
    with `AgentNotification::CounterSync { counter: coordinator_counter, boot_id: pool.boot_uuid }`
    when `req.last_notification_id > coordinator_counter` is detected

---

---

## Phase 6: WS Notification Gaps (2026-02-28) — COMPLETE

### What Was Missing

The core task submission integration (suite_uuid support, tag/group inheritance, counter
increment) was already done in the Phase 2/3 gap-fill. Phase 6 identified and fixed three
missing WS notification call sites:

1. **Suite cancellation** — `user_cancel_task_suite` had a `// TODO` noting that agents
   were not informed. Fixed: after transaction commit, `SuiteCancelled` is sent to all
   assigned agents and `DropBuffer` is sent to the suite task dispatcher.

2. **Agent assignment** — When agents are newly assigned to a suite (via `user_add_suite_agents`
   or `user_refresh_suite_agents`), idle agents would not know a suite was waiting for them.
   Fixed: after each operation, if the suite has `pending_tasks > 0`, `SuiteAvailable` is
   sent to the newly added agents.

3. **First task submission** — When the first task is submitted to a suite (`pending_tasks`
   transitions 0 → 1, including the reopen-from-Complete case), assigned agents were not
   notified. Fixed: in `internal_submit_task`, if `suite.pending_tasks == 1` after the
   transaction, `notify_agents_suite_available` is called. Subsequent tasks within the same
   suite do NOT re-notify (avoiding buffer flooding for batch submissions).

### Design Decisions

**Only notify on first task**: Sending `SuiteAvailable` on every task submission would flood
the WS buffer for WS-disconnected agents (VecDeque grows without bound before merge). Only
notifying when `pending_tasks` goes from 0 → 1 (first task, or suite reopen) is safe and
sufficient — agents already executing the suite ignore duplicate `SuiteAvailable`.

**Heartbeat fallback still the primary safety net**: The Check 1 in `agent_heartbeat` already
handles "idle agent with available suite" as a fallback. WS push is an optimization for
responsiveness.

**`DropBuffer` on cancel**: Frees the in-memory task buffer so no new fetches are served for
the cancelled suite. Tasks already claimed by agents were Running and are handled by the agent
via `SuiteCancelled` notification + task cancel/commit flow.

### Helper functions added to suite.rs

- `get_assigned_agent_uuids(db, suite_id)` — private; queries `task_suite_agent JOIN agents`
- `notify_agents_suite_available(pool, suite_id, suite_uuid, priority)` — `pub(crate)`
- `notify_agents_suite_cancelled(pool, suite_id, suite_uuid)` — `pub(crate)`

### Files Changed

- `netmito/src/service/suite.rs`
  - New imports: `AgentNotification`, `SuiteDispatcherOp`, `AgentWsRouter`
  - New helpers: `get_assigned_agent_uuids`, `notify_agents_suite_available`,
    `notify_agents_suite_cancelled`
  - `user_cancel_task_suite`: transaction returns `(u64, i64)` (cancelled_count, suite_id);
    calls `notify_agents_suite_cancelled` + `DropBuffer` after commit
  - `user_refresh_suite_agents`: after transaction, if `added_agents` non-empty and
    `suite.pending_tasks > 0`, sends `SuiteAvailable` to each added agent
  - `user_add_suite_agents`: same pattern as refresh

- `netmito/src/service/task.rs`
  - `internal_submit_task` (suite path): transaction return changed from
    `(ActiveTasks::Model, Uuid)` to `(ActiveTasks::Model, Uuid, i32, i32)` adding
    `suite_pending_tasks` and `suite_priority`; calls `notify_agents_suite_available`
    when `suite_pending_tasks == 1`

---

---

## Phase 7: Agent Client (Fake Implementation) (2026-02-28) — COMPLETE

### Design Decisions

**WS channel carries `WsNotificationEvent`** (not `AgentNotification`): The main loop
receives the full event struct so it can advance `self.notification_counter = event.id`.
This means the heartbeat's `last_notification_id` accurately reflects the highest notification
the agent has processed — the coordinator uses this to prune its catch-up buffer.

**Notification counter updated from two sources**:
1. WS events: `self.notification_counter = max(counter, event.id)` in the main select arm
2. Heartbeat catch-up: same `max()` update for each replayed event in `send_heartbeat()`
3. CounterSync: unconditional reset when a coordinator restart is detected (different boot_id)

**`suite_cancelled: bool` field** on `AgentClient`: Set to `true` by `SuiteCancelled`
and `PreemptSuite` handlers when the notification matches the currently assigned suite.
Checked at the top of each task-fetch iteration in `fake_suite_execution`. Cleared when
a new suite is accepted. This gives best-effort cancellation: if the signal arrives during
the blocking task loop it is noticed on the next iteration; for the fake implementation this
is acceptable because each iteration completes almost instantly.

**`TasksCancelled` is handled passively**: Individual task cancellation is detected at
`Commit` time — the coordinator returns an error if the task was already user-cancelled.
No client-side set is maintained; the error path in the Commit arm increments `tasks_failed`.

**Batch size from `WorkerSchedulePlan`**: `worker_count * task_prefetch_count`, matching
the `UpdateCapacity` formula sent to `SuiteTaskDispatcher` in `agent_accept_suite`.
This ensures the client asks for the same batch size that the dispatcher's buffer was
sized to serve.

**Empty-batch retry**: Up to `MAX_EMPTY_RETRIES = 3` retries with 500 ms sleep when
fetch returns empty. This handles the timing gap between task submission and dispatcher
buffer population. After 3 consecutive empty responses the loop exits (suite is drained).

**`enter_cleanup_api` added** (calls `POST /agents/suite/cleanup`): The state transition
sequence is now complete:
```
accept_suite  → state=Provision (DB)
start_suite   → state=Executing (DB)
[task loop]
enter_cleanup → state=Cleanup (DB)
[cleanup hooks]
complete_suite → state=Idle (DB)
```
Previously the agent skipped `enter_cleanup`, going directly from Executing to calling
`complete_suite`. The DB state never passed through Cleanup.

**Real task counts passed to `complete_suite`**: `tasks_completed` and `tasks_failed`
are now tracked in the task loop and passed to `CompleteSuiteReq` instead of both being 0.

### Files Changed

- `netmito/src/agent.rs`
  - `AgentClient` struct: added `suite_cancelled: bool` field
  - `run_agent`: initialised `suite_cancelled: false`
  - `run()`: channel changed from `mpsc::channel::<AgentNotification>` to
    `mpsc::channel::<WsNotificationEvent>`; main select arm advances `notification_counter`
  - `spawn_websocket_client` / `websocket_connect`: parameter type updated to
    `mpsc::Sender<WsNotificationEvent>`; `notification_tx.send(event)` (full event)
  - `send_heartbeat`: advances `notification_counter` from each replayed heartbeat event
  - `handle_notification`:
    - `SuiteCancelled`: sets `self.suite_cancelled = true` (was TODO)
    - `PreemptSuite`: sets `self.suite_cancelled = true` (was TODO for preemption)
    - `TasksCancelled`: comment explains passive detection (no client tracking needed)
  - `process_state` Idle arm:
    - resets `suite_cancelled = false` on new suite
    - changed `fake_suite_execution` call signature (now `&mut self`, returns `(u64, u64)`)
    - added `enter_cleanup_api()` call between execution and cleanup
    - passes real `(tasks_completed, tasks_failed)` to `complete_suite`
    - resets `suite_cancelled = false` after clearing `assigned_suite_uuid`
  - New method: `enter_cleanup_api()` — `POST /agents/suite/cleanup`, no body
  - New method: `fetch_tasks(suite_uuid, max_count)` — `POST /agents/tasks/fetch`
  - New method: `report_task(task_uuid, op)` — `POST /agents/tasks/{uuid}/report`
  - `fake_suite_execution`: rewrote; now `&mut self`, returns `(u64, u64)`;
    real fetch/report loop (Finish → Commit); batch_size from WorkerSchedulePlan;
    empty-batch retry; suite_cancelled check per iteration

---

## Remaining Work Audit (2026-02-28)

All 7 planned phases are complete. `cargo check` passes with 0 warnings.
The following items were identified as not yet implemented:

### Priority 1: Client CLI for Suites and Agents (HIGH)

`config/client/mod.rs` `ClientCommand` enum has no `Suites` or `Agents` variants.
All server-side APIs exist; only the client-side CLI wiring is missing.

**Suites subcommand needed:**
- `create-suite` → `POST /suites`
- `query-suites` → `POST /suites/query`
- `get-suite` → `GET /suites/{uuid}`
- `cancel-suite` → `DELETE /suites/{uuid}`
- `close-suite` → `POST /suites/{uuid}/close`
- `refresh-suite-agents` → `POST /suites/{uuid}/agents/refresh`
- `add-suite-agents` → `POST /suites/{uuid}/agents`
- `remove-suite-agents` → `DELETE /suites/{uuid}/agents`

**Agents subcommand needed:**
- `register-agent` → `POST /agents`
- `query-agents` → `POST /agents/query`
- `shutdown-agent` → `DELETE /agents/{uuid}`

Implementation pattern: follow the existing `Workers` subcommand in
`config/client/workers.rs` and handler in `client/mod.rs`.

### Priority 2: `machines` Table Integration (MEDIUM)

The `machines` table and `entity/machines.rs` exist but are never populated.
- `RegisterAgentReq` has no `machine_code` field
- `user_register_agent` never upserts into `machines`
- `agents.machine_id` is always NULL

**What needs to be done:**
1. Add `machine_code: Option<String>` to `RegisterAgentReq`
2. In `user_register_agent`: if `machine_code` is provided, upsert into `machines`
   (INSERT ... ON CONFLICT (machine_code) DO UPDATE SET last_seen_at=NOW())
   and set `agents.machine_id` to the resulting ID
3. Add `machine_code: Option<String>` to `AgentConfig` and `AgentConfigCli`
   (read from `/etc/machine-id` or `~/.config/mitosis/machine-id` if not specified)

### Priority 3: Agent `Shutdown` Notification (SMALL)

`agent.rs` `handle_notification` `Shutdown` arm has `// TODO: Implement shutdown logic`.
Fix: call `cancel_token.cancel()` — but the token is owned by `run()`, not accessible
from `handle_notification`. Options:
- Store a `CancellationToken` clone in `AgentClient`
- Or use a `tokio::sync::watch` boolean channel

### Priority 4: Force-Cancel Running Tasks (SMALL)

`service/suite.rs` `user_cancel_task_suite`:
> `// TODO: currently we do not use 'force' to cancel running tasks`

Currently both Graceful and Force only cancel `Ready` tasks. Force mode should also
cancel `Running` tasks (UPDATE active_tasks SET state=Cancelled WHERE
task_suite_id=? AND state=Running) and notify the assigned agents via WS
(`TasksCancelled` notification).

### Priority 5: Downstream Task Triggering for Suite Tasks (MEDIUM)

`service/worker/mod.rs` line ~716:
> `// TODO: should support task_suite in the future`

When a worker completes a task with `downstream_task_uuid`, `worker_trigger_pending_task`
is called. This doesn't handle the case where the downstream task belongs to a suite
(it would need to add the task to the `SuiteTaskDispatcher` buffer).

### Priority 6: Code-Quality TODOs (LOW)

| File | Line | Note |
|------|------|------|
| `ws/handler.rs` | `handle_agent_message` | Add more WS message handling beyond Ack |
| `ws/connection.rs` | `get_notifications` | Deduplicate notifications before returning |
| `schema/agent.rs` | `AgentNotification` | Review enum definition when design firms up |
| `schema/suite.rs` | `WorkerSchedulePlan` | Finalize scheduling plan variants |

### Priority 7: Integration / End-to-End Tests (LARGE)

No automated tests exist. The design doc calls for:
- Full lifecycle: create suite → register agent → fetch suite → execute tasks → suite completes
- Concurrent agents: no double-dispatch when two agents fetch from same suite
- Agent offline: heartbeat timeout → task reclamation → re-added to dispatcher
- Suite auto-close: inactivity timeout transitions Open → Closed
- WS delivery + heartbeat fallback: agent receives notifications with and without WS

---

---

## Phase 8: Client CLI for Suites and Agents (2026-02-28) — COMPLETE

### Design Decisions

**Two new config modules**: `config/client/suites.rs` and `config/client/agents.rs` following the
exact same pattern as `config/client/workers.rs` (`*Args` structs with clap derives + `From` impls
to convert into request types).

**ValueEnum added to `TaskSuiteState` and `AgentState`** in `entity/state.rs` to allow these
enums to be used as clap CLI filter arguments (same pattern as `TaskState` which already had
`ValueEnum`).

**`CreateSuiteArgs` exposes `WorkerSchedulePlan::FixedWorkers` fields directly** (`--workers`,
`--prefetch`) rather than accepting raw JSON. `exec_hooks` is omitted for now (complex to express
on CLI; should be done via file in a future phase).

**Remove-agents uses POST** (`/suites/{uuid}/agents/remove`) rather than DELETE with query params.
The DELETE variant (`remove_suite_agents_params`) requires URL-encoding the UUID list which is
awkward from CLI; the POST variant with JSON body is simpler.

**`output_suite_info` / `output_suite_list_info` / `output_agent_info`** added to
`client/interactive.rs` following the same `tracing::info!` pattern as `output_worker_info`.

### Files Changed

**Modified:**
- `netmito/src/entity/state.rs` — added `ValueEnum` to `TaskSuiteState` and `AgentState`
- `netmito/src/config/client/mod.rs` — added `suites`/`agents` modules + `Suites`/`Agents`
  variants to `ClientCommand`
- `netmito/src/client/http.rs` — added 11 new HTTP methods (suites: create/query/get/cancel/
  close/refresh_agents/add_agents/remove_agents; agents: register/query/shutdown)
- `netmito/src/client/interactive.rs` — added `output_suite_list_info`, `output_suite_info`,
  `output_agent_info`
- `netmito/src/client/mod.rs` — added 11 wrapper methods (`suites_*`, `agents_*`) + full
  `ClientCommand::Suites` and `ClientCommand::Agents` dispatch arms in `handle_command`

**New files:**
- `netmito/src/config/client/suites.rs` — `SuitesArgs`, `SuitesCommands`, `CreateSuiteArgs`,
  `QuerySuitesArgs`, `GetSuiteArgs`, `CancelSuiteArgs`, `CloseSuiteArgs`,
  `RefreshSuiteAgentsArgs`, `AddSuiteAgentsArgs`, `RemoveSuiteAgentsArgs`
- `netmito/src/config/client/agents.rs` — `AgentsArgs`, `AgentsCommands`, `RegisterAgentArgs`,
  `QueryAgentsArgs`, `ShutdownAgentArgs`

### Remaining Priority Items (updated)

P1 (Suites/Agents CLI) — **DONE**.
P3 (Agent Shutdown notification) — **DONE** (Phase 9A, 2026-02-28).
P4 (Force-cancel running tasks) — **DONE** (Phase 9B, 2026-02-28).

P2, P5, P6, P7 remain; see sections above for details.

---

## Phase 9: P3 + P4 Small Fixes (2026-02-28) — COMPLETE

### Phase 9A: Agent Shutdown Notification (P3)

**Design**: Added `shutdown_token: tokio_util::sync::CancellationToken` to `AgentClient`.
Initialized in `run_agent()` with `CancellationToken::new()`. In `run()`, the local
`cancel_token` is now `self.shutdown_token.clone()` instead of a fresh token — so any
cancellation (SIGINT, WS Shutdown notification) cancels the same underlying token and
exits the main loop.

**Files changed**: `netmito/src/agent.rs`
- `AgentClient` struct: added `shutdown_token: tokio_util::sync::CancellationToken`
- `run_agent`: initializes `shutdown_token: CancellationToken::new()` in struct literal
- `run()`: `let cancel_token = self.shutdown_token.clone()` (instead of creating new)
- `handle_notification` Shutdown arm: `self.shutdown_token.cancel()` (was TODO)

### Phase 9B: Force-Cancel Running Tasks (P4)

**Design**: `user_cancel_task_suite` now uses the `op` parameter (was `_op`/unused).
When `op == Force`, Running tasks for the suite are archived immediately (same logic as
Ready/Pending tasks) and `TasksCancelled` WS notifications are sent per-agent after the
transaction commits. The existing `SuiteCancelled` broadcast still runs for all cases.

**Implementation notes**:
- Transaction return type changed from `(u64, i64)` to `(u64, i64, Vec<(Option<Uuid>, Uuid)>)`
  where the Vec holds `(runner_id, task_uuid)` for force-cancelled Running tasks
- After commit: group by `runner_id` → `HashMap<Uuid, Vec<Uuid>>`; send one
  `TasksCancelled { task_uuids }` per agent
- `runner_id = None` Running tasks are archived but not notified (can't happen in practice)
- Cancellation result JSON is shared between both Ready/Pending and Running task archives

**Files changed**: `netmito/src/service/suite.rs`
- `user_cancel_task_suite`: renamed `_op` → `op`; added `force` flag; added Running task
  query + archive block inside the transaction; changed return tuple; added post-commit
  `TasksCancelled` notification loop using `std::collections::HashMap`

---

## Phase 10: Machines Table Integration (P2) (2026-02-28) — COMPLETE

### Design Decisions

**Auto-detection**: When `--machine-code` is not provided on CLI or in config, `run_agent`
reads `/etc/machine-id`, trims whitespace, and uses that as the machine code. Explicit
`--machine-code` overrides this. If neither is available (e.g., macOS), `machine_code = None`
and `agents.machine_id` stays NULL.

**Upsert via ON CONFLICT**: Uses sea_orm `insert().on_conflict(...).exec_with_returning()`.
The conflict target is `machine_code` (UNIQUE). On conflict, only `last_seen_at` is updated
(preserving `first_seen_at`). The returned row's `id` is used as `agents.machine_id`.

**`machine_code` in `RegisterAgentReq` is optional** (`#[serde(default)]`), so existing
clients that don't send it are unaffected and the agent's `machine_id` remains NULL.

### Files Changed

- `netmito/src/schema/agent.rs` — added `machine_code: Option<String>` to `RegisterAgentReq`
- `netmito/src/config/agent.rs` — added `machine_code: Option<String>` to `AgentConfig` and
  `AgentConfigCli` (`--machine-code` flag); Default impl includes `machine_code: None`
- `netmito/src/config/client/agents.rs` — added `--machine-code` to `RegisterAgentArgs`;
  `From<RegisterAgentArgs>` passes it through to `RegisterAgentReq`
- `netmito/src/agent.rs` — `run_agent`: resolves `machine_code` via `config.machine_code.take()
  .or_else(|| read /etc/machine-id)`; logs the resolved code; passes it in `RegisterAgentReq`
- `netmito/src/service/agent.rs` — added `machines as Machines` entity import + `OnConflict`
  to sea_query imports; `user_register_agent` extracts `machine_code`; if `Some`, upserts into
  `machines` table and captures the ID; creates agent with `machine_id: Set(machine_id)`

---

## Next Session Starting Point

**Suggested next phase: P7 (integration tests)**

For P7 (integration tests):
- Full lifecycle test: create suite → register agent → submit tasks → agent fetches/reports → complete
- Concurrent agents: verify no double-dispatch
- Heartbeat timeout → reclamation → re-dispatch

---

## Phase 11: Downstream Tasks for Suite Tasks (P5) (2026-02-28) — COMPLETE

### Design

`worker_trigger_pending_task` is called when a worker completes a task that has a
`downstream_task_uuid`. The downstream task was created with `state=Pending` and is now
promoted to `Ready`. Previously the function always dispatched to the **worker** task
dispatcher, which is wrong for suite tasks.

**Key insight**: when `internal_submit_task` creates a Pending suite task, it speculatively
calls `SuiteDispatcherOp::AddTask` immediately. However the fetch claim (`UPDATE WHERE
state=Ready`) fails silently for Pending tasks, consuming the buffer entry. So by the time
the task is triggered (set to Ready), the buffer entry is gone and no refill will find it
unless the buffer happens to be below threshold.

The fix: in `worker_trigger_pending_task`, after `UPDATE state=Ready`, branch on
`task.task_suite_id`:
- `Some(suite_id)` → send `SuiteDispatcherOp::AddTask { suite_id, task_id, priority }`
- `None` → original `TaskDispatcherOp::BatchAddTask` to worker queues

`SuiteDispatcherOp` was already imported in `service/task.rs`. The PriorityQueue handles
duplicate task_id entries safely (push-or-update semantics, no double-dispatch).

### Files Changed

- `netmito/src/service/task.rs` — `worker_trigger_pending_task`: refactored the dispatch
  block into `if let Some(suite_id) = task.task_suite_id { … } else { … }` branches
- `netmito/src/service/worker/mod.rs` — removed the `// TODO: should support task_suite`
  comment (the fix is now in `worker_trigger_pending_task` itself)
