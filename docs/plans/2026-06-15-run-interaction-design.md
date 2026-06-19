# Design: Suite Agent Run — Interaction Layer

**Date:** 2026-06-15 (consolidated 2026-06-16)
**Status:** Part A (agent ↔ coordinator protocol) — **DECIDED, ready to implement.** Part B (user-facing API) — pending design.
**Scope:** Wire the `suite_agent_runs` row into (A) the agent↔coordinator protocol and (B) the user↔coordinator query API.
**Builds on:** `2026-06-10-suite-agent-run-design.md` (entity + migration, committed in `f05c37d`).

> This is a self-contained implementation spec. Each numbered rule is a decision; read Part A top-to-bottom and implement it. There is intentionally no separate "decision log" — the prose *is* the decision record.

---

## Starting point

Already built & committed: `suite_agent_runs` entity, `SuiteRunState` enum, `RunFailureReason`/`RunFailureKind`, repointed `suite_hook_executions`, the migration. Table verified in the live DB.

Not yet wired: **nothing creates or transitions run rows**; the agent lifecycle and handlers are unchanged from before the table existed. Suite hooks are still `fake_*` in the agent — so the `/hook` endpoint (A.3/A.6) is forward-looking wiring that pairs with real hooks later; the run *lifecycle, counters, validation, and reclaim* (A.1, A.2, A.5) are fully implementable now.

`SuiteRunState` (entity `state.rs`): `Provision=0, Executing=1, Cleanup=2, Completed=3, Failed=4, Cancelled=5, Lost=6` (+ future `Preempted=7`). Non-terminal: `Provision`/`Executing`/`Cleanup`. Terminal: the rest.

### Code map (implementation anchors)

- **Agent lifecycle loop:** `netmito/src/agent.rs:608` (`process_state`, `Idle` arm) → `accept_suite`→`start_suite`→`enter_cleanup_api`→`complete_suite` (client methods `agent.rs:721+`).
- **Coordinator handlers:** `netmito/src/api/agents.rs:119+`, behind `agent_auth_middleware`, receive `Extension<AuthAgent>` = `{ id: i64, uuid: Uuid }` (`service/auth/mod.rs:53`).
- **Coordinator services:** `netmito/src/service/agent.rs` — `agent_accept_suite:725`, `agent_start_suite:805`, `agent_complete_suite:828`, `agent_enter_cleanup:870`, `remove_agent:884`, `mark_agent_offline:569`.
- **Task report / counters:** `service/agent_task.rs:237` (`agent_report_task`), Commit arm `:305`, `commit_suite_task:396`.
- **Task reclaim (heartbeat timeout):** `service/agent_heartbeat.rs:134` (sweeps `Running` only today).
- **Suite cancel (task teardown):** `service/suite.rs:848` (`user_cancel_task_suite`; handles `Ready`/`Pending` + force-`Running` today).
- **Schema types:** `netmito/src/schema/agent.rs` — `AcceptSuiteReq/Resp:182`, `StartSuiteReq:200`, `CompleteSuiteReq:206` (+ `SuiteCompletionReason:219` — **delete**), `RunFailureKind/Reason:236`.
- **User suite routes:** `netmito/src/api/suites.rs:20` (`suites_router`), nested `/suites/{uuid}/...`, group-permission middleware.

---

# Part A — Agent ↔ Coordinator protocol (DECIDED)

## A.1 Run creation & handle

