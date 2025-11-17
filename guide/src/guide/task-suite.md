# Task Suite

## Overview

A **Task Suite** is a first-class entity in Mitosis that represents a collection of related tasks with a shared execution environment, scheduling policies, and unified lifecycle management.

Think of a Task Suite as a "batch job" or "campaign" that groups many individual tasks under a single umbrella, allowing you to:
- Set up the execution environment once (instead of for each task)
- Monitor progress as a unified whole
- Control all tasks together (cancel entire suite, track completion)
- Clean up resources after all tasks complete

## Why Task Suites?

### The Problem

In traditional Mitosis, each task is independent. This works well for one-off jobs, but creates challenges for common workflows:

**Problem 1: Environment Setup Overhead**
```
Imagine you need to run 1000 ML training tasks, each needing a 50GB dataset:
- Without suites: Each task downloads the dataset → 50TB total downloaded!
- With suites: Download once in setup hook → 50GB total downloaded
```

**Problem 2: Resource Contention**
```
Multiple workers on the same GPU server compete for resources:
- Worker A: Uses GPU 0, 80% VRAM
- Worker B: Tries GPU 0 → OOM error
- Worker C: Competes for CPU cores → thrashing

No coordination = conflicts and failures
```

**Problem 3: Campaign Management**
```
You want to run "October training campaign" with 5000 tasks:
- How do you monitor overall progress?
- How do you cancel everything if early results are bad?
- How do you know when the entire campaign is done?

Manual tracking is tedious and error-prone
```

### The Solution

Task Suites solve these problems by introducing:

1. **Environment Orchestration**: Setup/teardown hooks that run once for all tasks
2. **Unified Lifecycle**: Track suite state, not just individual tasks
3. **Batch Operations**: Cancel, monitor, and manage tasks as a group
4. **Resource Coordination**: Node Manager ensures workers don't conflict (covered in [Node Manager guide](./node-manager.md))

## Common Use Cases

### Use Case 1: ML Training Campaign

**Scenario:** You're running a hyperparameter sweep for ResNet50 with 1000 different configurations.

**Without Task Suite:**
```
1. Submit 1000 individual tasks
2. Each task:
   - Downloads dataset (50GB × 1000 = 50TB!)
   - Extracts and preprocesses
   - Trains model
   - Uploads checkpoint
   - Deletes temporary files
3. Manually track which tasks belong to this campaign
```

**With Task Suite:**
```yaml
Suite: "ResNet50 Hyperparameter Sweep"

Environment Setup (runs once):
  - Download ImageNet dataset (50GB)
  - Extract and shard data
  - Warm up GPU

Tasks (1000 tasks, all share the environment):
  - Each task: Train with different hyperparameters
  - Save checkpoint to S3

Environment Cleanup (runs once at the end):
  - Upload best checkpoint
  - Delete temporary shards
  - Clear GPU memory
```

**Benefits:**
- 50TB → 50GB downloads (1000× reduction!)
- Single command to cancel entire sweep
- Track suite progress: "453/1000 tasks complete"
- Automatic cleanup when done

### Use Case 2: CI/CD Test Pipeline

**Scenario:** Pull request needs 170 tests across unit, integration, and E2E.

**With Task Suite:**
```yaml
Suite: "PR #1234 - Test Pipeline"

Environment Setup:
  - Clone repository at commit SHA abc123
  - Build Docker image
  - Start test database

Tasks:
  - 100 unit test tasks
  - 50 integration test tasks
  - 20 E2E test tasks

Environment Cleanup:
  - Stop database
  - Collect coverage reports
  - Clean Docker images
```

**Benefits:**
- Build once, test many times
- Database shared across all tests
- Cancel entire PR test run with one command
- Know when PR is fully tested

### Use Case 3: Batch Data Processing

**Scenario:** Process Q4 2024 logs (10,000 files, 1TB total).

