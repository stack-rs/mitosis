# Agent-Controlled Worker Execution — Implementation Plan (working note)

**Date:** 2026-06-19 (last updated 2026-06-25)
**Goal:** Replace the agent's `fake_suite_execution` with real execution. The agent manages an **in-process pool of executors** that run the suite's tasks (and hooks) by **reusing the worker's execution core**, reporting to the coordinator via the **agent endpoints** with the **run handle**. The agent stays the only coordinator-facing party, so the run/suite accounting built in Part A is preserved.

> ### ⏩ RESUME HERE (2026-06-25)
> **Done & cargo-clean (compiles + clippy, no warnings):** Phase 0 (agent coordinator endpoints) · Phase 1 (the `CoordinatorClient` seam: steps A1–A3 + B, consolidated) · the `executor` module extraction (committed `0ca8011`) · Phase 2 (`AgentCoordinatorClient`) · **Phase 3 (agent executor pool — replaces `fake_suite_execution`)** + the **2026-06-25 agent-loop refinements** below.
> **Phase 3 reviewed and ready to commit** (still unstaged); **not yet smoke-tested** — the regression gate below is deferred, not waived.
> **2026-06-25 agent-loop refinements (reviewed):**
> - **Notification-driven suite pickup.** `process_state` Idle no longer polls `fetch_suite(None)` every loop tick (that duplicated the coordinator's own per-heartbeat availability check → redundant DB load). Pickup is now driven by a `pending_suite: Option<PendingSuite>` slot on `AgentClient`. `SuiteAvailable`/`PreemptSuite` notifications record into it (`note_pending_suite`); the Idle branch `take`s it and does **one** fetch attempt — `None` ⇒ stay idle (no spin), `Any` ⇒ `fetch_suite(None)`, `Specific(uuid)` ⇒ `fetch_suite(Some(uuid))` (uses the notification's suite hint, which the old code discarded). A named suite always wins over `Any`. Set-while-Executing is intentional: a notification arriving mid-run is remembered and acted on the instant we reap to Idle (back-to-back pickup without waiting a heartbeat). `PreemptSuite` also records the new suite so the reap goes straight for it. Coordinator heartbeat reconcile (`service/agent.rs:224-358`) guarantees an idle agent with work is re-notified, so notifications are a sufficient (not just advisory) trigger.
> - **`JoinSet` for the executor pool consumers** (was `Vec<JoinHandle>` + for-await). Cancellation stays cooperative via `pool_token`; the set is always fully drained, so `JoinSet`'s abort-on-drop (hard cancel) never fires and teardown stays graceful.
> **Counter decision changed during Phase 3:** the agent no longer tallies completed/failed tasks. `complete_suite` always reports `0/0`; the coordinator's commit-derived run counters remain authoritative; the `CompleteSuiteReq` fields are kept on the wire (so a cross-check can return later) but the server-side reconcile `warn!` was removed. So `execute_task` stays untouched (no outcome return value).
> **Known deferred bug (not blocking commit):** the WS notification heartbeat-fallback drains the per-agent buffer **unconditionally** (`get_remaining_notifications` → `drain(..)`), not ACK-gated like the WS path — lossy if a heartbeat response is dropped. Tolerable today because state-critical notifications self-heal via the heartbeat reconcile; do **not** add informational, non-DB-reconstructible notification types until this is fixed (make the heartbeat path ack-by-`last_notification_id`).
> **Next: Phase 4** (real hooks + `SuiteRunOutcome::Failed` + 409→Idle), then Phase 5 (deferred redis/cpu_binding). `cpu_binding` confirmed not plumbed: schema carries it but `execute_pool` discards it (`..`) and `execute_task` sets no affinity env — intended (Phase 5, "passed as env var, not in-Rust binding").
> **Regression gate:** worker behavior is preserved by construction (the refactor was a pure relocation + behavior-preserving sub-steps) but has **not been run** — a worker smoke-test, plus an agent end-to-end run (live coordinator + DB + S3), is the gate before relying on the seam/pool.

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
- Counters: per-task `Commit` bumps the coordinator-authoritative run counters (unchanged). **The agent no longer tallies its own totals** (Phase-3 decision) — `/complete` sends `0/0` and the server-side cross-check `warn!` was removed; the `CompleteSuiteReq` count fields stay on the wire so a cross-check can return without a protocol change.

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

