# Node Manager

## Overview

A **Node Manager** is a device-level service that runs on your infrastructure (physical machines, VMs, containers) and manages worker pools for executing Task Suites.

Think of it as a "supervisor" that:
- Lives on your compute node (GPU server, cluster node, etc.)
- Spawns and manages multiple workers
- Coordinates resource allocation (CPU cores, GPUs, memory)
- Executes environment setup/cleanup hooks
- Communicates with the Coordinator via WebSocket

## Why Node Managers?

### The Problem

Traditional Mitosis workers operate independently, which creates several challenges:

**Problem 1: Resource Contention**
```
Same GPU server with 3 independent workers:
- Worker A: Claims GPU 0, uses 80% VRAM
- Worker B: Also tries GPU 0 → OOM crash
- Worker C: Competes for CPU cores → thrashing

No coordination → conflicts and crashes
```

**Problem 2: No Environment Orchestration**
```
ML training campaign needs:
1. Download 50GB dataset
2. Run 1000 tasks
3. Clean up dataset

Independent workers: Each downloads separately (waste!)
No one coordinates: Who deletes the dataset?
```

**Problem 3: Polling Overhead**
```
100 workers on same machine:
- Each polls coordinator every 5 seconds
- 100 workers × 12 polls/min = 1,200 requests/min per machine

Coordinator overload + network waste
```

### The Solution

Node Manager centralizes control:

1. **Single registration** per machine (not per worker)
2. **WebSocket connection** for push-based communication (no polling)
3. **Local resource coordination** (CPU binding, GPU allocation)
4. **Environment lifecycle management** (setup once, cleanup once)
5. **Worker health monitoring** (auto-restart crashed workers)

## How It Works

### Architecture

```
┌─────────────────────────────────────────────────┐
│                 Your GPU Server                  │
│                                                  │
│  ┌────────────────────────────────────────┐    │
│  │         Node Manager Process           │    │
│  │  • WebSocket to Coordinator            │    │
│  │  • Fetches Task Suites                 │    │
│  │  • Runs environment setup/cleanup      │    │
│  │  • Spawns and monitors workers         │    │
│  │  • Proxies task fetch/report via IPC   │    │
│  └────────┬──────────────────────┬────────┘    │
│           │ IPC (iceoryx2)       │              │
│           ▼                      ▼              │
│  ┌─────────────┐       ┌─────────────┐         │
│  │  Worker 1   │       │  Worker 2   │  ...    │
│  │  (managed)  │       │  (managed)  │         │
│  │             │       │             │         │
│  │ • CPU 0     │       │ • CPU 1     │         │
│  │ • GPU 0     │       │ • GPU 0     │         │
│  └─────────────┘       └─────────────┘         │
└─────────────────────────────────────────────────┘
```

### Lifecycle

```
1. Registration
   └─> Manager registers with Coordinator
       Receives JWT token, establishes WebSocket

2. Idle
   └─> Waiting for suite assignment
       Sends heartbeats, ready to work

3. Suite Assignment
   └─> Coordinator pushes suite via WebSocket
       Manager accepts and transitions to Preparing

4. Environment Preparation
   └─> Download resources from S3
       Execute setup command
       If success → continue
       If failure → abort suite, return to Idle

5. Worker Spawning
   └─> Spawn N workers (from suite spec)
       Apply CPU bindings
       Setup IPC communication

6. Task Execution
   └─> Workers fetch tasks via IPC → Manager → Coordinator
       Manager monitors worker health
       Auto-restart crashed workers
       Track failures (abort after 3 crashes)

7. Completion Detection
   └─> All tasks finished (suite becomes Complete)
       Transition to Cleanup

8. Environment Cleanup
   └─> Gracefully shutdown workers
       Execute cleanup command
       Report completion to Coordinator

9. Back to Idle
   └─> Fetch next eligible suite, repeat
```

## Key Responsibilities

### 1. Resource Management

**Prevent Resource Conflicts:**
```
Suite specifies:
  worker_count: 4
  cpu_binding:
    cores: [0, 1, 2, 3]
    strategy: Exclusive

Manager ensures:
  Worker 1 → CPU 0 (exclusive)
  Worker 2 → CPU 1 (exclusive)
  Worker 3 → CPU 2 (exclusive)
  Worker 4 → CPU 3 (exclusive)

No contention, predictable performance!
```