**With Task Suite:**
```yaml
Suite: "Q4 2024 Log Processing"

Environment Setup:
  - Download Q4 logs from S3 (1TB)
  - Create local index for fast lookup

Tasks:
  - 10,000 tasks (one per log file)
  - Each extracts metrics and anomalies

Environment Cleanup:
  - Upload aggregated results
  - Delete processed logs
```

**Benefits:**
- Download 1TB once, process locally
- Fast random access via local index
- Track: "7,342/10,000 files processed"
- Automatic cleanup saves disk space

## Key Concepts

### Lifecycle States

A Task Suite progresses through four states:

```
Open → Closed → Complete
  ↓       ↓         ↓
  └───→ Cancelled ←┘
```

| State | Meaning | What Happens |
|-------|---------|-------------|
| **Open** | Actively accepting new tasks | Tasks can be submitted, workers are executing |
| **Closed** | Temporarily paused (no new tasks for 3 minutes) | Manager can switch to other suites, but can reopen if new tasks arrive |
| **Complete** | All tasks finished | Environment cleanup runs, but suite can reopen with new tasks |
| **Cancelled** | User explicitly cancelled | Terminal state, all pending tasks are cancelled |

**Key Insight:** Suites are **long-lived** and **reusable**. A suite can cycle through Open → Closed → Complete → Open multiple times as you submit tasks in batches.

#### State Transitions

**Auto-Close (3 minutes):**
```
Suite is Open → No new tasks for 3 minutes → Suite becomes Closed

Why? Allows Node Manager to work on other suites while you prepare the next batch.
Can reopen? Yes! Submit a new task and it reopens automatically.
```

**Completion:**
```
All pending tasks finished → Suite becomes Complete

What happens? Environment cleanup hook runs.
Can reopen? Yes! Submit a new task to restart the suite.
```

**Cancellation:**
```
User cancels suite → State becomes Cancelled (permanent)

What happens? All pending/running tasks are cancelled.
Can reopen? No, cancelled is a terminal state.
```

### Environment Hooks

Environment hooks are commands that run at the suite level (not per-task).

**Preparation Hook** (runs before any tasks execute):
```bash
# Example: ML training setup
env_preparation:
  args: ["./setup.sh", "--download-dataset"]
  envs:
    DATA_DIR: "/mnt/data"
    CUDA_VISIBLE_DEVICES: "0,1"
  resources:
    - Download setup.sh from S3
  timeout: "5m"
```

The hook has access to context variables:
```bash
MITOSIS_TASK_SUITE_UUID=550e8400-e29b-41d4-a716-446655440000
MITOSIS_TASK_SUITE_NAME="ML Training Campaign"
MITOSIS_GROUP_NAME="ml-team"
MITOSIS_WORKER_COUNT=16
MITOSIS_NODE_MANAGER_ID=abcd1234-...
```

**Cleanup Hook** (runs after all tasks complete):
```bash
# Example: Upload results and clean up
env_cleanup:
  args: ["./cleanup.sh"]
  envs:
    RESULTS_BUCKET: "s3://my-results/q4-2024/"
  timeout: "2m"
```

**When do hooks run?**
- Preparation: When Node Manager starts executing the suite (before spawning workers)
- Cleanup: When suite becomes Complete (all tasks finished or cancelled)

**What if a hook fails?**
- Preparation failure: Suite is aborted, manager tries next suite
- Cleanup failure: Error is logged, but suite still marked as complete

### Worker Allocation

Task Suites specify how many workers to spawn and how to allocate resources:

```yaml
worker_schedule:
  worker_count: 16          # Spawn 16 workers
  cpu_binding:
    cores: [0, 1, 2, 3]    # Use CPU cores 0-3
    strategy: RoundRobin    # Distribute workers across cores
  task_prefetch_count: 32   # Buffer 32 tasks locally
```

**CPU Binding Strategies:**

