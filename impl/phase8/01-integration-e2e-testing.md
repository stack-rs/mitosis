# Phase 8: Integration and E2E Testing Implementation Guide

## Overview

Phase 8 focuses on comprehensive integration and end-to-end testing of the entire Task Suite and Node Manager system. This phase validates that all components (Coordinator, Node Manager, Managed Workers, WebSocket communication, environment hooks, and IPC) work together correctly under various scenarios including normal operation, multi-manager competition, failure modes, and high-load conditions.

**Duration:** 1 week

**Goal:** Ensure the complete system is production-ready with validated reliability, performance, and backward compatibility.

## Prerequisites

All Phases 1-7 must be completed:
- ✅ Phase 1: Database Schema - Task suites, node managers, and related tables
- ✅ Phase 2: API Schema and Coordinator Endpoints - REST APIs for suite and manager management
- ✅ Phase 3: WebSocket Manager - Persistent connections for real-time communication
- ✅ Phase 4: Node Manager Core - Registration, suite assignment, and state machine
- ✅ Phase 5: Environment Hooks - Setup/teardown script execution
- ✅ Phase 6: Worker Spawning and IPC - iceoryx2-based communication and worker pool management
- ✅ Phase 7: Managed Worker Mode - Worker binary with `--managed` flag and IPC communication

**Test Infrastructure Requirements:**
- Staging coordinator instance with PostgreSQL database
- Multiple test machines capable of running node managers
- S3-compatible storage for task artifacts
- Monitoring/observability tools (metrics, logs)
- Load generation tools for performance testing

## Timeline

**Week 1: Integration and E2E Testing**
- Days 1-2: End-to-end suite execution and multi-manager scenarios
- Days 3-4: Failure scenario testing and fault tolerance validation
- Day 5: Performance testing and benchmarking
- Day 6: Backward compatibility testing
- Day 7: Bug fixes, documentation of test results, and production readiness checklist

## Design References

### Test Scenarios from RFC

#### Suite Lifecycle
- Suite state transitions: Open → Closed (3-minute inactivity), Open → Complete (all tasks finished), Closed → Open (new task submitted)
- Suite assignment to managers based on tag matching (suite.tags ⊆ manager.tags)
- Manager selection: auto-matching vs. user-specified
- Environment preparation and cleanup execution

#### Manager States
- **Idle**: No suite assigned, waiting for work
- **Preparing**: Running env_preparation hook
- **Executing**: Workers running tasks
- **Cleanup**: Running env_cleanup hook
- **Offline**: Heartbeat timeout

#### Worker Failure Handling
- Worker crash detection (exit codes, signals)
- Failure tracking per task (task_execution_failures table)
- Auto-respawn logic: retry up to 3 times, then abort
- Task reclamation from failed workers

#### Network Resilience
- WebSocket reconnection with exponential backoff (1s → 60s max)
- Manager heartbeat timeout detection
- Suite reclamation from disconnected managers
- Prefetched task requeuing

## Implementation Tasks

### Task 8.1: End-to-End Suite Execution

**Objective:** Validate complete suite lifecycle from creation to completion.

#### Test Case 8.1.1: Basic Suite Execution
**Steps:**
1. Create a task suite with:
   ```json
   {
     "group_name": "test-group",
     "tags": ["gpu", "linux"],
     "worker_schedule": {
       "worker_count": 4,
       "task_prefetch_count": 16
     },
     "env_preparation": {
       "args": ["bash", "-c", "echo 'Setup complete' && sleep 2"],
       "envs": {"TEST_ENV": "value"},
       "resources": [],
       "timeout": "1m"
     },
     "env_cleanup": {
       "args": ["bash", "-c", "echo 'Cleanup complete' && sleep 1"],
       "envs": {},
       "resources": [],
       "timeout": "1m"
     }
   }
   ```

2. Register a node manager with matching tags:
   ```bash
   mitosis-manager \
     --coordinator-url https://coordinator.example.com \
     --tags gpu,linux \
     --worker-binary ./mitosis-worker
   ```

3. Submit 100 tasks to the suite:
   ```bash
   for i in {1..100}; do
     curl -X POST /tasks \
       -H "Authorization: Bearer $TOKEN" \
       -d "{\"suite_uuid\": \"$SUITE_UUID\", \"group_name\": \"test-group\", ...}"
   done
   ```

4. Monitor suite progression:
   - Verify manager transitions: Idle → Preparing → Executing → Cleanup → Idle
   - Check env_preparation hook executes successfully
   - Verify 4 workers spawn and connect via IPC
   - Confirm all 100 tasks complete
   - Check env_cleanup hook executes after last task

