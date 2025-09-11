# TaskDispatcher Review and Follow‑up Improvements

This document reviews the recent implementation against the requirements and the earlier plan, highlights strengths, identifies gaps/risks, and lists concrete next steps to harden the system.

## Summary: What’s Implemented
- Added scheduling modes in `netmito/src/service/worker/queue.rs`:
  - `ScheduleMode::{Fanout, Balanced(BalanceStrategy::LeastQueue)}`
  - Candidate-based ops: `EnqueueCandidates`, `EnqueueManyCandidates`.
  - Reverse index `task_index: HashMap<i64, HashSet<i64>>` for O(K) removal.
- Mode wiring:
  - `CoordinatorConfig` parses schedule mode/strategy; `InfraPool` carries `schedule_mode`.
  - `build_worker_task_queue` passes mode into `TaskDispatcher::new`.
- Mode-aware backfill:
  - `setup_worker_queues` skips backfill in Balanced mode; keeps backfill in Fanout.
- Early duplicate removal:
  - After successful assignment in `fetch_task`, code sends `RemoveTask(task_id)` immediately.
- Call-site migration to candidates:
  - `user_submit_task`, `user_change_task`, and `remove_worker` now use `EnqueueCandidates`.
- Performance:
  - Added GIN indices for `active_tasks.tags` and `workers.tags` (migration `m20250911_025409_add_gin_indices_on_tags.rs`).
- Tests:
  - Unit tests cover Fanout vs Balanced enqueue, reverse index integrity, priority order, and many-candidate paths.

## Requirements Coverage
- Tag matching and candidate selection: preserved (queries filter by group role and tags).
- Per-worker priority queue fetch and execute: unchanged; still periodic fetch via worker.
- Unregister/quit redistributes currently running task: covered (DB reset + `EnqueueCandidates`).
- Load-balanced scheduling option: implemented (Balanced: LeastQueue over queued counts).

## Strengths
- Backward compatible default (Fanout) while enabling Balanced.
- Early `RemoveTask` reduces duplicate pops and contention in Fanout.
- Reverse index makes `RemoveTask` O(K) and correctness clearer.
- Configurable schedule mode exposed via config/CLI; mode available in `InfraPool`.
- GIN indices address costly tags array operators at scale.

## Gaps / Risks and Suggested Fixes
1) Balanced mode: Ready tasks at startup/restoration are not enqueued
- Observation: `restore_workers` calls `setup_worker_queues` which skips backfill in Balanced. Existing `Ready` tasks will not be in any queue, so workers won’t fetch them post‑restart.
- Impact: Task starvation after coordinator restart or when enabling Balanced on an existing dataset.
- Fix: Add a one-time “initial seeding” step in Balanced mode on coordinator startup that enqueues all `Ready` tasks to one eligible worker each.
  - Option A (simple): single sweep after `Migrator::up` in `netmito/src/coordinator.rs` to enqueue all ready tasks using `EnqueueCandidates`.
  - Option B (scoped): add an API/CLI admin route to trigger a rebalance/seed job (documented for ops).

2) Balanced mode: Queued (not yet running) tasks are lost when a worker unregisters
- Observation: `TaskDispatcher::unregister_worker` removes the worker and cleans `task_index`, but does NOT redistribute that worker’s queued tasks to other candidates. In Fanout this is benign; in Balanced those tasks exist only on that worker and become stranded.
- Impact: Silent task loss from dispatcher queues (DB still has Ready tasks, but nothing enqueues them again).
- Fix: On `UnregisterWorker` (dispatcher side), capture the list of task_ids queued for that worker before removal, then emit an internal event to re‑enqueue each of them using `EnqueueCandidates` with the original priority.
  - Implementation approach:
    - Make `unregister_worker` return `Vec<(task_id, priority)>` or store them temporarily and process after removal.
    - Add a new op, e.g., `RedistributeTasks(Vec<(i64, i32)>)`, or reuse `EnqueueManyCandidates` by pairing with a cached last-known candidate set per task (see 3). If candidate set is unknown, re-query by task id (see 4).

3) Missing candidate memory (to make redistribution efficient)
- Observation: Dispatcher doesn’t remember the candidate set for a task; only which workers currently hold it (`task_index`). In Balanced, a task may only exist in one worker’s queue, so `task_index` can’t reconstruct candidates for redistribution.
- Fix: Optionally track `task_candidates: HashMap<i64, Vec<i64>>` within the dispatcher, populated on `EnqueueCandidates` calls, to use for quick redistribution. Expire this entry on `RemoveTask`.