| Strategy | Behavior | When to Use |
|----------|----------|-------------|
| **RoundRobin** | Workers cycle through cores (worker 0→core 0, worker 1→core 1, ...) | General purpose, balanced load |
| **Exclusive** | Each worker gets dedicated core(s) | CPU-intensive tasks, avoid contention |
| **Shared** | All workers share all cores | I/O-bound tasks, maximize concurrency |

### Task Counting

Each suite tracks two counters:

- **total_tasks**: Total tasks ever submitted to this suite (monotonically increasing)
- **pending_tasks**: Currently active/pending tasks (increases on submit, decreases on completion)

This lets you track progress:
```
Suite "ML Campaign"
  Total: 1000 tasks submitted
  Pending: 127 tasks remaining
  → 873 tasks completed (87.3% done)
```

### Tags and Labels

**Tags** (for matching suites to managers):
```yaml
tags: ["gpu", "linux", "cuda:11.8"]

Meaning: This suite requires managers with GPU, Linux, and CUDA 11.8.
Matching: Manager must have ALL these tags (subset matching).
```

**Labels** (for querying and filtering):
```yaml
labels: ["project:resnet", "phase:training", "experiment:run-42"]

Meaning: Arbitrary metadata for organizing suites.
Usage: Query suites with labels like "project:resnet".
```

## How Task Suites Work with Node Managers

Task Suites don't execute in isolation—they require a **Node Manager** to:
1. Execute environment preparation
2. Spawn the requested number of workers
3. Monitor task execution
4. Run environment cleanup

See the [Node Manager guide](./node-manager.md) for details on how suites are assigned and executed.

## Getting Started

### Creating a Task Suite

```bash
# Create a suite for ML training
curl -X POST https://coordinator/suites \
  -H "Authorization: Bearer $TOKEN" \
  -d '{
    "name": "ResNet Training",
    "group_name": "ml-team",
    "tags": ["gpu", "linux"],
    "labels": ["project:resnet"],
    "worker_schedule": {
      "worker_count": 8,
      "task_prefetch_count": 16
    },
    "env_preparation": {
      "args": ["./setup.sh"],
      "timeout": "5m"
    },
    "env_cleanup": {
      "args": ["./cleanup.sh"],
      "timeout": "2m"
    }
  }'
```

### Submitting Tasks to a Suite

```bash
# Submit a task to the suite
curl -X POST https://coordinator/tasks \
  -H "Authorization: Bearer $TOKEN" \
  -d '{
    "group_name": "ml-team",
    "suite_uuid": "550e8400-...",  # Link task to suite
    "task_spec": {
      "args": ["python", "train.py", "--lr=0.01"]
    }
  }'
```

### Monitoring Suite Progress

```bash
# Get suite details
curl https://coordinator/suites/550e8400-... \
  -H "Authorization: Bearer $TOKEN"

# Response shows:
{
  "state": "Open",
  "total_tasks": 1000,
  "pending_tasks": 127,
  "assigned_managers": ["mgr-1", "mgr-2"]
}
```

### Cancelling a Suite

```bash
# Cancel entire suite
curl -X POST https://coordinator/suites/550e8400-.../cancel \
  -H "Authorization: Bearer $TOKEN" \
  -d '{
    "cancel_running_tasks": true
  }'
```

## Best Practices

1. **Use suites for related tasks**: Don't mix unrelated workloads in the same suite
2. **Keep preparation hooks fast**: Long setup delays all tasks
3. **Handle cleanup failures gracefully**: Don't rely on cleanup for critical operations
4. **Use meaningful names and labels**: Makes monitoring and debugging easier
5. **Set appropriate worker counts**: Match your hardware capabilities
6. **Test hooks locally first**: Debug setup/cleanup scripts before deploying

## Summary

Task Suites bring batch job capabilities to Mitosis:
- **Environment orchestration** eliminates redundant setup
- **Unified lifecycle** simplifies monitoring and control
- **Resource coordination** prevents conflicts
- **Long-lived and reusable** for iterative workflows

Next, learn how [Node Managers](./node-manager.md) execute Task Suites on your infrastructure.