**Expected Results:**
- ✅ Suite state: Open → Complete
- ✅ All tasks committed successfully
- ✅ Manager returns to Idle state
- ✅ No orphaned processes
- ✅ Hooks execute in correct order

**Validation:**
```sql
-- Check suite completion
SELECT uuid, state, total_tasks, pending_tasks, completed_at
FROM task_suites WHERE uuid = $SUITE_UUID;
-- Expected: state=2 (Complete), total_tasks=100, pending_tasks=0

-- Check task results
SELECT state, COUNT(*) FROM active_tasks
WHERE task_suite_id = (SELECT id FROM task_suites WHERE uuid = $SUITE_UUID)
GROUP BY state;
-- Expected: All tasks in state=4 (Committed) or archived

-- Check manager state
SELECT state, assigned_task_suite_id FROM node_managers WHERE uuid = $MANAGER_UUID;
-- Expected: state=0 (Idle), assigned_task_suite_id=NULL
```

---

#### Test Case 8.1.2: Suite with Resource Downloads
**Scenario:** Test S3 resource download in env_preparation hook.

**Steps:**
1. Upload test script to S3:
   ```bash
   aws s3 cp setup.sh s3://test-bucket/suite-resources/setup.sh
   ```

2. Create suite with resource specification:
   ```json
   "env_preparation": {
     "args": ["bash", "setup.sh"],
     "envs": {"DATA_DIR": "/tmp/suite-data"},
     "resources": [
       {
         "remote_file": {"S3": {"bucket": "test-bucket", "key": "suite-resources/setup.sh"}},
         "local_path": "setup.sh"
       }
     ],
     "timeout": "5m"
   }
   ```

3. Verify resource downloads before hook execution

**Expected Results:**
- ✅ Script downloads from S3 successfully
- ✅ Script executes with correct permissions
- ✅ Environment variables propagate to workers

---

#### Test Case 8.1.3: Suite Auto-Close Timeout
**Scenario:** Test 3-minute inactivity timeout (Open → Closed transition).

**Steps:**
1. Create suite and submit 10 tasks
2. Wait for all tasks to complete
3. Wait 3+ minutes without submitting new tasks
4. Query suite state
5. Submit 10 more tasks
6. Verify suite reopens (Closed → Open)

**Expected Results:**
- ✅ Suite transitions to Closed after 3 minutes
- ✅ New task submission reopens suite
- ✅ Manager continues executing tasks after reopen

**Validation:**
```sql
-- Monitor state transitions
SELECT uuid, state, last_task_submitted_at,
       EXTRACT(EPOCH FROM (NOW() - last_task_submitted_at)) as seconds_since_last_task
FROM task_suites WHERE uuid = $SUITE_UUID;
```

---

### Task 8.2: Multi-Manager Scenarios

**Objective:** Test concurrent manager operation, tag matching, and task distribution.

#### Test Case 8.2.1: Multiple Managers Competing for Same Suite
**Scenario:** 10 managers with identical tags compete for tasks from one suite.

**Setup:**
```bash
# Start 10 managers on different machines
for i in {1..10}; do
  ssh manager-host-$i "mitosis-manager \
    --coordinator-url https://coordinator.example.com \
    --tags gpu,linux \
    --worker-count 16 &"
done
```

**Test Steps:**
1. Create a suite with tags ["gpu", "linux"]
2. Submit 10,000 tasks to the suite
3. All managers become eligible and start fetching tasks
4. Monitor task distribution across managers

**Expected Results:**
- ✅ All managers receive work (no starvation)
- ✅ No task executed twice (idempotency)
- ✅ Task distribution is roughly balanced (allowing for variance)
- ✅ All 10,000 tasks complete successfully
- ✅ No database deadlocks or race conditions

**Validation:**
```sql
-- Check task distribution per manager (via prefetch tracking)
SELECT assigned_worker, COUNT(*) as task_count
FROM active_tasks_history  -- Assuming historical tracking
WHERE task_suite_id = (SELECT id FROM task_suites WHERE uuid = $SUITE_UUID)
GROUP BY assigned_worker
ORDER BY task_count DESC;

-- Verify no duplicate task executions
SELECT task_uuid, COUNT(*) as execution_count
FROM task_execution_log  -- Assuming execution audit log
WHERE suite_uuid = $SUITE_UUID
HAVING COUNT(*) > 1;
-- Expected: 0 rows
```

