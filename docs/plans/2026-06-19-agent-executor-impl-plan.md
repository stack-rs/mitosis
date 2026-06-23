# Agent-Controlled Worker Execution — Implementation Plan (working note)

**Date:** 2026-06-19
**Status:** Plan agreed; ready to implement.
**Goal:** Replace the agent's `fake_suite_execution` with real execution. The agent manages an **in-process pool of executors** that run the suite's tasks (and hooks) by **reusing the worker's execution core**, reporting to the coordinator via the **agent endpoints** with the **run handle**. The agent stays the only coordinator-facing party, so the run/suite accounting built in Part A is preserved.

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

### Phase 2 — `AgentCoordinatorClient` impl — **NEXT**
- `AgentCoordinatorClient { http_client, agent_token, coordinator_url, run: i64 }` (the `run` is fixed for the executor's lifetime).
- `report(id, op)` → `POST /agents/tasks/report { run, id, op }`; surface a `409` ("run closed") so the executor loop can stop (Phase 3/4).
- `artifact_download_req`/`attachment_download_req` → the Phase-0 `GET /agents/tasks/{uuid}/artifacts|attachments`.
- `watch(uuid, target)` → **poll-only** (`GET /agents/tasks/{uuid}` + `is_reach`, no redis); `can_watch` → `true`; `unsubscribe`/`announce_state` → no-op.

### Phase 3 — Agent executor pool (replace `fake_suite_execution`)
- Spawn `worker_count` executors (async tasks).
- A shared bounded channel of `WorkerTaskResp` fed by batch `agent_fetch_tasks` (prefetch sized by `task_prefetch_count`); **overlap fetch and execution** (no collect-then-process — per project concurrency guidance).
- Each executor loop: pull task → `execute_task(reporter, ...)` → repeat until channel drained + no more tasks.
- Per-slot cache dirs (`resource/`, `exec/`, `result/`) keyed by executor index.
- Cancellation: `suite_cancelled` / preempt → cancel the pool token; drain in-flight gracefully (in-flight uncommitted tasks are reclaimed coordinator-side if needed).
- Aggregate `(tasks_completed, tasks_failed)` for the `/complete` cross-check (coordinator counters remain authoritative).

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
