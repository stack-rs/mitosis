# TaskDispatcher Improvements Plan

This plan proposes concrete, incremental tasks to improve the worker TaskDispatcher and add a load-balanced scheduling option while preserving current fanout behavior by default.

## Goals
- Reduce duplicate enqueues and wasted pops across worker queues.
- Preserve current Fanout mode behavior for compatibility.
- Add a configurable Balanced scheduling mode (least-queue first, extensible to RR/Random).
- Improve removal performance and correctness with a reverse index.
- Make backfill behavior on worker registration mode-aware.
- Add DB indices to speed up tags-based candidate discovery.
- Provide unit tests for dispatcher logic and basic integration checks at call-sites.

---

## Summary of Current Behavior
- Each eligible worker gets the same task enqueued (fanout). Workers pop from per-worker priority queues.
- The DB enforces assignment with a CAS-like update (Ready + NULL assigned → Running + assigned worker).
- On completion/commit, code broadcasts `RemoveTask(task_id)` to purge from all queues.
- On worker timeout/unregister, its running task is reset to Ready and re-enqueued to all eligible workers.

Issues:
- Duplicate enqueues across many workers ⇒ wasted pops and RemoveTask scans.
- RemoveTask scans all worker queues (O(N)).
- New worker backfill duplicates historical tasks into yet another queue.

---

## High-Level Changes
1) Add `ScheduleMode` with `Fanout` (default) and `Balanced(LeastQueue)`.
2) Extend TaskDispatcher ops to support candidate-based enqueue once, or all for fanout.
3) Maintain a reverse index `task_id -> {worker_ids}` for O(K) removals.
4) Early removal: after successful DB assignment in `fetch_task`, immediately send `RemoveTask(task_id)`.
5) Make worker registration backfill conditional on schedule mode (skip in Balanced).
6) Add config/CLI to select schedule mode and strategy.
7) Add GIN indexes for `active_tasks.tags` and `workers.tags` (Postgres arrays) for `.contains()`/`.contained()` queries.
8) Add focused unit tests for dispatcher and light integration checks around fetch/assign.

---

## Detailed Tasks

### A. Dispatcher Core: Scheduling + Reverse Index
- [ ] Introduce enums in `netmito/src/service/worker/queue.rs`:
  - `pub enum ScheduleMode { Fanout, Balanced(BalanceStrategy) }`
  - `pub enum BalanceStrategy { LeastQueue /*, RoundRobin, Random */ }`
- [ ] Update `TaskDispatcher` struct:
  - Add `mode: ScheduleMode`.
  - Add `task_index: HashMap<i64, HashSet<i64>>` mapping `task_id -> { worker_ids }`.
- [ ] Update constructor:
  - `pub fn new(cancel_token: CancellationToken, rx: UnboundedReceiver<TaskDispatcherOp>, mode: ScheduleMode) -> Self`
- [ ] Maintain `task_index` in all code paths:
  - On enqueue to a worker: insert task_id → worker_id into `task_index`.
  - On `RemoveTask`: look up worker set and remove from each queue, then delete the map entry.
  - On `UnregisterWorker`: remove worker’s queued task_ids from `task_index` (cleanup).

### B. New Dispatcher Ops (Candidate-Based Enqueue)
- [ ] Extend `TaskDispatcherOp` in `queue.rs`:
  - `EnqueueCandidates { candidates: Vec<i64>, task_id: i64, priority: i32 }`
  - Optional convenience: `EnqueueManyCandidates { candidates: Vec<i64>, tasks: Vec<(i64, i32)> }`
- [ ] Implement handling:
  - Fanout: push to all `candidates` (mirrors existing `BatchAddTask`).
  - Balanced(LeastQueue): select a single `worker_id` with the smallest `queue.len()`; tie-breaker can be first or random (deterministic preferred).
  - Update `task_index` accordingly.
- [ ] Keep existing ops for backward compatibility; plan to migrate call-sites to the new ops.

### C. Early Removal on Successful Assignment
- [ ] In `netmito/src/service/worker/mod.rs::fetch_task`:
  - After `update_many` returns a non-empty set (assignment succeeded), send `TaskDispatcherOp::RemoveTask(task_id)` immediately.
  - Rationale: mitigate duplicate pops in Fanout mode and ensure queues converge quickly.

### D. Call-Site Updates to Use Candidates
- [ ] `netmito/src/service/task.rs`:
  - In `user_submit_task`: replace `BatchAddTask` with `EnqueueCandidates` (reuse the existing candidates query).
  - In `user_change_task`: after `RemoveTask`, re-enqueue with `EnqueueCandidates`.