---

#### Test Case 8.2.2: Tag-Based Auto-Matching
**Scenario:** Test subset-based tag matching algorithm.

**Setup:**
- Suite A: tags = ["gpu", "cuda:11.8"]
- Suite B: tags = ["cpu", "linux"]
- Suite C: tags = ["gpu", "cuda:12.0", "nvlink"]
- Manager M1: tags = ["gpu", "cuda:11.8", "linux", "x86_64"]
- Manager M2: tags = ["cpu", "linux", "arm64"]
- Manager M3: tags = ["gpu", "cuda:12.0", "nvlink", "dgx"]

**Expected Matching:**
- Suite A → M1 ✅ (A.tags ⊆ M1.tags)
- Suite A → M2 ❌ (missing "gpu")
- Suite A → M3 ❌ (cuda version mismatch)
- Suite B → M1 ✅
- Suite B → M2 ✅
- Suite C → M3 ✅ only

**Test Steps:**
1. Create all suites and managers
2. Submit tasks to each suite
3. Verify assignment based on tag matching
4. Check that suites with no eligible managers remain unassigned

**Validation:**
```sql
-- Check manager eligibility for suite
SELECT s.uuid as suite_uuid, s.tags as suite_tags,
       m.uuid as manager_uuid, m.tags as manager_tags,
       (s.tags <@ m.tags) as eligible  -- PostgreSQL array containment
FROM task_suites s
CROSS JOIN node_managers m
WHERE s.uuid = $SUITE_UUID;
```

---

#### Test Case 8.2.3: User-Specified Manager Assignment
**Scenario:** Test explicit manager selection via suite_managers table.

**Setup:**
1. Create suite with selection_type = 1 (UserSpecified)
2. Insert specific managers into task_suite_managers table
3. Verify only specified managers receive assignment

**Test Steps:**
```sql
-- Create suite with user-specified managers
INSERT INTO task_suite_managers (task_suite_id, node_manager_id, selection_type)
VALUES
  ((SELECT id FROM task_suites WHERE uuid = $SUITE_UUID),
   (SELECT id FROM node_managers WHERE uuid = $MANAGER1_UUID),
   1),  -- UserSpecified
  ((SELECT id FROM task_suites WHERE uuid = $SUITE_UUID),
   (SELECT id FROM node_managers WHERE uuid = $MANAGER2_UUID),
   1);
```

**Expected Results:**
- ✅ Only MANAGER1 and MANAGER2 receive suite assignments
- ✅ Other managers with matching tags are ignored
- ✅ Task distribution limited to specified managers

---

### Task 8.3: Failure Scenarios

**Objective:** Validate fault tolerance, error handling, and recovery mechanisms.

#### Test Case 8.3.1: Worker Crash During Task Execution
**Scenario:** Worker process terminates unexpectedly mid-task.

**Test Steps:**
1. Start suite execution with 4 workers
2. Wait for tasks to start running
3. Kill one worker process:
   ```bash
   # Simulate different failure modes
   kill -9 $WORKER_PID     # SIGKILL (OOM, force kill)
   kill -11 $WORKER_PID    # SIGSEGV (segmentation fault)
   kill -6 $WORKER_PID     # SIGABRT (assertion failure)
   ```
4. Monitor manager response

**Expected Results:**
- ✅ Manager detects worker exit via process monitoring
- ✅ Manager logs failure to task_execution_failures table
- ✅ Manager auto-respawns worker (if failure_count < 3)
- ✅ Task retried on new worker
- ✅ After 3 failures: task aborted, returned to coordinator
- ✅ Suite continues with remaining workers

**Validation:**
```sql
-- Check failure tracking
SELECT task_id, node_manager_id, failure_count, last_failed_at, error_message
FROM task_execution_failures
WHERE task_id IN (SELECT id FROM active_tasks WHERE task_suite_id = $SUITE_ID)
ORDER BY last_failed_at DESC;

-- Verify task state after abort
SELECT uuid, state FROM active_tasks
WHERE id = $FAILED_TASK_ID;
-- Expected: state=1 (Ready) if failure_count >= 3
```

---

#### Test Case 8.3.2: Manager Disconnection During Execution
**Scenario:** Manager loses network connectivity to coordinator.

**Test Steps:**
1. Start suite with 16 workers and 1,000 tasks
2. After 100 tasks complete, simulate network failure:
   ```bash
   # Block manager's connection to coordinator
   iptables -A OUTPUT -d $COORDINATOR_IP -j DROP
   ```
