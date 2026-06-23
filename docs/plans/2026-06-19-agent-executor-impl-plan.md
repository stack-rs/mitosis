# Agent-Controlled Worker Execution — Implementation Plan (working note)

**Date:** 2026-06-19 (last updated 2026-06-23)
**Goal:** Replace the agent's `fake_suite_execution` with real execution. The agent manages an **in-process pool of executors** that run the suite's tasks (and hooks) by **reusing the worker's execution core**, reporting to the coordinator via the **agent endpoints** with the **run handle**. The agent stays the only coordinator-facing party, so the run/suite accounting built in Part A is preserved.

> ### ⏩ RESUME HERE (2026-06-23)
> **Done & cargo-clean:** Phase 0 (agent coordinator endpoints) · Phase 1 (the `CoordinatorClient` seam: steps A1–A3 + B, consolidated) · the `executor` module extraction · Phase 2 (`AgentCoordinatorClient`). Compiles with only expected dead-code warnings on `AgentCoordinatorClient` (unwired until Phase 3).
> **Not yet committed:** the `executor`-module extraction may be uncommitted — check `git status` first.
> **Next: Phase 3** (the agent executor pool — see that section below; prereqs all done). Then Phase 4 (real hooks + `SuiteRunOutcome::Failed` + 409→Idle), then Phase 5 (deferred redis/cpu_binding).
> **Regression gate:** worker behavior is preserved by construction (the refactor was a pure relocation + behavior-preserving sub-steps) but has **not been run** — a worker smoke-test is the gate before relying on the seam.

> This is a working checklist so we don't drop pieces mid-way. Delete once the work lands.

---

## Decisions (recap)

- **Execution model = Option A**: in-process executor pool reusing the worker's `execute_task` core. NOT subprocess workers, NOT agent-as-proxy.
- **The `TaskReporter` seam masks *all* worker↔coordinator interaction.** That's the whole point: once every coordinator call in the execution machinery (`execute_task`/`process_task_result`, resource fetch, report, watch-poll, chaining, state-announce) goes through the seam, the machinery is reused wholesale and only the I/O endpoints swap. Worker impl → `workers/*` + worker credential + `ReportTaskReq{id, op}`. Agent impl → `agents/*` + agent credential + `ReportAgentTaskReq{run, op}`.
- **Resource download = new agent endpoints** (clean auth split, reuse identity-free services). Download stays **inside `execute_task`, per executor** — no central agent pre-download.
- **Both dependency features are supported for agent tasks** (not deferred): see "Dependency features" below.
- **A suite task's spawned child auto-joins that suite/run** — the agent injects the parent's `suite_uuid` so the child is part of the same suite accounting (reopens the suite if needed).
- **Redis `announce_state` = no-op** for the agent reporter for now. Live *streaming* of suite tasks is deferred (would need an agent `redis_url` + ACL later); this only affects fine-grained intra-exec watch (see below), not coarse-milestone watch.
- **`cpu_binding` = not implemented in code**; passed to executors as an **env var** (like the cache/dir env vars).
- Counters still flow as already built: per-task `Commit` bumps the run counters; `/complete` cross-checks the agent's totals and `warn!`s (no overwrite).

## Dependency features (both supported for agent tasks)

Two distinct, both already implemented for workers; design to make them work for agent-controlled execution:

- **Chaining (`Submit` / upstream→downstream)** — a running task's process writes `new_task.json`; on the parent's `Commit` the child is submitted `Pending`, then triggered `Ready`. **Server machinery already exists and is suite-aware**: `worker_submit_pending_task`→`internal_submit_task` (`task.rs:355`, bumps suite `total/incomplete_tasks`, reopens, adds to suite dispatcher) and `worker_trigger_pending_task` (`task.rs:364`, dispatches via `SuiteDispatcherOp::AddTask` for suite tasks). **Design = route the agent path through these:** in `agent_report_task`, make the `Submit` arm call `worker_submit_pending_task` (parent's `creator_id`, parent uuid as upstream, **parent's `suite_uuid` injected** so the child joins the suite), and the `Commit` arm trigger `worker_trigger_pending_task` on `downstream_task_uuid` (mirrors `worker/mod.rs:715`).
- **Watch (`exec_options.watch = (uuid, state)`)** — a task waits for another to reach a state. Today it's **entirely skipped without redis** (`worker.rs:833` gate), even though `watch_task` already has a poll fallback. **Design = make watch redis-optional:** subscribe via redis if available, else **poll** the watched task's state and use `TaskState::is_reach`. The agent (no redis) polls a new `GET /agents/tasks/{uuid}` state endpoint. **Coverage:** poll resolves only **coarse milestones** (DB `TaskState`, e.g. "A committed") — the meaningful "wait for completion" case; fine-grained intra-exec watch needs the deferred agent-redis work.

## Background facts (verified)

- Worker download endpoints (`api/workers.rs:163,181`) extract `Extension<AuthWorker>` but **ignore it**; services `download_artifact_by_uuid` / `worker_download_attachment` take **no identity** → reusable as-is from an agent route.
- Worker is a **single serial executor** (`fetch → execute_task → report`). `worker_count` ⇒ N executors.
- `execute_task` (`worker.rs:576–1028`) + `process_task_result`: fetch input resources → spawn process w/ timeout → capture stdout/stderr → tar + presign + upload artifacts → report `Finish`/`Commit`. All coordinator I/O goes through `TaskExecutor` (`task_client`, worker `task_credential`, `task_url`) and `report_task(executor, ReportTaskReq{id, op})`.
- Identity quirk: worker reports by task **id** (`ReportTaskReq.id`); agent reports by task **uuid** in the path (`/agents/tasks/{uuid}/report`) + `run`. The reporter must carry the task identity (pass the `WorkerTaskResp`, which has both `id` and `uuid`).

---

## Phases

### Phase 0 — Complete the agent-side coordinator API surface (server-side)

Goal: every coordinator call the seam will make must have an `agents/*` endpoint *before* the client work, so Phases 1–2 have everything to call. Enumerated against the worker's execution-time coordinator interactions:

| Worker interaction | Agent endpoint | Status |
|---|---|---|
| fetch task (`GET workers/tasks`) | `POST /agents/tasks/fetch` (batch) | exists |
| report Finish/Cancel/Commit/Upload (`POST workers/tasks`) | `POST /agents/tasks/{uuid}/report {run, op}` | exists |
| download artifact | `GET /agents/tasks/{uuid}/artifacts/{content_type}` | **done** |
| download attachment | `GET /agents/tasks/{uuid}/attachments/{*key}` | **done** |
| report **`Submit`** (chaining) | same report endpoint, `Submit` op | **TODO: wire** |
| **`Commit` triggers downstream** | inside report `Commit` arm | **TODO: wire** |
| query task state (watch poll, `GET workers/tasks/{uuid}`) | `GET /agents/tasks/{uuid}` | **TODO: add** |

Phase 0 work — **ALL DONE (2026-06-20):**
1. ✅ **`GET /agents/tasks/{uuid}`** → reuses `service::task::get_task_by_uuid` (`TaskQueryResp` with `state`+`result`, resolves active **or** archived); behind `agent_auth_middleware`, identity-free.
2. ✅ **`agent_report_task` `Submit` arm** → `worker_submit_pending_task` (parent `creator_id`, parent uuid upstream, **parent `suite_uuid` injected**); links child as `downstream_task_uuid`.
3. ✅ **`agent_report_task` `Commit` arm** → triggers `worker_trigger_pending_task(downstream_uuid)` after archive.
4. ✅ download artifact / attachment endpoints (earlier).

**Phase 0 complete — the agent has a coordinator endpoint for every execution-time interaction.**

### Phase 1 — `CoordinatorClient` seam in `worker.rs` — **DONE (2026-06-23)**