**Machine-Level Mutex:**
- Only ONE manager runs per physical machine
- Prevents multiple managers from conflicting
- Uses lock file or system-level mechanism

### 2. Environment Orchestration

**Preparation Hook:**
```bash
# Runs ONCE before spawning workers
# Example: Download dataset
./setup.sh --download-dataset

# Manager provides context:
MITOSIS_TASK_SUITE_UUID=550e8400-...
MITOSIS_TASK_SUITE_NAME="ML Training"
MITOSIS_WORKER_COUNT=16
```

**Cleanup Hook:**
```bash
# Runs ONCE after all tasks complete
# Example: Upload results and delete temp files
./cleanup.sh

# Same context variables available
```

### 3. Worker Pool Management

**Spawning:**
- Launch workers as subprocesses
- Pass IPC configuration (manager UUID, worker local ID)
- Apply CPU affinity bindings
- Workers are **anonymous** (not registered with Coordinator)

**Monitoring:**
- Poll worker process health every 1 second
- Detect exits (normal, crash, OOM, segfault)
- Track which task the worker was executing

**Auto-Recovery:**
```
Worker crashes while executing task:
1. Manager detects exit (e.g., SIGSEGV)
2. Records failure in database
3. Checks failure count:
   - First crash → Respawn worker, retry task
   - Second crash → Respawn worker, retry task
   - Third crash → Abort task, return to coordinator
4. Respawn worker automatically
```

### 4. Communication Proxy

**IPC (Manager ↔ Workers):**
- Uses iceoryx2 shared memory (zero-copy, ultra-fast)
- Workers send: FetchTask, ReportTask
- Manager sends: TaskResp, ControlMessage (Shutdown, CancelTask)

**WebSocket (Manager ↔ Coordinator):**
- Persistent connection (no polling!)
- Push-based: Coordinator sends suite assignments
- Multiplexed: Handle multiple concurrent requests

**Task Fetch Flow:**
```
Worker 1 ──IPC: FetchTask──> Manager ──WS──> Coordinator
                                              ↓ (SELECT task)
Worker 1 <──IPC: TaskResp──── Manager <─WS─── Coordinator
```

Benefits:
- Worker gets task in ~1ms (IPC) instead of ~50ms (HTTP)
- Coordinator sees 1 WebSocket connection instead of 16 HTTP pollers
- Manager can prefetch tasks into local buffer

### 5. Failure Tracking

**Per-Task Failure Counting:**
```
Task UUID: abc123
Manager: mgr-1

Attempt 1: Worker 3 crashes (SIGSEGV) → failure_count = 1, retry
Attempt 2: Worker 3 crashes (SIGSEGV) → failure_count = 2, retry
Attempt 3: Worker 3 crashes (SIGSEGV) → failure_count = 3, ABORT

Manager tells Coordinator: "Task abc123 failed 3 times, aborting"
```

**Smart Abort Logic:**
- Hardware faults (SIGSEGV, SIGILL): Abort after 2 attempts
- Resource issues (SIGKILL/OOM): Abort after 3 attempts
- Exit codes: Abort after 3 attempts
- Graceful exits (SIGTERM): Never abort, it's intentional

## States

Node Manager progresses through five states:

| State | Meaning | What Manager Does |
|-------|---------|-------------------|
| **Idle** | No suite assigned | Send heartbeats, wait for assignment |
| **Preparing** | Running setup hook | Download resources, execute preparation command |
| **Executing** | Workers running tasks | Spawn workers, proxy IPC↔WS, monitor health |
| **Cleanup** | Running cleanup hook | Shutdown workers, execute cleanup command |
| **Offline** | Heartbeat timeout | Coordinator marks manager offline, reclaims suite |

**State Transitions:**
```
Idle → Preparing (suite assigned)
  ↓ (setup fails)
Idle ← Preparing (abort suite, try next)
  ↓ (setup succeeds)
Executing ← Preparing (spawn workers)
  ↓ (all tasks done)
Cleanup ← Executing
  ↓ (cleanup complete)
Idle ← Cleanup (fetch next suite)
```

## Worker Modes

Mitosis now supports two worker modes:

### Independent Mode (Existing - Unchanged)

```
Worker Process:
  • Registers with Coordinator (POST /workers)
  • Gets its own JWT token
  • Polls for tasks (GET /workers/tasks every 5s)
  • Visible in Coordinator database
  • No manager involvement

Use when:
  • One-off jobs
  • No resource coordination needed
  • Backward compatibility
```