1. **Create on accept.** `agent_accept_suite` inserts the run row in the same txn that claims the suite: state `Provision`; `run_id = COALESCE(MAX(run_id),0)+1` scoped per `task_suite_id` (unique index is the safety net — retry on conflict); copy `agent_id`, `agent_uuid`, `machine_id` off the agent row (`AuthAgent` already carries `id`+`uuid`; read `machine_id` from the agent row).
2. **Handle = opaque internal `id: i64`.** `AcceptSuiteResp` returns it; the agent stores it as an opaque token and echoes it on every later run-scoped call. The coordinator does single-column `find_by_id` lookups. The per-suite `run_id: i32` is **user-facing display only** and never appears on the agent wire.
3. **Every run-scoped call carries `run: i64`** — `start`, `cleanup`, `complete`, `hook`, **and** `report_task` (yes, the ~10 `worker.rs` call sites). Rationale: do not assume "one live run per agent"; explicit handle keeps the door open for one agent running multiple suites concurrently and makes validation a trivial id lookup everywhere.

## A.2 Lifecycle, state machine & counters

Happy path — each arrow is one agent→coordinator HTTP call carrying `run`:

```
            accept            start            cleanup          complete
 (Idle) ──────────► Provision ──────► Executing ──────► Cleanup ──────► Completed
   create run            │                │                │
   (Provision)           └────────── /complete Failed{reason} ─────────► Failed
```

Terminal states split by **who writes them**:
- **Agent-reported** (via `/complete`, see A.4): `Completed`, `Failed`.
- **Coordinator-written** (agent never reports these): `Cancelled` (user cancelled suite), `Lost` (heartbeat timeout / agent removed), `Preempted` (future; agent bumped for a higher-priority suite).

State transitions to wire into existing services:
- `agent_accept_suite` → create run `Provision` (A.1).
- `agent_start_suite` → `Provision → Executing`, set `started_at`.
- `agent_enter_cleanup` → `Executing → Cleanup`.
- `agent_complete_suite` → terminal `Completed`/`Failed` (A.4), set `finished_at`.
- heartbeat timeout (`agent_heartbeat`) / `mark_agent_offline` / `remove_agent` → any non-terminal run → `Lost`.
- suite cancellation (`user_cancel_task_suite`) → in-flight runs → `Cancelled`.

**Live counters — coordinator-authoritative.** The run's `tasks_completed` / `tasks_failed` are mutated **only** on a `report_task` `Commit` for a **non-terminal** run (incremented alongside the existing `commit_suite_task` suite-counter update). Every task's terminal fate — success *or* failure — flows through the coordinator as a `Commit` (the worker, and the agent mirroring it, reports a failed run-to-completion as `Finish` + `Commit{exit_status≠0}`, *not* as a silent retry), so this counter is the coordinator's own complete, first-hand ledger, attributed to the run via the `run` field on the report. (If the run is terminal, the report is rejected — see A.5 — so no counter update happens.)

`agent_complete_suite` does **not** overwrite these counters with the agent's `req.tasks_completed`/`tasks_failed`. The agent's numbers are a *claim* about the same events the coordinator already recorded authoritatively; `/complete` **compares** them against the stored run counters and emits a `warn!` on any disagreement (a cheap drift/bug signal), then writes only the terminal state + `finished_at`. The stored counters remain exactly what the `Commit` path recorded.

> Revisit only if the agent's task-execution layer ever gains *internal* retry (re-running a failed task without committing each attempt) — then the agent would see failures the coordinator doesn't, and would become authoritative for attempt-level counts. Neither the worker nor the (current `fake_*`) agent does this today, and there is no user-facing task-restart path either.

Hook timing (for A.3): `provision` runs during `Provision` (before `/start`); `background` runs alongside workers during `Executing`; `cleanup` runs during `Cleanup` (before `/complete`).

## A.3 Hook reporting — results, not status

1. **No "started"/live-status reports.** A hook's start is never reported; on the success path it's redundant (`/start` proves provision succeeded, `/cleanup` proves execution finished).
2. **One `suite_hook_executions` row per hook, written on completion** (success *or* failure), through a single dedicated endpoint `POST /agents/suite/hook` used uniformly for all three hook types.
3. **Reuse the result payload, not the task endpoint.** Result shape = `TaskResultSpec` (`exit_status` + `msg`). Do **not** route hooks through `/agents/tasks/{uuid}/report`: that handler is welded to the `active_tasks` lifecycle (needs an active_tasks row; `Commit` archives + decrements suite `incomplete_tasks`). A hook must never touch task counters.
4. **Accepted tradeoff — no live "background is running" indicator.** The background hook is only visible once it finishes; if it hangs there is no row and the run eventually goes `Lost`. The run being in `Executing` already implies background is up.