### Phase 3 — Agent executor pool (replace `fake_suite_execution`) — **DONE (2026-06-23)**
Prereqs done: `execute_task` is `pub(crate)`; `executor` module exists; `AgentCoordinatorClient` exists.

**Concurrency restructure (the key design point, not in the original sketch):** running the suite inline in `process_state` would block the single `select!` loop, starving **both** heartbeats and notification handling for the whole run (→ coordinator marks the run `Lost`; cancel/preempt never delivered). So the suite lifecycle is now **spawned**:
- `AgentClient` gained `pool_token: Option<CancellationToken>` + `current_execution: Option<JoinHandle<()>>` (and `polling_interval`, `cache_path`); dropped `suite_cancelled`/`current_run`.
- `process_state` **Idle** = consume the `pending_suite` slot (`take`; `None` ⇒ stay idle) → `fetch_suite(target)` → `accept_suite` (quick, on the main task; sets `assigned_suite_uuid` + run handle) → make a fresh `pool_token` → **spawn** `SuiteRunner::run(suite, run)` → `state = Executing`, return. **Executing** = non-blocking `JoinHandle::is_finished()` reap → clear `assigned_suite_uuid`/`pool_token` → `Idle`. Heartbeat stays in the main loop (no separate task needed — spawning the lifecycle already keeps it alive). *(2026-06-25: Idle no longer polls unconditionally — pickup is notification-driven via `pending_suite`; see RESUME HERE.)*
- `SuiteCancelled`/`PreemptSuite`/shutdown now **cancel `pool_token`** (replacing the old `suite_cancelled` bool that nothing read in time). `PreemptSuite` additionally records the new suite into `pending_suite` so the post-reap Idle goes straight for it.

**`SuiteRunner`** (owns cloned connection context: `http_client`, `token`, `coordinator_addr`, `polling_interval`, `cache_path`, `pool_token`) holds the relocated run-scoped methods (`start_suite`/`enter_cleanup_api`/`complete_suite`/`fake_env_*`) and drives provision→`/start`→`execute_pool`→`/cleanup`→`/complete`, logging+proceeding on each error, then best-effort removes `<cache_path>/<run>`.

**`execute_pool`:** bounded `tokio::sync::mpsc` (cap = `worker_count*prefetch`) fed by one **producer** task looping `agent_fetch_tasks(suite_uuid, capacity)` (free fn; empty-batch backoff w/ `MAX_EMPTY_RETRIES` then drop sender; honors `pool_token`); **`worker_count` consumers** in a `tokio::task::JoinSet` (2026-06-25; was a `Vec<JoinHandle>` + for-await — drained to completion so abort-on-drop never fires, cancellation stays cooperative via `pool_token`) sharing `Arc<Mutex<Receiver>>`, each `create_dir_all`-ing its per-slot dir (`<cache_path>/<run>/<slot>/{result,exec,resource}`) and building a `TaskExecutor { task_cancel_token: pool_token, coordinator_force_exit: Arc::new(AtomicBool::new(false)) /* vestigial — unread by execute_task */, …, client: AgentCoordinatorClient{ run, cancel_token: pool_token, … } }`, looping `recv → execute_task`. Overlaps fetch & execution; a `409` (or any cancel) stops the whole pool. `worker_schedule.cpu_binding` is discarded here (Phase 5).

**Counters:** no agent-side tally (see RESUME HERE) — `complete_suite` sends `0/0`; `execute_task` returns `Result<()>` unchanged. `fake_suite_execution`/`fetch_tasks`/`report_task` deleted; `polling_interval` added to `AgentConfig`.

**Not done:** smoke/e2e test (regression gate, deferred). **Still fake:** provision/cleanup hooks (Phase 4).

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