### Managed Mode (New)

```
Worker Process:
  • Spawned by Node Manager
  • Communicates via IPC (not HTTP)
  • Anonymous to Coordinator (not in database)
  • Manager authenticates on behalf of workers
  • Tied to Task Suite lifecycle

Use when:
  • Running Task Suites
  • Need resource coordination
  • Environment setup/cleanup
  • High task throughput (prefetching)
```

**Key Insight:** Managed workers are "invisible" to the Coordinator. The Coordinator only sees the Node Manager, which proxies all worker requests.

## How Suites Get Assigned

### Tag Matching

Managers have tags describing their capabilities:
```yaml
manager_tags: ["gpu", "linux", "x86_64", "cuda:11.8"]
```

Suites specify required tags:
```yaml
suite_tags: ["gpu", "linux", "cuda:11.8"]
```

**Matching Rule:** Manager must have ALL of the suite's tags (subset matching).
```
Manager tags: ["gpu", "linux", "x86_64", "cuda:11.8"]
Suite tags:   ["gpu", "linux", "cuda:11.8"]

Match? YES! Manager has all required tags.
```

### Assignment Methods

**Method 1: Automatic (Tag-Based)**
```bash
# Refresh tag-matched managers for a suite
curl -X POST https://coordinator/suites/550e8400-.../managers/refresh

# Coordinator finds all managers with matching tags
# and Write permission, then assigns them
```

**Method 2: Explicit (User-Specified)**
```bash
# Explicitly assign specific managers
curl -X POST https://coordinator/suites/550e8400-.../managers \
  -d '{
    "manager_uuids": ["mgr-1", "mgr-2"]
  }'

# Useful for testing, dedicated hardware, etc.
```

### Push Notification

```
User creates suite → Coordinator assigns managers
                   ↓
Coordinator ──WebSocket: SuiteAssigned──> Manager
                                          ↓
                                    Manager accepts
                                    Transitions to Preparing
```

## Task Prefetching

Managers maintain a local buffer of tasks to minimize latency:

**Without Prefetching:**
```
Worker: "Give me a task"
Manager: "Wait, I'll ask Coordinator..." (50ms round-trip)
Worker: (idle, waiting...)
```

**With Prefetching:**
```
Manager: (maintains buffer of 32 tasks in memory)

Worker: "Give me a task"
Manager: "Here you go!" (1ms, from local buffer)
Worker: (starts immediately)

Manager: (async refills buffer in background)
```

**Configuration:**
```yaml
worker_schedule:
  task_prefetch_count: 32  # Buffer size

Higher count → Less latency, more memory
Lower count → More latency, less memory
```

## Getting Started

### Installing Node Manager

```bash
# Download the mitosis-manager binary
wget https://releases/mitosis-manager

# Make executable
chmod +x mitosis-manager

# Create configuration
cat > manager-config.toml <<EOF
coordinator_url = "https://coordinator.example.com"
tags = ["gpu", "linux", "x86_64", "cuda:11.8"]
labels = ["datacenter:us-west", "machine:gpu-01"]
groups = ["ml-team"]
EOF
```

### Registering the Manager

```bash
# Register with Coordinator (one-time)
./mitosis-manager register \
  --config manager-config.toml \
  --token-file /etc/mitosis/manager-token.jwt

# Starts manager service
./mitosis-manager run \
  --config manager-config.toml \
  --token-file /etc/mitosis/manager-token.jwt
```

**What happens:**
1. Manager registers with Coordinator (POST /managers)
2. Receives JWT token (saves to token-file)
3. Establishes WebSocket connection
4. Enters Idle state, ready for suite assignments

### Running as a Service

```bash
# systemd service example
cat > /etc/systemd/system/mitosis-manager.service <<EOF
[Unit]
Description=Mitosis Node Manager
After=network.target

[Service]
Type=simple
User=mitosis
ExecStart=/usr/local/bin/mitosis-manager run \
  --config /etc/mitosis/manager-config.toml \
  --token-file /etc/mitosis/manager-token.jwt
Restart=always
RestartSec=10

[Install]
WantedBy=multi-user.target
EOF

# Enable and start
sudo systemctl enable mitosis-manager
sudo systemctl start mitosis-manager

# Check status
sudo systemctl status mitosis-manager
```

### Monitoring Manager Status