### A.3.5 Hook artifacts (large logs) — `suite_hook_artifacts`

Hook logs do **not** reuse the task `artifacts` table. That table keys on a task UUID and has no foreign key (the `task_id` link is logical), so reusing it for hooks would (a) overload `task_id` with non-task ids and (b) make run deletion *harder* — the rows wouldn't cascade and would have to be enumerated by hand. Instead, hook logs get a dedicated, hook-owned table with a real cascading FK:

```
suite_hook_artifacts {
  id                       bigserial PK,
  suite_hook_execution_id  bigint NOT NULL,   -- FK → suite_hook_executions.id, ON DELETE CASCADE
  content_type             int    NOT NULL,   -- reuses entity::content::ArtifactContentType
  size                     bigint NOT NULL,   -- bytes; drives the quota refund on delete
  created_at, updated_at   timestamptz NOT NULL default now(),
  UNIQUE (suite_hook_execution_id, content_type)   -- ≤1 artifact per type per hook; enables upsert
}
```

- **Cascade chain.** `suite_agent_runs → suite_hook_executions → suite_hook_artifacts`, all `ON DELETE CASCADE`. Deleting a run wipes all three levels of *metadata* automatically. S3 blobs never cascade (the DB can't reach S3), so the run-delete path reads these rows' sizes/keys first, batch-`delete_objects` them, refunds quota once (`group.storage_used -= Σ size`), then deletes the run (rows cascade away).
- **S3 key:** `hooks/{suite_hook_execution_id}/{content_type}` in the artifacts bucket — globally unique, derivable from the row, namespaced under `hooks/` so it never collides with task keys (`{task_uuid}/{content_type}`).
- **Quota:** charged on the owning suite's group, resolved via hook artifact → hook execution → run (`task_suite_id`) → suite → `group_id` (no denormalized `group_id`, matching the `artifacts` table). `Upload` checks `storage_used + len ≤ storage_quota`, upserts the row (by `hook_exec_id + content_type`), bumps `storage_used`.
- **No `uuid` column.** A hook execution never moves tables (unlike a task crossing `active → archived`), so its `i64` id is a stable enough key.

**Endpoint ordering (`POST /agents/suite/hook`, op `Result` | `Upload`):** `Result` writes the `suite_hook_executions` row (state `Completed`/`Failed` from `exit_status`), so it must precede `Upload`, which presigns against that row's `id`. The agent flow is therefore: hook process exits → `Result` (record outcome) → `Upload` per log (presign) → PUT bytes to S3. `Upload` on a `(run, hook_type)` with no recorded execution → `404/409`. This inverts the task order (where `Upload` precedes `Commit`) because a hook's row is created *on completion* (A.3.1, no start report), and the agent already holds both the exit status and the logs once the hook exits. Both ops are **append-only** and accepted even on a terminal run (ownership + existence checked).

## A.4 Outcome & failure model

1. **Agent outcome enum — two variants, by design:**
   ```rust
   enum SuiteRunOutcome { Completed, Failed { reason: RunFailureReason } }
   ```
   Replace `CompleteSuiteReq.completion_reason: SuiteCompletionReason` with `outcome: SuiteRunOutcome`; **delete `SuiteCompletionReason`** (no backward-compat — suite is new on this branch). Two variants is permanent, not provisional: the agent reports only what *it* did. `Cancelled`/`Preempted`/`Lost` are coordinator decisions, never agent outcomes.
2. **Single failure path.** A failed hook gets no dedicated "fail" call: the agent writes the failing hook's row via `/hook`, then calls `/complete` with `Failed{reason}`. The run row stores only the one-line `RunFailureReason{kind,message}`; full hook output lives in the `suite_hook_executions` row. `/complete` must be reachable from **any** non-terminal state (provision can fail before `/start` was ever called).
3. **Preemption is coordinator-owned.** Only the coordinator sees other suites' priorities and which agent to bump (same authority as `fetch_best_available_suite`; already modeled by `AgentNotification::PreemptSuite`). `Preempted` is coordinator-written to `SuiteRunState` — a future append (discriminant `7`, migration-free), kept distinct from `Cancelled` (preempt = agent bumped, suite lives on; cancel = suite killed). Not implemented now; listed so the enum/state handling leaves room.

## A.5 Report validation, terminal ownership & task reclaim

**Validation — on every run-scoped report** (`start`/`cleanup`/`complete`/`hook`/`report_task`):
1. Look up the run by internal **`id`** (not `run_id`) → must exist.
2. The run's `agent_uuid` must match the authenticated agent → must be the right agent.
3. Check `state` (see per-endpoint rule below).

The **run row is the single authority** — no `current_run_id` needed, so this is independent of design-task 5. It is also the correct check for the multi-suite future (a singular `current_run_id` can't represent multiple live runs).

**Behavior on a terminal run — split by whether the report mutates run/task state:**
- **State-mutating reports are rejected** with **409 Conflict**: `/start`, `/cleanup`, `/complete`, `report_task`. The agent must handle 409 gracefully ("run already closed → I'm free"), not as an error.
- **`/hook` is append-only and is accepted even on a terminal run** (ownership + existence still checked). It only inserts a `suite_hook_executions` row, never mutates `suite_agent_runs`, so recording post-terminal is harmless — and it preserves diagnostic detail for a hook that runs *after* a coordinator terminal (e.g. cleanup during a cancel).

**Coordinator owns teardown.** When the coordinator writes a terminal state (`Cancelled`/`Preempted`/`Lost`) it **also releases the agent** (clears its assignment / frees its slot) in the same step — it never relies on the agent's later `/complete`. The agent independently returns to `Idle` on the `SuiteCancelled`/`PreemptSuite`/`Shutdown` notification, so a rejected `/complete` is fine.

**Stranded task reclaim (important — fixes an orphan the validation rule creates).**
A suite task's lifecycle is `Running --Finish--> Finished --Commit(result)--> archived` (two separate agent calls; `Commit` carries the result and *archives* the row, removing it from `active_tasks`). So a task still in `active_tasks` as **`Finished`** is **executed-but-uncommitted** — its result was never persisted and `incomplete_tasks` was never decremented. When a run terminates between `Finish` and `Commit`, the `Commit` is rejected (above) and the task would be stranded.

Decision: **the teardown path owns the fate of uncommitted in-flight tasks — the report handler never resets tasks.**
- Extend the reclaim sweep (`agent_heartbeat.rs:147`) and the cancel sweep (`user_cancel_task_suite`) to cover **`Running` *and* `Finished`** (today they only cover `Running` / `Ready`+`Pending`+force-`Running`). `Cancelled`-state tasks are left alone (deliberate cancels, not re-run).
- `Lost` → reclaim those tasks: set `Ready`, clear `runner_uuid`, re-add to the dispatcher (`SuiteDispatcherOp::AddTask`) → re-run.
- `Cancelled` → archive those tasks as `Cancelled` → not re-run.
- The `report_task` handler on a terminal run **just rejects** (409) — by the time a late `Finish`/`Commit` arrives, teardown already decided the task's fate. The existing task-ownership check (`runner_uuid == reporting agent`, `agent_task.rs:255`) additionally prevents a late report from clobbering a task already re-dispatched to another agent. Do **not** put "reset to Ready" in the report handler (it would wrongly resurrect cancelled-suite tasks and could clobber re-dispatched ones).

## A.6 Endpoint contract (agent wire)

All behind `agent_auth_middleware`. New / changed fields in **bold**.

| Method · path | Body | Effect |
|---|---|---|
| `GET /agents/suite` | — | fetch best/specific assigned suite *(unchanged)* |
| `POST /agents/suite/accept` | `{ suite_uuid }` | create run (`Provision`); resp `{ accepted, `**`run: i64`**`, reason? }` |
| `POST /agents/suite/start` | **`{ run }`** | `Provision → Executing`; terminal/not-owned → **409** |
| `POST /agents/suite/cleanup` | **`{ run }`** | `Executing → Cleanup`; terminal/not-owned → **409** |
| `POST /agents/suite/complete` | **`{ run, tasks_completed, tasks_failed, outcome }`** | non-terminal → terminal + release; counters are **not** overwritten (compare to stored, `warn!` on mismatch); terminal → **409** |
| **`POST /agents/suite/hook`** | **`{ run, hook_type, op: Upload{content_type, content_length} \| Result(TaskResultSpec) }`** | write `suite_hook_executions` row; **accepted even if terminal** (append-only; ownership checked) |
| `POST /agents/tasks/{uuid}/report` | **`{ run, op }`** | task report; on non-terminal `Commit`, also bump run counters; terminal run → **409** |

## A.7 Agent client changes (design-task 7)

- Thread the `run: i64` handle (from `AcceptSuiteResp`) through `start_suite` / `enter_cleanup_api` / `complete_suite` / `report_task` and the new hook reporting.
- Replace the `complete_suite` reason arg with the `SuiteRunOutcome` (`Completed` on clean run/teardown, `Failed{reason}` on failure).
- Treat a `409` from any lifecycle report as "this run is already closed; I'm freed" → return to `Idle` (the stop notification also drives this); never error out.
- Report each hook result via `/agents/suite/hook` once real hooks exist (uploading large logs via the `Upload` op first).

---

# Part B — User ↔ Coordinator API (PENDING — not yet designed)

Routes carried over from the 06-10 doc; shapes still to be decided.

```
POST   /suites/{uuid}/runs/query     # filters → run summaries
GET    /suites/{uuid}/runs/{run_id}  # run detail + its hook executions  (user-facing run_id here)
DELETE /suites/{uuid}/runs           # filtered bulk delete, terminal-only
```

Open questions:
- **B1.** Route nesting + auth — expected under `suites_router` with group-permission middleware (matches existing suite routes).
- **B2.** Query filter set + run-summary response fields.
- **B3.** Detail response while hooks are still `fake_*` — build the hook-join now (returns empty) or defer.
- **B4.** Delete semantics — **decided** in 06-10: ≥1 filter required; terminal runs only.

---

# Implementation order (maps to 06-10 task list)

1. **Task 2 — run lifecycle wiring:** A.1 (create on accept), A.2 (transitions, incl. coordinator-written `Lost`/`Cancelled`), A.5 (validation + reclaim sweep extension + teardown-owns-release).
2. **Task 3 — live counters:** A.2 counters (increment on non-terminal `Commit`, reconcile at `/complete`).
3. **Task 4 — failure reporting:** A.4 (`SuiteRunOutcome`, delete `SuiteCompletionReason`); A.3 single failure path.
4. **Task 7 — agent client:** A.7.
5. **Task 6 — user API:** Part B (design first).
6. **Task 5 — `agents.current_run_id`:** *not required by A.5.* Optional later for "single source of truth" / dispatcher / fetch; if pursued, reconsider a singular pointer vs. the multi-suite direction in A.1/A.3.

Note: agent and worker task reporting intentionally remain **separate** endpoints and **separate** service functions (different auth: `AuthAgent` vs `AuthWorker`; different bookkeeping: suite/run counters vs the `Worker` row). This is already the case in the code — keep it. Only the `run` field is added to the agent path.