Done as compiling, behavior-preserving sub-steps (regression gate = worker smoke-test, since it can't be run here):
- **A1** — unified `report` (the `report_task` free fn + the inline upload-presign into one method returning `Option<String>`; preserves the `403`/cancel nuances).
- **A2** — extracted `download_resource` (pure I/O + resilience) returning a typed `ResourceError`; the per-status task-commit policy moved to `execute_task`'s match; the `Cancel`+`Commit` ritual became `report_cancel_commit`.
- **A3** — deduped the watch poll into the `query_task` free fn (disjoint-field args to dodge the redis-pubsub borrow).
- **B** — the trait split, on **`Box<dyn CoordinatorClient>`** + `async-trait`:

```rust
#[async_trait] pub(crate) trait CoordinatorClient: Send {
    async fn report(&mut self, id: i64, op: ReportTaskOp) -> Result<Option<String>>;   // id-based (worker & agent symmetric)
    fn artifact_download_req(&self, uuid, ct) -> reqwest::RequestBuilder;               // shared download_resource uses these
    fn attachment_download_req(&self, task_uuid, key) -> reqwest::RequestBuilder;
    async fn watch(&mut self, uuid, target);                                           // worker: redis-or-poll; agent: poll-only
    fn can_watch(&self) -> bool;                                                        // worker: has redis; agent: true
    async fn unsubscribe(&mut self, uuid);                                             // worker: redis; agent: no-op
    async fn announce_state(&mut self, uuid, state, ex: Option<u64>);                  // worker: redis; agent: no-op
}
```
- `WorkerCoordinatorClient` owns the transport (+redis) and holds the real impl. `TaskExecutor` = shared context (`cancel_token`, `force_exit`, `polling_interval`, `cache_path`, `http_client`) + `Box<dyn CoordinatorClient>`, with thin delegating wrappers so `execute_task` is unchanged. The run-loop fetch is worker-specific and keeps its own transport.
- **Endpoint symmetry fix (prereq):** the agent `report` endpoint was aligned to the worker's — `POST /agents/tasks/report` with body `ReportAgentTaskReq{run, id, op}` → `Json<ReportTaskResp>`, lookup by `find_by_id`. So `report(id, op)` is uniform across both impls (no uuid threading / `set_current_task`).

### Extracting the shared core — `executor` module — **DONE (2026-06-23)**
The seam now lives in its own module rather than in `worker.rs`. Code layout:
- **`netmito/src/executor.rs`** (`pub(crate) mod executor;`) — the **transport-agnostic core**: the `CoordinatorClient` trait, `TaskExecutor` (`{ task_cancel_token, coordinator_force_exit, polling_interval, task_cache_path, http_client, client: Box<dyn CoordinatorClient> }` + delegating wrappers), `pub(crate) execute_task`, `process_task_result`, `download_resource` + `ResourceError`/`report_cancel_commit`/`report_task`/`submit_new_task_if_present`, `ProcessOutput`/`TaskResult`.
- **`worker.rs`** — `MitoWorker`, `WorkerCoordinatorClient` (`impl CoordinatorClient`, owns transport + redis), the run loop (worker-specific fetch keeps its own transport), and the worker-specific `query_task` (hardcodes `workers/tasks/{uuid}`, used by `WorkerCoordinatorClient::watch`).
- **`agent.rs`** — `AgentCoordinatorClient` (`impl CoordinatorClient`) + (Phase 3) the pool.
- Move was a pure relocation (sed line-slice + `cargo fix` imports) → byte-identical logic; worker smoke-test still the gate.

### Phase 2 — `AgentCoordinatorClient` impl — **DONE (2026-06-23)**
`netmito/src/agent.rs`, `pub(crate) struct AgentCoordinatorClient { http_client, token, coordinator_addr, run: i64, polling_interval, cancel_token }`:
- `report(id, op)` → `POST api/agents/tasks/report { run, id, op }`, parses `ReportTaskResp`; **`409` → `cancel_token.cancel()` + Ok(None)** (run closed → stop the pool); `404` → Ok(None); `Upload`+`403` → skip; connection-retry like the worker.
- `artifact_download_req`/`attachment_download_req` → `GET api/agents/tasks/{uuid}/artifacts|attachments`.
- `watch` → **poll-only** (`query_task` against `api/agents/tasks/{uuid}` + `is_reach`, 30s). `can_watch` → `true`; `unsubscribe`/`announce_state` → no-op.
- **Watch design agreed:** redis-first, poll fallback, manually-disable-capable — agent is poll-only until the Phase-5 redis work; `can_watch` gates whether the watch-wait runs (worker: has-redis; agent: always). Caveat: agent poll resolves only *coarse* milestones; fine intra-exec watch targets time out until redis lands.
- Currently **unwired** → dead-code warnings on `AgentCoordinatorClient`/`api_url`/`query_task`; they clear when Phase 3 constructs it.

### Phase 3 — Agent executor pool (replace `fake_suite_execution`) — **NEXT (start here)**
Prereqs already done: `execute_task` is `pub(crate)`; `executor` module exists; `AgentCoordinatorClient` exists.
- In `agent.rs`, replace `fake_suite_execution` with a pool:
  - Spawn `worker_count` executor tasks (from `suite.worker_schedule` `FixedWorkers`).
  - A bounded `tokio::sync::mpsc` channel of `WorkerTaskResp`, fed by a producer that loops `agent_fetch_tasks(suite_uuid, batch=task_prefetch_count)` and sends; **overlap fetch & execution** (no collect-then-run).
  - Each consumer builds `crate::executor::TaskExecutor { task_cancel_token: pool_token.clone(), coordinator_force_exit: <a dummy Arc<AtomicBool>>, polling_interval, task_cache_path: <per-slot dir>, http_client: self.http_client.clone(), client: Box::new(AgentCoordinatorClient{ run, cancel_token: pool_token.clone(), … }) }` and loops `recv → execute_task(task, &mut te)`.
  - One pool `CancellationToken` shared into every `AgentCoordinatorClient` (so a `409` stops the whole pool) and cancelled on `self.suite_cancelled`/preempt.
  - Per-slot cache dirs (`resource/`/`exec/`/`result/`) keyed by executor index — **note `TaskExecutor`/`execute_task` assume these dirs exist** (worker creates them in `setup`); the pool must `create_dir_all` per slot.
  - Aggregate `(tasks_completed, tasks_failed)` for the `/complete` cross-check.
- The agent must still call accept→`/start`→(execute)→`/cleanup`→`/complete` around the pool (currently `fake_suite_execution` is sandwiched between `start_suite` and `enter_cleanup_api` in `process_state`).

### Phase 4 — Real hooks + outcome (closes forward-looking Task 4 + Task 7)
- Run provision / cleanup / background as **real processes** via the same exec core.
- Report each via `/agents/suite/hook`: `Result(TaskResultSpec)` (+ `Upload` for large logs).
- Timing: provision before `/start`; cleanup before `/complete`; background spawned alongside `Executing`.
- On hook/exec failure → send `SuiteRunOutcome::Failed{reason}` on `/complete`.
- Treat **409 from any lifecycle report as "run closed → go Idle"** (don't error the loop).

### Phase 5 — Deferred / optional
- Redis live-streaming for agent tasks (extend `RegisterAgentResp` with a `redis_url` + ACL; make `announce_state` real).
- `cpu_binding` env-var plumbing at executor launch.

---

## Open items to resolve during impl

- **Dependency features — RESOLVED (2026-06-20).** Both `watch` and chaining are supported for agent tasks; design captured in "Dependency features" above and folded into Phase 0 (server) + Phase 1 (seam). Suite-task children auto-join the suite.
- **`TaskExecutor` worker-isms.** Cache path is keyed by `worker_id`; generalize to a per-executor id for the agent.
- **`worker_query_task` reuse.** Confirm it (or `find_task_by_uuid`) resolves active **and** archived tasks and exposes `state` + `result` for `is_reach`; the new `GET /agents/tasks/{uuid}` needs both.
- **Process env.** Confirm `execute_task` passes dirs/limits via env so we can inject `cpu_binding` + cache dirs the same way.
- **Credential.** Agent uses one agent token for both report and download.