3. Wait for heartbeat timeout (default: 600s or configured lower for testing)
4. Restore network:
   ```bash
   iptables -D OUTPUT -d $COORDINATOR_IP -j DROP
   ```
5. Observe manager reconnection

**Expected Results:**
- ✅ Coordinator detects heartbeat timeout
- ✅ Coordinator marks manager as Offline
- ✅ Coordinator reclaims prefetched tasks (returns to Ready state)
- ✅ Coordinator reopens suite for other managers
- ✅ Manager detects WebSocket disconnect
- ✅ Manager attempts reconnection with exponential backoff (1s, 2s, 4s, ..., max 60s)
- ✅ Upon reconnection:
  - Manager sends heartbeat
  - Manager reports current state
  - If suite was reclaimed: abort local execution, return to Idle
  - If suite still assigned: resume execution

**Validation:**
```bash
# Check manager logs for reconnection attempts
grep "WebSocket connection failed" manager.log
grep "WebSocket reconnected" manager.log

# Monitor heartbeat sequence
SELECT uuid, last_heartbeat, state,
       EXTRACT(EPOCH FROM (NOW() - last_heartbeat)) as seconds_since_heartbeat
FROM node_managers
WHERE uuid = $MANAGER_UUID;
```

---

#### Test Case 8.3.3: Coordinator Restart
**Scenario:** Coordinator restarts while managers are executing suites.

**Test Steps:**
1. Start 5 managers executing different suites
2. Restart coordinator:
   ```bash
   systemctl restart mitosis-coordinator
   ```
3. Monitor manager reconnections
4. Verify suite state recovery

**Expected Results:**
- ✅ All WebSocket connections drop
- ✅ Managers attempt reconnection
- ✅ Coordinator loads state from PostgreSQL on startup
- ✅ Managers re-authenticate and resume execution
- ✅ No task loss (committed tasks persist)
- ✅ Prefetched tasks re-queried from database
- ✅ Suite execution continues seamlessly

**Validation:**
```sql
-- Verify no tasks lost
SELECT COUNT(*) as total_tasks FROM active_tasks WHERE task_suite_id = $SUITE_ID;
-- Compare with pre-restart count

-- Check manager reconnections
SELECT uuid, last_heartbeat, state FROM node_managers
WHERE state != 4  -- Not Offline
ORDER BY last_heartbeat DESC;
```

---

#### Test Case 8.3.4: Environment Preparation Failure
**Scenario:** env_preparation hook fails (non-zero exit code).

**Test Steps:**
1. Create suite with failing preparation hook:
   ```json
   "env_preparation": {
     "args": ["bash", "-c", "exit 1"],  // Simulate failure
     "envs": {},
     "resources": [],
     "timeout": "1m"
   }
   ```
2. Submit tasks to suite
3. Assign manager

**Expected Results:**
- ✅ Manager transitions Idle → Preparing
- ✅ Hook executes and exits with code 1
- ✅ Manager detects failure
- ✅ Manager aborts suite assignment
- ✅ Manager transitions Preparing → Idle
- ✅ Manager reports failure to coordinator
- ✅ Suite remains in queue for other managers
- ✅ No workers spawned

**Edge Cases to Test:**
- Hook timeout (exceeds configured timeout)
- Hook killed by signal (SIGTERM, SIGKILL)
- Resource download failure (S3 access denied)

---

#### Test Case 8.3.5: Database Connection Loss
**Scenario:** Coordinator loses PostgreSQL connection temporarily.

**Test Steps:**
1. Start suite execution
2. Block coordinator's database connection:
   ```bash
   iptables -A OUTPUT -p tcp --dport 5432 -j DROP
   ```
3. Wait 30 seconds
4. Restore connection:
   ```bash
   iptables -D OUTPUT -p tcp --dport 5432 -j DROP
   ```

**Expected Results:**
- ✅ Coordinator detects connection pool exhaustion
- ✅ Coordinator retries queries with backoff
- ✅ WebSocket connections remain alive (heartbeats cached)
- ✅ Task fetch requests queue or fail gracefully
- ✅ Upon reconnection: normal operation resumes
- ✅ No permanent data loss

**Monitoring:**
```bash
# Check coordinator logs
grep "database connection" coordinator.log | tail -50
grep "connection pool" coordinator.log | tail -50
```

---

### Task 8.4: Performance Testing

**Objective:** Validate throughput, latency, and resource efficiency under load.

#### Test Case 8.4.1: Single Manager High-Task Load
**Scenario:** 1 manager × 16 workers × 10,000 tasks