4) Alternate redistribution path via DB
- Observation: If we don’t cache candidates in dispatcher, we can re-query candidates for a specific task id at redistribution time.
- Fix: In `remove_worker` code path, we handled only the task currently running. For queued tasks owned by a worker that is unregistering, either:
  - Emit a higher-level op to the service layer to re-query and `EnqueueCandidates` by task id; or
  - Add a small async task in coordinator to watch dispatcher’s `UnregisterWorker` results and perform candidate queries for those task ids.

5) Load metric quality in Balanced mode
- Observation: Load = `queue.len()` only. Running tasks are not counted; a worker with long-running tasks but empty queue is preferred unfairly.
- Fix options (incremental):
  - Track `running_count` by sending `AckAssigned(worker_id, task_id)` and `AckFinished(worker_id, task_id)` (or infer from `RemoveTask` + `report_task`), and balance by `(queue_len + running_count)`.
  - Future: weighted load using task priority and/or recent throughput.

6) Tie-breaking fairness in Balanced mode
- Observation: `min_by_key(queue.len())` leads to stable but possibly unfair ties (depends on DB/candidate order).
- Fix: Add a simple round‑robin cursor per candidate set or a global worker round‑robin to break ties fairly; expose `BalanceStrategy::RoundRobin`.

7) Operational visibility
- Observation: No metrics for queue lengths, task_index size, or redistribution events.
- Fix: Emit counters/gauges via tracing/logs or metrics facade (e.g., `metrics` crate) for:
  - Per‑worker queue length, running count
  - Enqueue time, assignment success/failure rate, redistribution counts
  - Fanout vs Balanced mode activity

8) Backfill policy clarity
- Observation: Skipping backfill in Balanced mode is correct to avoid duplication, but it needs documentation and an explicit rebalancing/seed mechanism.
- Fix: Document mode semantics in the guide; add an admin API or CLI command to trigger a seed/rebalance.

9) Test coverage additions
- Add tests for:
  - Balanced mode unregister: verifying queued tasks are re‑enqueued (after implementing redistribution).
  - Startup seeding: Balanced mode seed logic enqueues all `Ready` tasks exactly once.
  - Tie-break behavior and fairness (once RR is added).
  - Candidate caching correctness if implemented.

## Concrete Next Steps
1) Implement Balanced startup seeding
- Coordinator startup (after DB is ready):
  - Query all `Ready` tasks with their priorities and candidate workers (current query pattern from submit/change), then dispatch `EnqueueCandidates` per task.
  - Gate by `if matches!(infra_pool.schedule_mode, ScheduleMode::Balanced(_))`.

2) Redistribute queued tasks on worker unregister
- In `TaskDispatcher::unregister_worker`:
  - Before removal, extract all `(task_id, priority)` from that worker’s queue.
  - After removal, for each extracted task:
    - If `task_index` shows other workers still hold it (fanout), skip.
    - Else (balanced) re‑enqueue with `EnqueueCandidates` (requires candidate set lookup, see (3)/(4)).

3) Choose candidate recall strategy
- Option A (dispatcher‑local): maintain `task_candidates: HashMap<i64, Vec<i64>>` on `EnqueueCandidates` and clear it on `RemoveTask`.
- Option B (service‑assisted): emit an op or callback to the service layer to re-query candidates by task id for redistribution.

4) Improve load metric for Balanced
- Add optional `running_count` tracking to the dispatcher and use `queue_len + running_count` in LeastQueue selection.
- Wire `AckAssigned` in `fetch_task` and decrement on finish/cancel/commit.

5) Add `RoundRobin` tie-breaker and config
- Implement `BalanceStrategy::RoundRobin` with a per-mode cursor (or per-candidate set seed).
- Add `--task-balance-strategy round-robin` config path.

6) Add metrics/telemetry
- Emit periodic logs/metrics: queue sizes, assignment attempts, redistribute counts.
- Consider a `/health` or `/stats` endpoint showing scheduling mode and queue metrics.

7) Documentation updates
- Guide: document Fanout vs Balanced semantics, backfill behavior, and the new seed/rebalance operation.
- README: mention new config flags and what Balanced mode optimizes for.

## Nice-to-Haves (Future)
- Periodic background “rebalance sweep” to move queued tasks from overloaded to underutilized workers in Balanced mode.
- Per‑group schedulers to isolate tenants.
- Priority aging to prevent starvation of medium/low priority tasks.

## Code Pointers
- Dispatcher: `netmito/src/service/worker/queue.rs`
- Worker service and DB flows: `netmito/src/service/worker/mod.rs`
- Task submission/change: `netmito/src/service/task.rs`
- Coordinator + config: `netmito/src/config/coordinator.rs`, `netmito/src/coordinator.rs`
- Migrations: `netmito/src/migration/`

---

If you want, I can start with (1) seeding Balanced mode at startup and (2) safe redistribution on unregister, then add (4) improved load metric and (5) tie‑break fairness.