```bash
# Query managers
curl https://coordinator/managers?group_name=ml-team

# Response:
{
  "managers": [
    {
      "uuid": "mgr-550e8400-...",
      "tags": ["gpu", "linux", "cuda:11.8"],
      "state": "Executing",
      "assigned_suite_uuid": "suite-abc123-...",
      "last_heartbeat": "2025-11-17T10:30:00Z"
    }
  ]
}
```

### Graceful Shutdown

```bash
# Shutdown manager gracefully
curl -X POST https://coordinator/managers/mgr-550e8400-.../shutdown \
  -d '{"op": "Graceful"}'

# Manager will:
# 1. Stop accepting new suites
# 2. Finish current suite's cleanup
# 3. Shutdown all workers
# 4. Disconnect WebSocket
# 5. Exit
```

## Best Practices

1. **One manager per machine**: Prevents resource conflicts
2. **Use accurate tags**: Ensures suites match appropriate hardware
3. **Monitor heartbeats**: Detect offline managers quickly
4. **Set appropriate buffer sizes**: Balance latency vs. memory
5. **Test hooks locally**: Debug setup/cleanup scripts before deploying
6. **Use systemd or equivalent**: Ensure manager auto-restarts on failure
7. **Secure token storage**: Store JWT with restricted permissions (0600)

## Advanced Topics

### CPU Binding Strategies

**RoundRobin (Default):**
```
4 workers, 4 cores → Worker 0: Core 0, Worker 1: Core 1, etc.

Good for: Balanced CPU workloads
```

**Exclusive:**
```
2 workers, 4 cores → Worker 0: Cores 0-1, Worker 1: Cores 2-3

Good for: CPU-intensive tasks, avoid cache conflicts
```

**Shared:**
```
4 workers, 4 cores → All workers share all cores

Good for: I/O-bound tasks, maximize concurrency
```

### Heartbeat and Lease

**Heartbeat Interval:** Manager sends heartbeat every 30 seconds
**Heartbeat Timeout:** Coordinator marks manager offline after 2 minutes (no heartbeat)

**Suite Lease:** When assigned a suite, manager gets a lease (expires after inactivity)
- Prevents manager from holding suite forever if it crashes
- Coordinator reclaims suite if lease expires

### Reconnection and Recovery

**WebSocket Disconnect:**
```
Network failure → Manager detects disconnect
              ↓
Manager reconnects with exponential backoff (1s → 60s max)
              ↓
Re-establish connection, send heartbeat
              ↓
Resume executing current suite (if any)
```

**Manager Crash:**
```
Manager crashes mid-suite
              ↓
Coordinator detects heartbeat timeout (2 min)
              ↓
Marks manager offline, reclaims suite
              ↓
Suite tasks returned to coordinator queue
              ↓
Another manager can pick up the suite
```

## Troubleshooting

### Manager won't start

```bash
# Check logs
journalctl -u mitosis-manager -f

# Common issues:
# - Token file missing/invalid → Re-register
# - Coordinator unreachable → Check network
# - Port conflict → Check if another manager is running
```

### Manager stuck in Preparing

```bash
# Check preparation hook logs
# Likely: Hook timed out or failed

# Fix:
# - Increase timeout in suite definition
# - Debug hook script locally
# - Check resource downloads (S3 access)
```

### Workers keep crashing

```bash
# Check failure tracking
curl https://coordinator/tasks/{task_uuid}

# Response shows failure count:
{
  "failures": [
    {"manager_uuid": "mgr-1", "count": 3, "reason": "SIGSEGV"}
  ]
}

# Possible causes:
# - Hardware issues (bad RAM, GPU)
# - Task code has segfault bug
# - Resource exhaustion (OOM)
```

### High latency

```bash
# Increase prefetch buffer
# Edit suite worker_schedule:
"task_prefetch_count": 64  # Was 32

# Monitor buffer usage in logs
# Ensure manager has enough memory for buffer
```

## Summary

Node Managers bring enterprise-grade resource management to Mitosis:

- **Centralized control** per machine (not per worker)
- **WebSocket-based** push communication (no polling waste)
- **Resource coordination** prevents conflicts
- **Environment orchestration** via hooks
- **Automatic recovery** from worker crashes
- **Task prefetching** for low latency

Combined with [Task Suites](./task-suite.md), Node Managers enable large-scale batch processing with minimal overhead and maximum reliability.
