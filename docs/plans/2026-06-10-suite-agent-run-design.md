# Design: Suite Agent Runs

**Date:** 2026-06-10
**Status:** Draft
**Scope:** Introduce `suite_agent_runs` table to track each attempt of an agent running a task suite; repoint `suite_hook_executions` from (suite, agent) to a run; add user-facing query/detail/delete APIs for run history.

## Motivation

Today `suite_hook_executions` references `(task_suite_id, agent_id)` directly. An agent can attempt the same suite multiple times (provision failure + retry, suite reopened, agent crash and recovery), and all hook rows from those attempts pile up under the same pair with no way to distinguish "provision of attempt 3" from attempt 2's. Run-level facts (outcome, phase reached, task counts) currently have no home — `agent_complete_suite` logs `tasks_completed`/`tasks_failed` via tracing and discards them.

This follows the standard CI hierarchy: pipeline → job run → steps. The run is the missing middle layer:

- Hook executions of one attempt group naturally under a run (FK + cascade delete).
- Run-level state gets one durable row instead of being inferred from hook rows or logs.
- Retention becomes trivial: delete old terminal runs, hook rows cascade. This resolves the expiry TODO at the top of `entity/suite_hook_executions.rs`.

## Identity model (decided)

Agent UUID is **not** the machine id: `user_register_agent` mints `Uuid::new_v4()` per registration. Machine continuity lives in the `machines` table, keyed by `machine_code` (auto-detected from `/etc/machine-id`, upserted on registration, never deleted). Agent rows are deleted on shutdown (`remove_agent`), so a run row must not depend on the agent row surviving.

Decision: denormalize **both identities** onto the run at accept time:

- `agent_uuid: Uuid` — plain column, no FK. Survives agent deletion; identifies the specific registration/session.
- `machine_id: i64` — FK → `machines`, `on_delete = Restrict`. Machines are append-only, so this is the durable anchor for "which physical box ran this". Post-mortem query is `suite_agent_runs JOIN machines` regardless of agent lifecycle. NOT NULL because machine_code is now mandatory at registration (see below), so every agent has a real machines row to copy from at accept time.

### Machine code is always present (decided 2026-06-11)

`machine_code` is required end-to-end, eliminating the old `agents.machine_id = 0` sentinel (no backward compatibility needed — nothing in production uses the machines table yet):

- **Agent process (`mito agent`)**: `resolve_machine_code` in `agent.rs` always produces one. Precedence: explicit config override → cached value at `<config_dir>/mitosis/machine-id` → `/etc/machine-id` → freshly generated UUID. The resolved value (unless explicitly overridden) is persisted to the cache file, so identity stays stable across restarts even where `/etc/machine-id` is unavailable (containers, non-Linux hosts). Mirrors the credential cache in `service/auth/cred.rs`.
- **CLI (`mito client agents register`)**: `--machine-code` is a required argument. This one-shot command registers an agent identity on behalf of *another* host (it prints a token and exits; nothing in this repo consumes that token — it exists for external/custom agent implementations), so auto-detecting the operator's machine would record wrong data. The resolver stays private to `agent.rs`; no shared module needed.
- **Server**: `RegisterAgentReq.machine_code` is `String` (required); registration rejects empty values and always upserts the machines row.

Caveat: agent rows created *before* this change may still carry `machine_id = 0`; they cannot start runs (FK would reject) and must re-register. Acceptable since agents are ephemeral registrations.
- `agent_id: Option<i64>` — FK → `agents`, `on_delete = SetNull`. Convenience join while the agent is alive.

Rejected alternative: deriving agent UUID deterministically from machine-id (uuid5 of `machine_code`). Breaks multiple agents per machine and clean re-registration (the `agents.uuid` unique constraint would collide if a machine re-registers before its old agent row is reaped). The ephemeral-registration vs. durable-machine split stays as is.

## Schema

### New table: `suite_agent_runs`

| column | type | notes |
|---|---|---|
| `id` | i64 PK | internal, used by FKs |
| `task_suite_id` | i64 | FK → task_suites, Cascade |
| `run_id` | i32 | ascending **per suite** (across agents); **UNIQUE(task_suite_id, run_id)** |
| `agent_id` | Option\<i64> | FK → agents, SetNull |
| `agent_uuid` | Uuid | plain column, survives agent deletion |
| `machine_id` | i64 | FK → machines, Restrict; always present since machine_code is mandatory at registration |
| `state` | SuiteRunState | see below |
| `tasks_completed` | i32 | incremented live as tasks commit; reconciled at completion |
| `tasks_failed` | i32 | incremented live as tasks commit; reconciled at completion |
| `failure_reason` | Option\<Json> | `{ kind, message }` — see "Failure reason semantics" below |
| `created_at` | timestamptz | accept time |
| `started_at` | Option\<timestamptz> | provision done, execution began |
| `finished_at` | Option\<timestamptz> | reached terminal state |
| `updated_at` | timestamptz | |