**Configuration:**
```json
{
  "worker_schedule": {
    "worker_count": 16,
    "task_prefetch_count": 64
  }
}
```

**Test Steps:**
1. Create suite and submit 10,000 tasks
2. Start 1 manager with 16 workers
3. Measure:
   - Task throughput (tasks/second)
   - Task fetch latency (P50, P95, P99)
   - Worker CPU utilization
   - Manager memory usage
   - IPC message latency

**Target Metrics:**
| Metric | Target |
|--------|--------|
| Task fetch latency (P50) | < 1ms (prefetch) |
| Task fetch latency (P95) | < 10ms |
| Task fetch latency (P99) | < 50ms |
| Throughput per worker | 0.1 - 100 tasks/sec (task-dependent) |
| Manager memory usage | < 500MB |
| Worker memory usage | < 100MB per worker |

**Validation:**
```bash
# Measure task completion rate
watch -n 5 'psql -c "SELECT pending_tasks, total_tasks FROM task_suites WHERE uuid = $SUITE_UUID"'

# Profile manager performance
perf stat -p $MANAGER_PID

# Check prefetch buffer efficiency
grep "prefetch buffer" manager.log | awk '{print $NF}' | histogram
```

---

#### Test Case 8.4.2: Multi-Manager Stress Test
**Scenario:** 10 managers × 16 workers × 100,000 tasks

**Setup:**
- 10 identical managers across different machines
- 1 large suite with 100,000 tasks
- Each manager configured with task_prefetch_count=64

**Test Steps:**
1. Pre-create all 100,000 tasks
2. Start all 10 managers simultaneously
3. Measure:
   - Total execution time
   - Task distribution balance (Gini coefficient)
   - Database query latency
   - WebSocket message throughput
   - Network bandwidth usage

**Target Metrics:**
| Metric | Target |
|--------|--------|
| Total execution time | < 1 hour (task-dependent) |
| Task distribution variance | < 20% (Gini < 0.2) |
| Database connection pool saturation | < 80% |
| WebSocket message loss rate | 0% |
| Coordinator CPU usage | < 70% |

**Stress Test Variations:**
- Add managers incrementally (1, 5, 10, 20) to test scaling
- Test with mixed task durations (1s, 10s, 60s)
- Simulate random manager restarts during execution

---

#### Test Case 8.4.3: Latency Benchmarking
**Objective:** Measure end-to-end latency for different operations.

**Benchmarks:**

| Operation | Measurement | Target |
|-----------|-------------|--------|
| Suite creation (HTTP POST /suites) | Time to 201 response | < 100ms |
| Manager registration (HTTP POST /managers) | Time to 201 response | < 100ms |
| Task submission (HTTP POST /tasks) | Time to 201 response | < 50ms |
| Task fetch (WS FetchTask) | Request → Response | < 10ms (P95) |
| Task commit (WS ReportTask) | Request → Response | < 20ms (P95) |
| Manager heartbeat (WS Heartbeat) | Request → Response | < 15ms (P95) |
| WebSocket round-trip | Ping → Pong | < 5ms (P95) |

**Test Methodology:**
```bash
# HTTP latency testing
ab -n 1000 -c 10 -T application/json -p suite.json https://coordinator/suites

# WebSocket latency testing
# Custom tool to measure WS message round-trip time
ws-bench --url wss://coordinator/ws/managers --auth $JWT --requests 1000
```

---

#### Test Case 8.4.4: Resource Utilization Profiling
**Objective:** Profile CPU, memory, network, and disk I/O under load.

**Monitoring Setup:**
```bash
# Coordinator resource monitoring
docker stats mitosis-coordinator

# Manager resource monitoring
top -p $(pgrep mitosis-manager)
iotop -p $(pgrep mitosis-manager)
nethogs -p $(pgrep mitosis-manager)

# Database monitoring
SELECT * FROM pg_stat_activity;
SELECT * FROM pg_stat_database WHERE datname = 'mitosis';
```

**Profile:**
1. CPU usage per component (coordinator, manager, workers, database)
2. Memory usage over time (check for leaks)
3. Network bandwidth (WebSocket traffic, S3 transfers)
4. Disk I/O (database writes, log files)
5. File descriptor usage (worker processes, IPC channels)

**Acceptance Criteria:**
- ✅ No memory leaks (stable memory usage over 24 hours)
- ✅ CPU usage scales linearly with task load
- ✅ File descriptors released properly (no leaks)
- ✅ Network connections closed cleanly