- [ ] `netmito/src/service/worker/mod.rs`:
  - In `remove_worker`: when redistributing a reset running task, switch `BatchAddTask` → `EnqueueCandidates`.

### E. Registration Backfill Behavior (Mode-Aware)
- [ ] In `netmito/src/service/worker/mod.rs::setup_worker_queues`:
  - If `ScheduleMode::Fanout` → keep current backfill (add historical eligible tasks to this worker’s queue).
  - If `ScheduleMode::Balanced(_)` → skip backfill to avoid duplicating tasks across queues; rely on new submissions and redistribution.
- [ ] Plumb schedule mode into worker module (see Config section). Options:
  - Add schedule mode to `InfraPool` or provide a global config handle that can be read here.

### F. Config Surface (Coordinator)
- [ ] In `netmito/src/config/coordinator.rs`:
  - Add `task_schedule_mode: Option<String>` ("fanout" | "balanced").
  - Add `task_balance_strategy: Option<String>` ("least-queue" | "round-robin" | "random").
  - Parse into `ScheduleMode` with default `Fanout` and `LeastQueue` when balanced.
  - Extend CLI (`CoordinatorConfigCli`) with corresponding flags.
- [ ] In `build_worker_task_queue`, pass computed `ScheduleMode` to `TaskDispatcher::new`.
- [ ] Consider exposing selected mode in `InfraPool` or a static so `setup_worker_queues` can branch.

### G. DB Indices for Tags
- [ ] Add a migration (e.g., `mYYYYMMDD_HHMMSS_add_gin_indices_on_tags.rs`) to create GIN indexes:
  - `CREATE INDEX IF NOT EXISTS idx_active_tasks_tags_gin ON active_tasks USING GIN (tags);`
  - `CREATE INDEX IF NOT EXISTS idx_workers_tags_gin ON workers USING GIN (tags);`
- [ ] Ensure the migration integrates with the existing migrator and is idempotent.

### H. Tests
- [ ] Unit tests for `TaskDispatcher` (pure in-memory):
  - Fanout enqueue: one task to N workers; `RemoveTask` purges across all.
  - Balanced(LeastQueue): candidates with different queue lengths → task goes to least loaded.
  - Reverse index integrity across add/remove/unregister paths.
  - Fetch behavior: `FetchTask` pops in priority order.
- [ ] Integration-ish checks (lightweight):
  - In `fetch_task`, after successful assignment, ensure `RemoveTask` is issued and other queues no longer return the id.

### I. Rollout & Backward Compatibility
- [ ] Default to Fanout to avoid behavioral changes for existing deployments.
- [ ] Document behavior differences in Balanced mode (no backfill on register, single-target enqueue, same redistribution semantics).
- [ ] Provide a config toggle/flags documented in README/guide.

### J. Optional Future Enhancements (Not in initial scope)
- [ ] Add `AckAssigned(worker_id, task_id)` and track `running_count` to prefer `queue_len + running` for load.
- [ ] Implement `RoundRobin` and `Random` strategies.
- [ ] Add a periodic “rebalance sweep” that migrates queued tasks away from overloaded workers in Balanced mode.
- [ ] Per-group schedulers if multi-tenancy isolation is needed.

---

## File/Code Touch List
- `netmito/src/service/worker/queue.rs` (core changes: mode, ops, reverse index, selection)
- `netmito/src/service/worker/mod.rs` (early removal, registration backfill gating, redistribute op)
- `netmito/src/service/task.rs` (submit/change call-site op switch)
- `netmito/src/config/coordinator.rs` (config + CLI + dispatcher construction)
- `netmito/src/migration/mYYYYMMDD_HHMMSS_add_gin_indices_on_tags.rs` (new migration)
- Tests under an appropriate `tests/` or module test files adjacent to the above.

---

## Acceptance Criteria
- Fanout mode remains default and behaviorally equivalent from a client perspective.
- Balanced mode enqueues tasks to exactly one eligible worker, selected by least queue length.
- `RemoveTask(task_id)` is O(K) in the number of workers that actually had the task.
- After assignment, duplicate task ids are promptly removed from other queues.
- Worker registration backfill is skipped in Balanced mode.
- Tags queries for candidates benefit from GIN indexes (measurable improvement on large data sets).
- Unit tests cover core dispatcher scenarios for both modes.