`run_id` allocation: `COALESCE(MAX(run_id), 0) + 1` per suite inside the accept transaction, with the unique index as safety net (retry on conflict). Multiple agents can accept the same suite concurrently, but contention is negligible at realistic agent counts.

Decided: `run_id` is per-suite (not per `(suite, agent)`), so users can view all attempts on a suite in one numbered sequence without picking an agent first.

### `SuiteRunState`

```rust
pub enum SuiteRunState {
    Provision = 0,   // accepted, provision hook running
    Executing = 1,   // tasks being executed
    Cleanup   = 2,   // cleanup hook running
    Completed = 3,   // terminal: success
    Failed    = 4,   // terminal: failed (failure_reason has phase + cause)
    Cancelled = 5,   // terminal: suite cancelled mid-run
    Lost      = 6,   // terminal: agent heartbeat timeout / removed mid-run
}
```

The non-terminal states mirror `AgentState` phases. `Lost` prevents zombie "running" rows when an agent dies. Single enum + `failure_reason` is preferred over separate phase/outcome columns; the state at failure time tells us which phase failed.

### Failure reason semantics

`failure_reason` is a **one-line, run-level summary** of why the run terminated abnormally — never the logs themselves. Lightly structured Json so the list view is filterable:

```json
{ "kind": "ProvisionFailed",    "message": "provision hook exited with code 1" }
{ "kind": "BackgroundExited",   "message": "background hook exited early (signal 9)" }
{ "kind": "ExecutionError",     "message": "worker pool crashed" }
{ "kind": "CleanupFailed",      "message": "cleanup hook exited with code 1" }
{ "kind": "AgentLost",          "message": "heartbeat timeout" }
{ "kind": "SuiteCancelled",     "message": "suite cancelled by user" }
```

The actual stdout/stderr/exit code of the provision or background process lives in the `suite_hook_executions` row for this run (`result` column, or S3 pointer for large output). Debugging flow: run list shows `failure_reason` → `GET .../runs/{run_id}` → read the related hook execution's `result`.

### Live task counters

The agent reports tasks **individually**, not in batch: `ReportTaskOp::Finish` then `ReportTaskOp::Commit(result)` per task (see `agent_report_task`), and `commit_suite_task` already updates suite-level counters per commit. The run's `tasks_completed`/`tasks_failed` are incremented on that same Commit path (run located via task's `task_suite_id` + the reporting agent's current non-terminal run). `agent_complete_suite` then writes the agent's own final tallies as the authoritative reconcile, so transient drift (lost requests, retries) self-corrects at run end.

### Changes to `suite_hook_executions`

- Replace `task_suite_id` + `agent_id` columns with a single `suite_agent_run_id` FK (Cascade).
- Suite/agent are reachable by join; keeping them denormalized invites drift.
- Replace the existing `(suite, agent)` indexes with an index on `suite_agent_run_id`.

### Indexes

- `UNIQUE(task_suite_id, run_id)` on suite_agent_runs
- `(state, finished_at)` — serves the query API and future TTL sweeps
- `agent_uuid` — "all runs by this registration" lookups

## Lifecycle wiring (coordinator)

| event | run effect |
|---|---|
| `agent_accept_suite` | create run (state `Provision`), return run handle in `AcceptSuiteResp` |
| `agent_start_suite` | → `Executing`, set `started_at` |
| `agent_enter_cleanup` | → `Cleanup` |
| task `Commit` report | increment `tasks_completed`/`tasks_failed` (live counters) |
| `agent_complete_suite` | → `Completed` (or `Failed`), reconcile task counts, `finished_at` |
| heartbeat timeout / `mark_agent_offline` / `remove_agent` | any non-terminal run → `Lost` |
| suite cancelled | in-flight runs → `Cancelled` |

**Gap to fill (decided, deferred to task list):** there is currently no API for an agent to report run *failure* (e.g. provision hook failed). Plan: extend `CompleteSuiteReq` with an outcome field (`Completed | Failed { reason }`) — one terminal endpoint is easier to keep consistent than a separate `/suite/fail` route.

**Agent protocol:** `AcceptSuiteResp` returns the run identifier; the agent echoes it in start/cleanup/complete/hook reports. Explicit beats inferred — it survives coordinator restarts and lets the server reject stale reports from a previous attempt (report for run 3 arriving after run 4 started).

Hook reporting endpoints (when real hooks are implemented) write `suite_hook_executions` rows referencing the run.

## Replacing `agents.assigned_task_suite_id` with `current_run_id` (decided: yes, as follow-up step)

Pros:

- **Single source of truth.** Today the agent's assignment and the run table could disagree. With `current_run_id` the invariant "agent busy ⇔ `current_run_id` set ⇔ that run is non-terminal" is checkable; the suite is derivable by join.
- **Strictly richer.** Points at *which attempt*, not just which suite — stale-report rejection becomes a trivial id compare (a report for run 3 arriving after run 4 started is rejected).
- **Retention-safe.** Deletes only touch terminal runs; `current_run_id` only ever points at non-terminal ones, so SetNull never fires in practice.

Cons:

- **More code churn.** Every "which suite is this agent on" lookup becomes a join: `fetch_specific_suite`, `agent_start_suite`'s filter, `remove_agent`, dispatcher capacity update.
- **Bidirectional FKs** between `agents` and `suite_agent_runs` (run→agent SetNull, agent→run SetNull). Fine in Postgres, but needs care with migration ordering and distinct SeaORM relation names.

Decision: do it, but as its own migration + refactor step *after* the run lifecycle is wired, so each change stays reviewable.

## User-facing API

Follows house style (POST `/query` with body filters, group-permission middleware like other suite routes). Supports the debugging flow: list outcomes → drill into one run → bulk delete old records.

```
POST /suites/{uuid}/runs/query
  body: { agent_uuid?: Uuid, states?: [SuiteRunState],
          created_before?: ts, created_after?: ts, limit?, offset? }
  → run summaries: { run_id, agent_uuid, machine_code, state,
      tasks_completed, tasks_failed, failure_reason,
      created_at, started_at, finished_at }

GET /suites/{uuid}/runs/{run_id}
  → full detail: run summary + hook executions
    (hook_type, state, spec, result, started_at, completed_at)

DELETE /suites/{uuid}/runs
  body: { run_id_lt?: i32, agent_uuid?: Uuid, created_before?: ts }
  → { deleted_count }
```

Delete semantics:

- At least one filter required — a bare DELETE must not wipe everything.
- Only **terminal** runs are deleted; in-flight runs (`Provision`/`Executing`/`Cleanup`) are skipped.
- Supports the target use cases: "delete runs older than May 1st" (`created_before`), "delete runs with id < 10" (`run_id_lt`, now suite-wide since run_id is per-suite).

## Retention

- Manual: the DELETE endpoint above.
- Automatic (future): a small tokio interval job in the coordinator deleting terminal runs older than N days (configurable), using the `(state, finished_at)` index. Hook executions cascade.

## Storage notes

- Keep large hook outputs (provision logs) out of the `result` Json column. Anything beyond a few KB should go to S3 via the existing attachment machinery, with the row storing a pointer.

## Open questions

1. Run identifier in the agent protocol: internal `id` vs. `(suite_uuid, run_id)` pair (leaning: internal id is fine; it's scoped by agent auth).

Resolved:

- ~~Outcome field vs. dedicated fail endpoint~~ → extend `CompleteSuiteReq` (task 4).
- ~~`assigned_task_suite_id` → `current_run_id`?~~ → yes, as a separate follow-up step (task 5).

## Tasks

- [x] **1a. Machine code always present.** `resolve_machine_code` (config override → cache → `/etc/machine-id` → generated, persisted to `<config_dir>/mitosis/machine-id`) in `agent.rs`; `RegisterAgentReq.machine_code` required; CLI `agents register` requires `--machine-code`; server validates non-empty and always upserts machines; `suite_agent_runs.machine_id` is NOT NULL.
- [x] **1. Entity + migration.** Added `SuiteRunState` to `entity/state.rs` and `RunFailureReason`/`RunFailureKind` to `schema/agent.rs`; created `suite_agent_runs` entity; updated `suite_hook_executions` entity/relations. Migration `m20260611_100000_create_suite_agent_runs`: creates `suite_agent_runs` (with `agent_uuid`, NOT NULL `machine_id` FK), alters `suite_hook_executions` (drops `task_suite_id`/`agent_id` after clearing unattributable rows, adds `suite_agent_run_id` FK Cascade), indexes (`UNIQUE(task_suite_id, run_id)`, `(state, finished_at)`, `agent_uuid`, run-scoped partial active index).
- [ ] **2. Run lifecycle wiring.** Create run in `agent_accept_suite` (transactional max+1 `run_id` allocation, retry on conflict); transitions in `agent_start_suite` / `agent_enter_cleanup` / `agent_complete_suite`; mark `Lost` in heartbeat-timeout and agent-removal paths; mark `Cancelled` on suite cancellation.
- [ ] **3. Live task counters.** Increment `tasks_completed`/`tasks_failed` in the `agent_report_task` Commit path; reconcile with agent-reported totals in `agent_complete_suite`.
- [ ] **4. Failure reporting.** Extend `CompleteSuiteReq` with `outcome: Completed | Failed { reason }`; return run handle in `AcceptSuiteResp` and echo it in start/cleanup/complete reports (reject stale-run reports).
- [ ] **5. `agents.current_run_id`.** Separate migration replacing `assigned_task_suite_id`; refactor `fetch_specific_suite`, `agent_start_suite`, `remove_agent`, dispatcher capacity paths to join via run.
- [ ] **6. User API.** `POST /suites/{uuid}/runs/query`, `GET /suites/{uuid}/runs/{run_id}`, `DELETE /suites/{uuid}/runs` under `suites_router` with group-permission middleware.
- [ ] **7. Agent client.** Carry the run handle through the suite lifecycle calls; report hook executions against the run.
- [ ] **8. (Later) TTL sweep.** Coordinator interval job deleting terminal runs older than N days (configurable).