---

### Task 8.5: Backward Compatibility Testing

**Objective:** Ensure independent workers continue functioning unchanged.

#### Test Case 8.5.1: Independent Worker Task Execution
**Scenario:** Verify existing independent workers unaffected by suite changes.

**Test Steps:**
1. Register independent worker (existing HTTP-based flow):
   ```bash
   mitosis-worker \
     --coordinator-url https://coordinator.example.com \
     --tags gpu,linux
   ```

2. Submit tasks WITHOUT suite_uuid (legacy mode):
   ```bash
   curl -X POST /tasks \
     -H "Authorization: Bearer $TOKEN" \
     -d '{
       "group_name": "test-group",
       "tags": ["gpu"],
       "priority": 5,
       "task_spec": {...}
     }'
   # Note: No suite_uuid field
   ```

3. Verify worker fetches and executes task
4. Confirm task lifecycle identical to pre-suite behavior

**Expected Results:**
- ✅ Worker registers successfully (no schema changes break registration)
- ✅ Worker polls /workers/tasks endpoint (unchanged)
- ✅ Worker receives tasks (task_suite_id = NULL)
- ✅ Worker executes and reports task (unchanged)
- ✅ Worker heartbeat still works
- ✅ No errors or warnings in worker logs

---

#### Test Case 8.5.2: Mixed Workload (Independent + Managed)
**Scenario:** Independent and managed workers coexist, executing different tasks.

**Setup:**
- 5 independent workers (HTTP-based, registered individually)
- 2 node managers with 8 managed workers each (IPC-based)
- Suite A: 1,000 tasks (assigned to managers)
- Standalone tasks: 1,000 tasks (no suite_uuid, for independent workers)

**Test Steps:**
1. Submit suite tasks to Suite A
2. Submit standalone tasks (legacy API, no suite_uuid)
3. Start independent workers
4. Start node managers
5. Monitor task distribution

**Expected Results:**
- ✅ Independent workers ONLY receive standalone tasks
- ✅ Managed workers ONLY receive suite tasks
- ✅ No cross-contamination
- ✅ Both execution modes complete successfully
- ✅ Database correctly tracks both task types

**Validation:**
```sql
-- Check task distribution
SELECT task_suite_id IS NULL as is_standalone,
       COUNT(*) as task_count
FROM active_tasks
GROUP BY task_suite_id IS NULL;

-- Verify independent workers don't see suite tasks
SELECT * FROM active_tasks
WHERE task_suite_id IS NOT NULL
  AND assigned_worker IN (SELECT id FROM workers WHERE ... independent ...);
-- Expected: 0 rows
```

---

#### Test Case 8.5.3: API Backward Compatibility
**Scenario:** Verify all existing API endpoints remain unchanged.

**Test Checklist:**
- ✅ `POST /workers` - Worker registration (no schema changes)
- ✅ `GET /workers/tasks` - Task polling (returns tasks with task_suite_id = NULL)
- ✅ `POST /workers/tasks` - Task reporting (accepts same payload)
- ✅ `POST /workers/heartbeat` - Heartbeat (unchanged)
- ✅ `POST /tasks` - Task submission (suite_uuid optional)
- ✅ `GET /tasks/{uuid}` - Task query (new fields optional in response)
- ✅ `POST /tasks/{uuid}/cancel` - Task cancellation (works for both types)

**Regression Tests:**
```bash
# Run existing integration test suite (pre-Phase 8)
pytest tests/integration/test_independent_workers.py -v

# Expected: All tests pass without modification
```

---

## Testing Checklist

### Functional Testing
- [ ] Suite creation and state transitions (Open/Closed/Complete/Cancelled)
- [ ] Manager registration and WebSocket connection
- [ ] Tag-based suite matching (subset algorithm)
- [ ] User-specified manager assignment
- [ ] Environment preparation hook execution (success/failure/timeout)
- [ ] Environment cleanup hook execution
- [ ] Worker spawning and IPC communication
- [ ] Task prefetching and buffering
- [ ] Worker health monitoring and auto-respawn
- [ ] Task failure tracking (task_execution_failures table)
- [ ] Suite completion detection
- [ ] Manager heartbeat and timeout detection
- [ ] Suite reclamation from offline managers
- [ ] Task cancellation (individual and suite-wide)
- [ ] Independent worker backward compatibility

### Failure & Recovery Testing
- [ ] Worker crash (SIGKILL, SIGSEGV, SIGABRT, exit codes)
- [ ] Worker auto-respawn (up to 3 retries)
- [ ] Task abort after 3 failures
- [ ] Manager WebSocket disconnection
- [ ] Manager reconnection with exponential backoff
- [ ] Coordinator restart recovery
- [ ] Database connection loss and reconnection
- [ ] Environment hook failure (preparation/cleanup)
- [ ] Hook timeout enforcement
- [ ] S3 resource download failure
- [ ] Prefetched task reclamation
- [ ] Suite state persistence across restarts

### Performance & Load Testing
- [ ] 1 manager × 16 workers × 10,000 tasks
- [ ] 10 managers × 16 workers × 100,000 tasks
- [ ] Task fetch latency (P50 < 1ms, P95 < 10ms, P99 < 50ms)
- [ ] Throughput meets targets (0.1-100 tasks/sec per worker)
- [ ] WebSocket message throughput
- [ ] Database query performance under load
- [ ] Task distribution balance across managers
- [ ] CPU/memory/network resource usage
- [ ] File descriptor management (no leaks)
- [ ] Long-running stability (24+ hours)

### Concurrency & Race Conditions
- [ ] Multiple managers fetching from same suite (no duplicate task execution)
- [ ] Concurrent task submissions to same suite
- [ ] Concurrent manager registrations
- [ ] Concurrent WebSocket messages (request-response multiplexing)
- [ ] Database transaction isolation (no deadlocks)
- [ ] Suite state transitions under concurrent task submissions
- [ ] Manager state transitions during disconnect/reconnect

### Security & Permissions
- [ ] Group-based manager access control (group_node_manager roles)
- [ ] Suite-to-manager permission checks (Write role required)
- [ ] JWT authentication on WebSocket connection
- [ ] Token refresh mechanism
- [ ] Unauthorized suite assignment rejection
- [ ] Task visibility scoped to groups

### Observability & Monitoring
- [ ] Manager state transition logs
- [ ] Worker spawn/exit logs
- [ ] Task execution metrics (latency, throughput)
- [ ] Error logging (failures, retries, aborts)
- [ ] WebSocket connection events
- [ ] Database query slow log
- [ ] Resource utilization metrics (CPU, memory, network)
- [ ] Alert thresholds (manager offline, high failure rate, database saturation)

---

## Success Criteria

### Correctness
✅ **Zero data loss**: All committed tasks persisted correctly
✅ **Idempotency**: No task executed more than once
✅ **State consistency**: Suite and manager states always valid
✅ **Isolation**: Independent workers unaffected by suite changes

### Reliability
✅ **Fault tolerance**: System recovers from all tested failure modes
✅ **No crashes**: Coordinator and managers stable under stress
✅ **Graceful degradation**: Failures logged, errors handled
✅ **Auto-recovery**: Workers respawn, managers reconnect

### Performance
✅ **Latency targets met**: P95 task fetch < 10ms
✅ **Throughput targets met**: 100,000 tasks complete in < 1 hour (with sufficient workers)
✅ **Resource efficiency**: No memory leaks, CPU usage reasonable
✅ **Scalability validated**: 10 managers, 160 workers tested

### Compatibility
✅ **Backward compatible**: Independent workers work unchanged
✅ **API stability**: All existing endpoints function correctly
✅ **Migration path**: Existing tasks/workers continue functioning

### Documentation
✅ **Test results documented**: All test cases recorded with pass/fail status
✅ **Known issues cataloged**: Bugs and limitations documented
✅ **Performance baselines**: Benchmark results recorded for future comparison
✅ **Production readiness**: Sign-off checklist completed

---

## Dependencies

**Required Completed Phases:**
- Phase 1: Database Schema (task_suites, node_managers, task_suite_managers, task_execution_failures)
- Phase 2: API Schema and Coordinator Endpoints (suite and manager REST APIs)
- Phase 3: WebSocket Manager (persistent connections, message routing)
- Phase 4: Node Manager Core (registration, state machine, suite assignment)
- Phase 5: Environment Hooks (env_preparation, env_cleanup execution)
- Phase 6: Worker Spawning and IPC (iceoryx2 setup, worker pool management)
- Phase 7: Managed Worker Mode (--managed flag, IPC communication)

**Infrastructure Dependencies:**
- PostgreSQL database with full schema deployed
- S3-compatible storage configured
- Multiple test machines for distributed testing
- Monitoring/logging infrastructure (Prometheus, Grafana, ELK stack, etc.)
- Load testing tools (ab, wrk, custom WebSocket benchmarking)

---

## Next Phase

**Phase 9: Documentation and Deployment (1 week)**

After successful completion of Phase 8 integration testing, proceed to:
- User guide and API documentation
- Deployment runbooks (Docker, Kubernetes)
- Monitoring dashboard setup (Grafana dashboards, alerts)
- Production migration plan
- Feature announcement and rollout

**Handoff Artifacts:**
1. Complete test results report (all test cases with pass/fail status)
2. Performance benchmark results (latency, throughput, resource usage)
3. Known issues and limitations document
4. Production readiness checklist (signed off by engineering and QA)
5. Rollback plan (in case of production issues)

---

## Appendix: Test Data and Scripts

### Sample Test Scripts

#### E2E Test Script (Bash)
```bash
#!/bin/bash
set -e

COORDINATOR_URL="https://coordinator.example.com"
TOKEN="your-jwt-token"

echo "=== Phase 8 E2E Test ==="

# 1. Create suite
SUITE_UUID=$(curl -X POST "$COORDINATOR_URL/suites" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "group_name": "test-group",
    "tags": ["test"],
    "worker_schedule": {"worker_count": 4, "task_prefetch_count": 16},
    "env_preparation": {"args": ["echo", "setup"], "timeout": "1m"}
  }' | jq -r '.uuid')

echo "Suite created: $SUITE_UUID"

# 2. Submit tasks
for i in {1..100}; do
  curl -X POST "$COORDINATOR_URL/tasks" \
    -H "Authorization: Bearer $TOKEN" \
    -H "Content-Type: application/json" \
    -d "{
      \"suite_uuid\": \"$SUITE_UUID\",
      \"group_name\": \"test-group\",
      \"tags\": [\"test\"],
      \"task_spec\": {\"args\": [\"echo\", \"task-$i\"]}
    }" > /dev/null
done

echo "100 tasks submitted"

# 3. Monitor completion
while true; do
  STATE=$(curl -s "$COORDINATOR_URL/suites/$SUITE_UUID" \
    -H "Authorization: Bearer $TOKEN" | jq -r '.state')

  if [ "$STATE" == "2" ]; then  # Complete
    echo "Suite completed!"
    break
  fi

  sleep 5
done

# 4. Verify results
PENDING=$(curl -s "$COORDINATOR_URL/suites/$SUITE_UUID" \
  -H "Authorization: Bearer $TOKEN" | jq -r '.pending_tasks')

if [ "$PENDING" == "0" ]; then
  echo "✅ E2E test PASSED"
else
  echo "❌ E2E test FAILED: $PENDING tasks still pending"
  exit 1
fi
```

#### Performance Test (Python)
```python
import asyncio
import aiohttp
import time
from statistics import mean, median, stdev

async def benchmark_task_fetch(ws_url, token, num_requests=1000):
    """Benchmark WebSocket task fetch latency"""
    latencies = []

    async with aiohttp.ClientSession() as session:
        async with session.ws_connect(
            ws_url,
            headers={"Authorization": f"Bearer {token}"}
        ) as ws:
            for i in range(num_requests):
                start = time.perf_counter()

                # Send FetchTask request
                await ws.send_json({
                    "type": "FetchTask",
                    "request_id": i,
                    "suite_uuid": "test-suite-uuid"
                })

                # Wait for response
                msg = await ws.receive_json()

                latency = (time.perf_counter() - start) * 1000  # Convert to ms
                latencies.append(latency)

    # Calculate percentiles
    latencies.sort()
    p50 = latencies[len(latencies) // 2]
    p95 = latencies[int(len(latencies) * 0.95)]
    p99 = latencies[int(len(latencies) * 0.99)]

    print(f"Task Fetch Latency (n={num_requests}):")
    print(f"  P50: {p50:.2f}ms")
    print(f"  P95: {p95:.2f}ms")
    print(f"  P99: {p99:.2f}ms")
    print(f"  Mean: {mean(latencies):.2f}ms")
    print(f"  StdDev: {stdev(latencies):.2f}ms")

    # Validate targets
    assert p50 < 1.0, f"P50 latency {p50}ms exceeds 1ms target"
    assert p95 < 10.0, f"P95 latency {p95}ms exceeds 10ms target"
    assert p99 < 50.0, f"P99 latency {p99}ms exceeds 50ms target"

    print("✅ Performance targets met!")

if __name__ == "__main__":
    asyncio.run(benchmark_task_fetch(
        "wss://coordinator.example.com/ws/managers",
        "your-jwt-token"
    ))
```

---

**End of Phase 8 Implementation Guide**
