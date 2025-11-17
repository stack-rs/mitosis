# Phase 9: Documentation and Deployment Implementation Guide

## Overview

Phase 9 is the final phase of the Task Suite and Node Manager implementation. This phase focuses on comprehensive documentation, monitoring setup, and production deployment of the new distributed task orchestration system.

**Timeline:** 1 week

**Purpose:** Ensure the system is production-ready with complete documentation, monitoring infrastructure, and a safe deployment strategy.

## Prerequisites

- **All Phases 1-8 completed and tested:**
  - Phase 1: Database Schema ✓
  - Phase 2: API Schema and Coordinator Endpoints ✓
  - Phase 3: WebSocket Manager ✓
  - Phase 4: Node Manager Core ✓
  - Phase 5: Environment Hooks ✓
  - Phase 6: Worker Spawning and IPC ✓
  - Phase 7: Managed Worker Mode ✓
  - Phase 8: Integration and E2E Testing ✓

- **System ready for production:**
  - All E2E tests passing
  - Performance targets met
  - No critical bugs
  - Security review completed

## Timeline

**Total Duration:** 1 week

**Breakdown:**
- Documentation: 3 days
- Monitoring and Alerts: 1 day
- Staging Deployment: 1 day
- Production Rollout Planning: 1 day
- Production Deployment: 1 day (phased)

## Design References

### Architecture Components

The system consists of three main components:

1. **Coordinator** - Central orchestration service with WebSocket manager
2. **Node Manager** - Device-level service managing worker pools
3. **Managed Workers** - Workers spawned by Node Manager, communicating via IPC

### Key Concepts

- **Task Suite:** A collection of related tasks with shared lifecycle and execution environment
- **Tag Matching:** Algorithm to match suites to managers based on tag sets
- **Environment Hooks:** Scripts executed before/after suite execution (env_preparation, env_cleanup)
- **Task Pre-fetching:** Manager fetches multiple tasks ahead of time to reduce latency
- **Managed Worker:** Worker spawned by Node Manager, communicating via IPC (anonymous to coordinator)
- **Independent Worker:** Worker registered directly with coordinator via HTTP (existing mode)

### State Machines

**Task Suite States:**
- `Queued` → `Open` → `Executing` → `Complete`
- Also: `Cancelled`, `Failed`
- Auto-close timeout: 3 minutes of inactivity

**Node Manager States:**
- `Idle` → `Assigned` → `Preparing` → `Executing` → `Cleaning` → `Idle`
- Also: `Offline`, `Failed`

## Implementation Tasks

### Task 9.1: User Documentation

Create comprehensive user-facing documentation covering all aspects of the Task Suite and Node Manager system.

#### 9.1.1 Suite Creation Guide

**File:** `docs/user-guide/suite-creation.md`

**Content:**
- **What is a Task Suite?** - Overview and benefits
- **When to use suites** - Use cases (batch jobs, ML campaigns, CI/CD, log processing)
- **Creating a suite** - API examples with curl
  ```bash
  # Example: Create a suite
  curl -X POST https://api.example.com/suites \
    -H "Authorization: Bearer $TOKEN" \
    -H "Content-Type: application/json" \
    -d '{
      "name": "ML Training Campaign",
      "group_name": "ml-team",
      "description": "October 2024 ResNet training",
      "labels": ["project:resnet", "campaign:oct2024"],
      "tags": ["gpu", "linux", "cuda:12.0"],
      "worker_schedule": {
        "strategy": "Proportional",
        "worker_count": 16,
        "cpu_binding": "RoundRobin"
      },
      "env_preparation": {
        "command": "./scripts/setup_training_env.sh",
        "timeout_seconds": 300,
        "working_directory": "/opt/ml"
      },
      "env_cleanup": {
        "command": "./scripts/cleanup.sh",
        "timeout_seconds": 60
      },
      "auto_close_timeout_seconds": 180
    }'
  ```

- **Submitting tasks to a suite**
  ```bash
  # Submit tasks to the suite
  SUITE_UUID="550e8400-e29b-41d4-a716-446655440000"
  for i in {1..1000}; do
    curl -X POST https://api.example.com/tasks \
      -H "Authorization: Bearer $TOKEN" \
      -H "Content-Type: application/json" \
      -d '{
        "group_name": "ml-team",
        "suite_uuid": "'$SUITE_UUID'",
        "task_spec": {
          "model": "resnet50",
          "batch": '$i',
          "epochs": 10
        }
      }'
  done
  ```

- **Querying suite status**
  ```bash
  # Get suite details
  curl -X GET https://api.example.com/suites/$SUITE_UUID \
    -H "Authorization: Bearer $TOKEN"

  # Query suites with filters
  curl -X GET "https://api.example.com/suites?group_name=ml-team&state=Executing&labels=project:resnet" \
    -H "Authorization: Bearer $TOKEN"
  ```

- **Cancelling a suite**
  ```bash
  # Cancel suite and all running tasks
  curl -X POST https://api.example.com/suites/$SUITE_UUID/cancel \
    -H "Authorization: Bearer $TOKEN" \
    -H "Content-Type: application/json" \
    -d '{
      "reason": "User requested cancellation",
      "cancel_running_tasks": true
    }'
  ```

- **Managing suite-manager assignments**
  ```bash
  # Refresh tag-matched managers
  curl -X POST https://api.example.com/suites/$SUITE_UUID/managers/refresh \
    -H "Authorization: Bearer $TOKEN"

  # Add specific managers
  curl -X POST https://api.example.com/suites/$SUITE_UUID/managers \
    -H "Authorization: Bearer $TOKEN" \
    -H "Content-Type: application/json" \
    -d '{
      "manager_uuids": ["mgr-uuid-1", "mgr-uuid-2"]
    }'

  # Remove managers
  curl -X DELETE https://api.example.com/suites/$SUITE_UUID/managers \
    -H "Authorization: Bearer $TOKEN" \
    -H "Content-Type: application/json" \
    -d '{
      "manager_uuids": ["mgr-uuid-1"]
    }'
  ```

- **Suite lifecycle and state transitions**
- **Best practices** - Naming conventions, tag design, timeout settings

#### 9.1.2 Manager Deployment Guide

**File:** `docs/user-guide/manager-deployment.md`

**Content:**
- **Node Manager overview** - Purpose and architecture
- **System requirements**
  - OS: Linux (for iceoryx2 IPC)
  - CPU cores: At least N+1 cores for N workers
  - Memory: Depends on worker requirements
  - Network: Stable connection to coordinator

- **Installation**
  ```bash
  # Download binary
  curl -L https://releases.example.com/mitosis-manager-latest -o mitosis-manager
  chmod +x mitosis-manager

  # Or build from source
  cargo build --release --bin mitosis-manager
  ```

- **Configuration**
  ```yaml
  # config.yaml
  coordinator_url: "https://api.example.com"
  websocket_url: "wss://api.example.com/ws/managers"

  manager:
    name: "gpu-node-01"
    tags: ["gpu", "linux", "cuda:12.0", "datacenter:us-west"]
    max_workers: 16
    heartbeat_interval_seconds: 30

  groups:
    - name: "ml-team"
      role: "Worker"

  logging:
    level: "info"
    file: "/var/log/mitosis-manager.log"
  ```

- **Registration and authentication**
  ```bash
  # Register manager (one-time)
  curl -X POST https://api.example.com/managers \
    -H "Authorization: Bearer $USER_TOKEN" \
    -H "Content-Type: application/json" \
    -d '{
      "name": "gpu-node-01",
      "tags": ["gpu", "linux", "cuda:12.0"],
      "group_memberships": [
        {"group_name": "ml-team", "role": "Worker"}
      ]
    }' | jq -r .token > manager-token.txt
  ```

- **Running the manager**
  ```bash
  # Start manager
  ./mitosis-manager \
    --config config.yaml \
    --token-file manager-token.txt

  # Run as systemd service
  systemctl start mitosis-manager
  systemctl enable mitosis-manager
  ```

- **Manager lifecycle and states**
  - Idle: Waiting for suite assignment
  - Assigned: Received suite assignment
  - Preparing: Running env_preparation hook
  - Executing: Workers processing tasks
  - Cleaning: Running env_cleanup hook
  - Offline: Disconnected or crashed

- **Monitoring manager health**
  ```bash
  # Query manager status
  curl -X GET https://api.example.com/managers?tags=gpu \
    -H "Authorization: Bearer $TOKEN"
  ```

- **Systemd service example**
  ```ini
  # /etc/systemd/system/mitosis-manager.service
  [Unit]
  Description=Mitosis Node Manager
  After=network.target

  [Service]
  Type=simple
  User=mitosis
  WorkingDirectory=/opt/mitosis
  ExecStart=/opt/mitosis/mitosis-manager --config /etc/mitosis/config.yaml --token-file /etc/mitosis/token.txt
  Restart=always
  RestartSec=10

  [Install]
  WantedBy=multi-user.target
  ```

#### 9.1.3 Environment Hooks Examples

**File:** `docs/user-guide/environment-hooks.md`

**Content:**
- **Overview** - Purpose and execution model
- **Hook types:**
  - `env_preparation`: Runs before first worker starts
  - `env_cleanup`: Runs after all workers finish

- **Example 1: ML Model Training**
  ```bash
  #!/bin/bash
  # setup_training_env.sh

  set -e

  # Download dataset
  aws s3 sync s3://datasets/imagenet /data/imagenet

  # Download pretrained model
  wget https://models.example.com/resnet50-base.pth -O /tmp/model.pth

  # Create local index
  python build_index.py --input /data/imagenet --output /tmp/index.json

  echo "Environment preparation complete"
  ```

  ```bash
  #!/bin/bash
  # cleanup.sh

  # Upload results
  aws s3 sync /tmp/results s3://results/campaign-oct2024

  # Clean up local data
  rm -rf /data/imagenet /tmp/index.json /tmp/model.pth

  echo "Cleanup complete"
  ```

- **Example 2: Log Processing**
  ```bash
  #!/bin/bash
  # prepare_logs.sh

  # Download logs from S3
  aws s3 sync s3://logs/2024-10 /tmp/logs

  # Extract archives
  for file in /tmp/logs/*.gz; do
    gunzip "$file"
  done

  # Build index
  find /tmp/logs -name "*.log" > /tmp/log-files.txt
  ```

- **Example 3: CI/CD Environment**
  ```bash
  #!/bin/bash
  # setup_build_env.sh

  # Clone repository
  git clone https://github.com/example/repo.git /tmp/repo
  cd /tmp/repo

  # Install dependencies
  npm install

  # Run tests
  npm test
  ```

- **Best practices:**
  - Use absolute paths
  - Set appropriate timeouts
  - Handle errors gracefully
  - Log progress for debugging
  - Clean up resources in cleanup hook
  - Test hooks independently before using in production

- **Timeout handling** - What happens when hooks timeout
- **Failure handling** - Manager aborts suite, tries next one

#### 9.1.4 Troubleshooting Guide

**File:** `docs/user-guide/troubleshooting.md`

**Content:**

**Common Issues and Solutions:**

1. **Suite stays in Queued state**
   - **Cause:** No managers with matching tags
   - **Solution:**
     - Check manager tags: `GET /managers?tags=gpu`
     - Add managers with matching tags
     - Or explicitly assign managers: `POST /suites/{uuid}/managers`
     - Or update suite tags

2. **Manager shows as Offline**
   - **Cause:** Network issues, crashed process, heartbeat timeout
   - **Solution:**
     - Check manager logs: `/var/log/mitosis-manager.log`
     - Verify network connectivity to coordinator
     - Restart manager service: `systemctl restart mitosis-manager`
     - Check WebSocket connection status

3. **Environment preparation fails**
   - **Cause:** Script errors, timeout, missing dependencies
   - **Solution:**
     - Check manager logs for error details
     - Test hook script independently
     - Increase timeout if needed
     - Verify all dependencies are installed
     - Check file permissions and paths

4. **Workers crash immediately**
   - **Cause:** Invalid IPC configuration, missing binaries, resource limits
   - **Solution:**
     - Check iceoryx2 service status
     - Verify worker binary path
     - Check CPU core availability
     - Review file descriptor limits: `ulimit -n`
     - Check memory availability

5. **Tasks stuck in Running state**
   - **Cause:** Worker crash, network partition, task deadlock
   - **Solution:**
     - Check worker status via manager
     - Manager auto-respawns crashed workers
     - After 3 worker failures, task aborts
     - Cancel task manually if needed

6. **Suite doesn't auto-close**
   - **Cause:** Tasks still being submitted, timeout not reached
   - **Solution:**
     - Wait for 3-minute inactivity period
     - Or manually complete suite: `POST /suites/{uuid}/cancel`
     - Check for background processes still submitting tasks

7. **High task fetch latency**
   - **Cause:** Network issues, coordinator overload, prefetch disabled
   - **Solution:**
     - Enable task pre-fetching (default: 32 tasks)
     - Check network latency to coordinator
     - Monitor coordinator metrics
     - Use WebSocket multiplexing (enabled by default)

8. **Manager JWT token expired**
   - **Cause:** Token lifetime exceeded (default: 30 days)
   - **Solution:**
     - Manager auto-refreshes when < 24h remaining
     - Manually refresh: `POST /managers/refresh-token`
     - Check token expiry: decode JWT

9. **Database connection errors**
   - **Cause:** Connection pool exhausted, network issues
   - **Solution:**
     - Check coordinator logs
     - Review database connection pool settings
     - Check PostgreSQL max_connections
     - Monitor active connections

10. **WebSocket connection keeps dropping**
    - **Cause:** Network instability, firewall, load balancer timeout
    - **Solution:**
      - Manager auto-reconnects with exponential backoff
      - Check firewall rules
      - Verify load balancer WebSocket support
      - Increase load balancer timeout settings

**Debugging Commands:**

```bash
# Check suite status
curl -X GET https://api.example.com/suites/$SUITE_UUID \
  -H "Authorization: Bearer $TOKEN" | jq

# List all managers
curl -X GET https://api.example.com/managers \
  -H "Authorization: Bearer $TOKEN" | jq

# Query tasks in a suite
curl -X GET "https://api.example.com/tasks?suite_uuid=$SUITE_UUID" \
  -H "Authorization: Bearer $TOKEN" | jq

# View manager logs
journalctl -u mitosis-manager -f

# Check iceoryx2 services
iceoryx2-introspection

# Monitor system resources
htop
iostat -x 5
```

**Performance Tuning:**

- **Task pre-fetch count:** Adjust based on task execution time
- **Worker count:** Balance between parallelism and resource usage
- **CPU binding strategy:** RoundRobin (default), Exclusive, Shared
- **Heartbeat interval:** Lower for faster failure detection, higher for less overhead
- **Connection pool size:** Tune based on concurrent managers

### Task 9.2: API Documentation

Update API documentation with complete suite and manager endpoint specifications.

#### 9.2.1 OpenAPI Spec Update

**File:** `api/openapi.yaml`

**Update sections:**

1. **Suite Management Endpoints:**
   - `POST /suites` - Create task suite
   - `GET /suites` - Query suites with filters
   - `GET /suites/{uuid}` - Get suite details
   - `POST /suites/{uuid}/cancel` - Cancel suite
   - `POST /suites/{uuid}/managers/refresh` - Refresh tag-matched managers
   - `POST /suites/{uuid}/managers` - Add managers explicitly
   - `DELETE /suites/{uuid}/managers` - Remove managers

2. **Manager Endpoints:**
   - `POST /managers` - Register node manager
   - `POST /managers/heartbeat` - Manager heartbeat
   - `GET /managers` - Query managers
   - `GET /managers/{uuid}` - Get manager details
   - `POST /managers/refresh-token` - Refresh JWT token

3. **Task Endpoint Updates:**
   - Add `suite_uuid` field to `POST /tasks`
   - Document backward compatibility

4. **Schema Definitions:**
   ```yaml
   TaskSuite:
     type: object
     properties:
       uuid:
         type: string
         format: uuid
       name:
         type: string
       description:
         type: string
       state:
         type: string
         enum: [Queued, Open, Executing, Complete, Cancelled, Failed]
       group_name:
         type: string
       labels:
         type: array
         items:
           type: string
       tags:
         type: array
         items:
           type: string
       worker_schedule:
         $ref: '#/components/schemas/WorkerSchedulePlan'
       env_preparation:
         $ref: '#/components/schemas/EnvHookSpec'
       env_cleanup:
         $ref: '#/components/schemas/EnvHookSpec'
       total_tasks:
         type: integer
       pending_tasks:
         type: integer
       completed_tasks:
         type: integer
       failed_tasks:
         type: integer
       auto_close_timeout_seconds:
         type: integer
       created_at:
         type: string
         format: date-time
       updated_at:
         type: string
         format: date-time

   NodeManager:
     type: object
     properties:
       uuid:
         type: string
         format: uuid
       name:
         type: string
       state:
         type: string
         enum: [Idle, Assigned, Preparing, Executing, Cleaning, Offline]
       tags:
         type: array
         items:
           type: string
       assigned_suite_uuid:
         type: string
         format: uuid
       metrics:
         type: object
         properties:
           active_workers:
             type: integer
           tasks_completed:
             type: integer
           tasks_failed:
             type: integer
       created_at:
         type: string
         format: date-time
       last_heartbeat:
         type: string
         format: date-time
   ```

#### 9.2.2 Code Examples

**File:** `docs/api/examples.md`

**Content:**

1. **Python SDK examples**
   ```python
   import requests

   class MitosisClient:
       def __init__(self, base_url, token):
           self.base_url = base_url
           self.headers = {"Authorization": f"Bearer {token}"}

       def create_suite(self, suite_spec):
           """Create a new task suite."""
           response = requests.post(
               f"{self.base_url}/suites",
               json=suite_spec,
               headers=self.headers
           )
           response.raise_for_status()
           return response.json()

       def submit_task(self, task_spec, suite_uuid=None):
           """Submit a task, optionally to a suite."""
           payload = task_spec.copy()
           if suite_uuid:
               payload["suite_uuid"] = suite_uuid

           response = requests.post(
               f"{self.base_url}/tasks",
               json=payload,
               headers=self.headers
           )
           response.raise_for_status()
           return response.json()

       def get_suite(self, suite_uuid):
           """Get suite details."""
           response = requests.get(
               f"{self.base_url}/suites/{suite_uuid}",
               headers=self.headers
           )
           response.raise_for_status()
           return response.json()

       def wait_for_suite(self, suite_uuid, poll_interval=5):
           """Wait for suite to complete."""
           import time
           while True:
               suite = self.get_suite(suite_uuid)
               if suite["state"] in ["Complete", "Cancelled", "Failed"]:
                   return suite
               time.sleep(poll_interval)

   # Usage
   client = MitosisClient("https://api.example.com", token="...")

   # Create suite
   suite = client.create_suite({
       "name": "ML Training",
       "group_name": "ml-team",
       "tags": ["gpu"],
       "worker_schedule": {
           "strategy": "Proportional",
           "worker_count": 16
       }
   })

   # Submit tasks
   for i in range(1000):
       client.submit_task({
           "group_name": "ml-team",
           "task_spec": {"batch": i}
       }, suite_uuid=suite["uuid"])

   # Wait for completion
   final_suite = client.wait_for_suite(suite["uuid"])
   print(f"Suite completed: {final_suite['completed_tasks']} tasks")
   ```

2. **Bash script examples** (already included in user guide)

3. **Go SDK examples**
   ```go
   package main

   import (
       "bytes"
       "encoding/json"
       "fmt"
       "net/http"
   )

   type Client struct {
       baseURL string
       token   string
       http    *http.Client
   }

   func (c *Client) CreateSuite(spec map[string]interface{}) (map[string]interface{}, error) {
       body, _ := json.Marshal(spec)
       req, _ := http.NewRequest("POST", c.baseURL+"/suites", bytes.NewReader(body))
       req.Header.Set("Authorization", "Bearer "+c.token)
       req.Header.Set("Content-Type", "application/json")

       resp, err := c.http.Do(req)
       if err != nil {
           return nil, err
       }
       defer resp.Body.Close()

       var result map[string]interface{}
       json.NewDecoder(resp.Body).Decode(&result)
       return result, nil
   }
   ```

4. **Error handling examples**
   ```python
   try:
       suite = client.create_suite(spec)
   except requests.HTTPError as e:
       if e.response.status_code == 400:
           print(f"Invalid request: {e.response.json()}")
       elif e.response.status_code == 403:
           print("Permission denied")
       elif e.response.status_code == 409:
           print("Conflict: suite already exists")
       else:
           raise
   ```

### Task 9.3: Internal Documentation

Create comprehensive internal documentation for developers and operators.

#### 9.3.1 Architecture Diagrams

**File:** `docs/architecture/overview.md`

**Content:**

1. **System Architecture Diagram**
   ```
   ┌─────────────────────────────────────────────────────────────────┐
   │                         Coordinator                              │
   │  ┌──────────────┐  ┌──────────────┐  ┌──────────────────────┐  │
   │  │ HTTP API     │  │ WebSocket    │  │ Background Tasks     │  │
   │  │ (Workers)    │  │ Manager      │  │ - Suite state sync   │  │
   │  │              │  │              │  │ - Heartbeat monitor  │  │
   │  └──────┬───────┘  └──────┬───────┘  └──────────────────────┘  │
   │         │                  │                                     │
   │         └──────────┬───────┴─────────────────┐                  │
   │                    │                          │                  │
   │         ┌──────────▼──────────┐    ┌─────────▼────────┐        │
   │         │ Task Queue          │    │ Manager Registry │        │
   │         │ (PostgreSQL)        │    │ (PostgreSQL)     │        │
   │         └─────────────────────┘    └──────────────────┘        │
   └─────────────────────────────────────────────────────────────────┘
                      │                            ▲
                      │ HTTP                       │ WebSocket
                      │ (Independent)              │ (Managed)
                      │                            │
   ┌──────────────────▼────────────────────────────┴─────────────────┐
   │                      Node Manager (per device)                   │
   │  ┌──────────────┐  ┌──────────────┐  ┌──────────────────────┐  │
   │  │ WebSocket    │  │ Worker Pool  │  │ IPC Services         │  │
   │  │ Client       │  │ Manager      │  │ (iceoryx2)           │  │
   │  └──────┬───────┘  └──────┬───────┘  └──────┬───────────────┘  │
   │         │                  │                  │                  │
   │         │    ┌─────────────▼──────────────────▼───────┐         │
   │         │    │ Environment Hook Executor              │         │
   │         │    │ - env_preparation                       │         │
   │         │    │ - env_cleanup                           │         │
   │         │    └────────────────────────────────────────┘         │
   └─────────┴──────────────────┬───────────────────────────────────┘
                                 │ IPC (iceoryx2)
                    ┌────────────┴────────────┐
                    │                         │
          ┌─────────▼──────────┐   ┌─────────▼──────────┐
          │ Managed Worker 0   │   │ Managed Worker N   │
          │ - Task fetch (IPC) │   │ - Task fetch (IPC) │
          │ - Task report      │   │ - Task report      │
          │ - Task execution   │   │ - Task execution   │
          └────────────────────┘   └────────────────────┘
   ```

2. **Data Flow Diagram**
   - Independent worker flow (existing)
   - Managed worker flow (new)
   - Suite lifecycle flow

3. **Component Responsibilities**
   - Coordinator: Suite management, manager registry, task queue
   - Node Manager: Worker spawning, IPC services, environment hooks
   - Managed Worker: Task execution via IPC

#### 9.3.2 State Machine Diagrams

**File:** `docs/architecture/state-machines.md`

**Content:**

1. **Task Suite State Machine**
   ```
                       ┌─────────────────────┐
                       │                     │
              submit   │    new task         │  submit new task
              tasks    │    (pending > 0)    │  (reopen)
                ┌──────┴─────────┬───────────┴──────┐
                │                │                   │
           ┌────▼────┐      ┌────▼────┐        ┌────▼────┐
           │ Queued  │      │  Open   │        │Complete │
           └────┬────┘      └────┬────┘        └─────────┘
                │                │
                │ manager        │ manager
                │ assigned       │ starts execution
                │                │
           ┌────▼────┐      ┌────▼──────┐
           │  Open   ├──────► Executing │
           └─────────┘      └────┬──────┘
                                 │
                                 │ all tasks complete
                                 │ OR 3min inactivity
                                 │
                            ┌────▼────┐
                            │Complete │
                            └─────────┘

   Cancel transitions (from any state):
   - User cancellation → Cancelled
   - Critical failure → Failed
   ```

2. **Node Manager State Machine**
   ```
   ┌────────────────────────────────────────────────────────────────┐
   │                         Manager Lifecycle                       │
   └────────────────────────────────────────────────────────────────┘

        ┌─────────┐
        │ Offline │ (initial state)
        └────┬────┘
             │ register + connect
        ┌────▼────┐ ◄──────────────────────────────┐
        │  Idle   │                                 │
        └────┬────┘                                 │
             │ receive SuiteAssigned                │
        ┌────▼────────┐                             │
        │  Assigned   │                             │
        └────┬────────┘                             │
             │ start env_preparation                │
        ┌────▼──────────┐                           │
        │  Preparing    │───► (hook fails) ─────────┤
        └────┬──────────┘                           │
             │ hook success                         │
        ┌────▼──────────┐                           │
        │  Executing    │ ◄─┐                       │
        └────┬──────────┘   │                       │
             │               │ fetch next task       │
             │               └───────────────────────┤
             │ all tasks complete OR suite cancelled │
        ┌────▼──────────┐                           │
        │  Cleaning     │───► (hook fails) ─────────┤
        └────┬──────────┘                           │
             │ hook success                         │
             └──────────────────────────────────────┘

   Failure transitions:
   - Network disconnect → Offline (auto-reconnect)
   - Heartbeat timeout → Offline (coordinator marks it)
   - Critical error → Offline
   ```

3. **Worker Lifecycle State Machine**
   ```
   ┌─────────┐
   │ Spawned │
   └────┬────┘
        │ connect to IPC
   ┌────▼────┐
   │  Ready  │ ◄──────────────┐
   └────┬────┘                │
        │ fetch task          │
   ┌────▼──────────┐          │
   │  Processing   │          │
   └────┬──────────┘          │
        │ complete/fail       │
        └─────────────────────┘

   Exit states:
   - Success (task complete)
   - Crash (manager respawns)
   - Shutdown (suite complete)
   ```

#### 9.3.3 Deployment Runbook

**File:** `docs/operations/deployment-runbook.md`

**Content:**

1. **Pre-deployment Checklist**
   - [ ] All tests passing
   - [ ] Database migration tested
   - [ ] Rollback plan documented
   - [ ] Feature flag configured
   - [ ] Monitoring dashboards ready
   - [ ] Alert rules configured
   - [ ] On-call engineer assigned
   - [ ] Stakeholders notified

2. **Database Migration**
   ```bash
   # Backup database
   pg_dump -h $DB_HOST -U $DB_USER -d mitosis > backup-$(date +%Y%m%d).sql

   # Run migrations
   sqlx migrate run

   # Verify schema
   psql -h $DB_HOST -U $DB_USER -d mitosis -c "\d task_suites"
   ```

3. **Coordinator Deployment**
   ```bash
   # Step 1: Deploy to staging
   ./deploy.sh staging coordinator v0.7.0

   # Step 2: Verify staging
   curl https://staging-api.example.com/health

   # Step 3: Deploy to production (with feature flag)
   export ENABLE_TASK_SUITES=false
   ./deploy.sh production coordinator v0.7.0

   # Step 4: Enable feature flag gradually
   # 10% traffic
   ./feature-flag.sh set enable_task_suites 10

   # Monitor for 1 hour, then increase
   # 50% traffic
   ./feature-flag.sh set enable_task_suites 50

   # Monitor for 4 hours, then full rollout
   # 100% traffic
   ./feature-flag.sh set enable_task_suites 100
   ```

4. **Node Manager Deployment**
   ```bash
   # Deploy to 10% of machines
   ./deploy-managers.sh --percentage 10

   # Monitor for issues
   ./monitor-managers.sh

   # Increase to 50%
   ./deploy-managers.sh --percentage 50

   # Full rollout
   ./deploy-managers.sh --percentage 100
   ```

5. **Smoke Tests**
   ```bash
   # Create test suite
   SUITE_UUID=$(./create-test-suite.sh)

   # Submit test tasks
   ./submit-test-tasks.sh $SUITE_UUID 100

   # Wait for completion
   ./wait-for-suite.sh $SUITE_UUID

   # Verify results
   ./verify-suite-results.sh $SUITE_UUID
   ```

6. **Rollback Procedure** (see section below)

7. **Post-deployment Verification**
   - [ ] Suite creation working
   - [ ] Manager registration working
   - [ ] Task execution via managed workers
   - [ ] Independent workers still functioning
   - [ ] Metrics showing healthy state
   - [ ] No error spikes in logs
   - [ ] Database performance stable

### Task 9.4: Monitoring and Alerts

Set up comprehensive monitoring and alerting infrastructure.

#### 9.4.1 Prometheus Metrics

**File:** `src/metrics.rs`

**Metrics to add:**

1. **Suite Metrics**
   ```rust
   // Counter: Total suites created
   mitosis_suites_created_total{group_name, state}

   // Gauge: Active suites by state
   mitosis_suites_active{state}

   // Histogram: Suite execution duration
   mitosis_suite_duration_seconds{group_name}

   // Counter: Suite state transitions
   mitosis_suite_state_transitions_total{from_state, to_state}

   // Gauge: Tasks per suite
   mitosis_suite_tasks{suite_uuid, state}
   ```

2. **Manager Metrics**
   ```rust
   // Gauge: Active managers by state
   mitosis_managers_active{state}

   // Counter: Manager state transitions
   mitosis_manager_state_transitions_total{from_state, to_state}

   // Gauge: Workers per manager
   mitosis_manager_workers{manager_uuid}

   // Counter: Manager reconnections
   mitosis_manager_reconnections_total{manager_uuid}

   // Histogram: Heartbeat latency
   mitosis_manager_heartbeat_latency_seconds{manager_uuid}
   ```

3. **Task Metrics**
   ```rust
   // Histogram: Task fetch latency (managed workers)
   mitosis_task_fetch_latency_seconds{via="ipc"}

   // Counter: Task prefetch hits/misses
   mitosis_task_prefetch_total{result="hit"|"miss"}

   // Gauge: Prefetch buffer size
   mitosis_manager_prefetch_buffer_size{manager_uuid}

   // Counter: Task execution via managed workers
   mitosis_tasks_executed_total{via="managed"|"independent", result="success"|"failure"}
   ```

4. **Environment Hook Metrics**
   ```rust
   // Histogram: Hook execution duration
   mitosis_env_hook_duration_seconds{hook_type="preparation"|"cleanup", result}

   // Counter: Hook failures
   mitosis_env_hook_failures_total{hook_type}
   ```

5. **WebSocket Metrics**
   ```rust
   // Gauge: Active WebSocket connections
   mitosis_websocket_connections{type="manager"}

   // Counter: WebSocket messages
   mitosis_websocket_messages_total{direction="sent"|"received", message_type}

   // Histogram: WebSocket message latency
   mitosis_websocket_message_latency_seconds{message_type}
   ```

#### 9.4.2 Grafana Dashboards

**File:** `monitoring/grafana/task-suites-dashboard.json`

**Dashboard Panels:**

1. **Overview Panel**
   - Active suites by state (pie chart)
   - Suite creation rate (graph)
   - Task execution rate (graph)
   - Active managers by state (pie chart)

2. **Suite Performance Panel**
   - Suite execution duration (histogram)
   - Tasks per suite distribution
   - Suite state transition timeline
   - Completion rate

3. **Manager Health Panel**
   - Manager states over time
   - Heartbeat latency by manager
   - Worker count by manager
   - Manager reconnection events

4. **Task Execution Panel**
   - Task fetch latency (managed vs independent)
   - Prefetch hit/miss ratio
   - Task execution rate by worker type
   - Task failure rate

5. **Environment Hooks Panel**
   - Hook execution duration
   - Hook success/failure rate
   - Hook timeout events

6. **Resource Utilization Panel**
   - Database connection pool usage
   - WebSocket connection count
   - API request rate
   - Memory and CPU usage

#### 9.4.3 Alert Rules

**File:** `monitoring/prometheus/alerts.yaml`

**Alert Rules:**

```yaml
groups:
  - name: task_suites
    interval: 30s
    rules:
      # High suite failure rate
      - alert: HighSuiteFailureRate
        expr: |
          rate(mitosis_suite_state_transitions_total{to_state="Failed"}[5m]) > 0.1
        for: 5m
        labels:
          severity: warning
        annotations:
          summary: "High suite failure rate"
          description: "Suite failure rate is {{ $value }} failures/sec"

      # Suite stuck in Queued state
      - alert: SuitesStuckInQueue
        expr: |
          count(mitosis_suites_active{state="Queued"}) > 10
        for: 15m
        labels:
          severity: warning
        annotations:
          summary: "Multiple suites stuck in queue"
          description: "{{ $value }} suites stuck in Queued state for >15min"

      # No active managers
      - alert: NoActiveManagers
        expr: |
          sum(mitosis_managers_active) == 0
        for: 5m
        labels:
          severity: critical
        annotations:
          summary: "No active managers"
          description: "No node managers are currently active"

      # High manager offline rate
      - alert: HighManagerOfflineRate
        expr: |
          rate(mitosis_manager_state_transitions_total{to_state="Offline"}[5m]) > 0.5
        for: 5m
        labels:
          severity: warning
        annotations:
          summary: "High manager offline rate"
          description: "Managers going offline at {{ $value }}/sec"

      # High task fetch latency
      - alert: HighTaskFetchLatency
        expr: |
          histogram_quantile(0.95,
            rate(mitosis_task_fetch_latency_seconds_bucket[5m])
          ) > 1.0
        for: 10m
        labels:
          severity: warning
        annotations:
          summary: "High task fetch latency"
          description: "P95 task fetch latency is {{ $value }}s"

      # Environment hook failures
      - alert: HighEnvHookFailureRate
        expr: |
          rate(mitosis_env_hook_failures_total[5m]) > 0.05
        for: 5m
        labels:
          severity: warning
        annotations:
          summary: "High environment hook failure rate"
          description: "Hook failures at {{ $value }}/sec"

      # WebSocket connection drops
      - alert: WebSocketConnectionDrops
        expr: |
          rate(mitosis_websocket_connections[5m]) < -0.5
        for: 5m
        labels:
          severity: warning
        annotations:
          summary: "WebSocket connections dropping"
          description: "WebSocket connections dropping at {{ $value }}/sec"

      # Database connection pool saturation
      - alert: DatabasePoolSaturated
        expr: |
          mitosis_db_pool_connections_active / mitosis_db_pool_connections_max > 0.9
        for: 5m
        labels:
          severity: critical
        annotations:
          summary: "Database connection pool saturated"
          description: "Database pool at {{ $value }}% capacity"
```

### Task 9.5: Deployment Procedures

#### 9.5.1 Phased Rollout Plan

**Phase 1: Database Migration (Day 1)**

1. **Backup database**
   ```bash
   pg_dump -h $PROD_DB_HOST -U $DB_USER -d mitosis > backup-$(date +%Y%m%d-%H%M%S).sql
   gzip backup-*.sql
   aws s3 cp backup-*.sql.gz s3://backups/mitosis/
   ```

2. **Run migrations in transaction**
   ```bash
   # Test migration on staging first
   export DATABASE_URL=$STAGING_DB_URL
   sqlx migrate run

   # Verify staging
   ./verify-schema.sh staging

   # Production migration
   export DATABASE_URL=$PROD_DB_URL
   sqlx migrate run
   ```

3. **Verify schema**
   ```sql
   -- Verify new tables exist
   SELECT table_name FROM information_schema.tables
   WHERE table_schema = 'public'
   AND table_name IN ('task_suites', 'node_managers', 'task_suite_managers');

   -- Verify triggers
   SELECT trigger_name FROM information_schema.triggers
   WHERE trigger_schema = 'public';
   ```

**Phase 2: Coordinator Deployment (Day 2)**

1. **Deploy with feature flag disabled**
   ```bash
   # Build new version
   cargo build --release

   # Deploy to production (blue-green deployment)
   ./deploy.sh production coordinator v0.7.0 \
     --env ENABLE_TASK_SUITES=false \
     --env ENABLE_NODE_MANAGERS=false
   ```

2. **Verify independent workers still work**
   ```bash
   # Submit test task
   ./test-independent-worker.sh

   # Monitor metrics
   ./check-metrics.sh
   ```

3. **Enable suite creation (10% traffic)**
   ```bash
   ./feature-flag.sh set enable_task_suites 10
   ```

4. **Monitor for 2 hours**
   - Check error rates
   - Monitor database performance
   - Verify suite creation works

5. **Gradually increase traffic**
   ```bash
   # 50% traffic
   ./feature-flag.sh set enable_task_suites 50
   # Monitor for 4 hours

   # 100% traffic
   ./feature-flag.sh set enable_task_suites 100
   ```

**Phase 3: Node Manager Deployment (Day 3-4)**

1. **Deploy to 10% of machines**
   ```bash
   # Select 10% of machines
   ./select-deployment-targets.sh --percentage 10 > targets.txt

   # Deploy managers
   ansible-playbook -i targets.txt deploy-manager.yaml
   ```

2. **Create test suites**
   ```bash
   ./create-test-suite.sh --tags "test,canary"
   ```

3. **Monitor for 4 hours**
   - Manager registration
   - Suite assignment
   - Task execution
   - Environment hooks

4. **Increase to 50% of machines**
   ```bash
   ./select-deployment-targets.sh --percentage 50 > targets.txt
   ansible-playbook -i targets.txt deploy-manager.yaml
   ```

5. **Full rollout to 100%**
   ```bash
   ./select-deployment-targets.sh --percentage 100 > targets.txt
   ansible-playbook -i targets.txt deploy-manager.yaml
   ```

**Phase 4: Production Validation (Day 5)**

1. **Run production test suite**
   ```bash
   ./run-production-test.sh
   ```

2. **Monitor all metrics**
   - Suite creation and completion rates
   - Manager health
   - Task execution performance
   - Error rates

3. **Gather feedback from users**

4. **Document and announce feature**

#### 9.5.2 Rollback Plan

**Scenario: Critical issues found during rollout**

**Step 1: Disable suite creation**
```bash
# Disable via feature flag
./feature-flag.sh set enable_task_suites 0
```

**Step 2: Drain existing suites**
```bash
# Let existing suites complete
./monitor-suites.sh --wait-for-completion

# Or force cancel all suites
./cancel-all-suites.sh
```

**Step 3: Shut down node managers**
```bash
# Graceful shutdown (wait for tasks to complete)
ansible-playbook shutdown-managers.yaml --extra-vars "graceful=true"

# Or force shutdown
ansible-playbook shutdown-managers.yaml --extra-vars "graceful=false"
```

**Step 4: Revert coordinator to previous version**
```bash
# Blue-green: Switch traffic back to old version
./deploy.sh production coordinator v0.6.5 --switch-traffic

# Or rolling update
./deploy.sh production coordinator v0.6.5 --rolling
```

**Step 5: Database rollback (if needed)**
```bash
# Export suite data for analysis
pg_dump -h $PROD_DB_HOST -U $DB_USER -t task_suites -t node_managers \
  -t task_suite_managers > suite_data_backup.sql

# Drop new tables
psql -h $PROD_DB_HOST -U $DB_USER -d mitosis << EOF
DROP TABLE IF EXISTS task_execution_failures CASCADE;
DROP TABLE IF EXISTS task_suite_managers CASCADE;
DROP TABLE IF EXISTS group_node_manager CASCADE;
DROP TABLE IF EXISTS node_managers CASCADE;
ALTER TABLE active_tasks DROP COLUMN IF EXISTS task_suite_id;
DROP TABLE IF EXISTS task_suites CASCADE;
EOF
```

**Step 6: Archive logs for post-mortem**
```bash
# Archive coordinator logs
./archive-logs.sh coordinator
./archive-logs.sh managers
aws s3 sync /var/log/mitosis s3://incident-logs/$(date +%Y%m%d)/
```

**Step 7: Verify system health**
```bash
# Check independent workers
./verify-independent-workers.sh

# Check all metrics
./check-all-metrics.sh

# Run smoke tests
./smoke-test.sh
```

#### 9.5.3 Configuration Management

**File:** `docs/operations/configuration.md`

**Coordinator Configuration:**

```yaml
# /etc/mitosis/coordinator.yaml

server:
  host: "0.0.0.0"
  port: 8080
  workers: 4

database:
  url: "postgresql://user:pass@localhost/mitosis"
  pool_size: 50
  max_connections: 100
  connection_timeout_seconds: 30

websocket:
  heartbeat_interval_seconds: 30
  heartbeat_timeout_seconds: 90
  max_connections: 1000

task_suites:
  enabled: true
  default_auto_close_timeout_seconds: 180
  max_tasks_per_suite: 1000000
  prefetch_count: 32

node_managers:
  enabled: true
  lease_duration_seconds: 3600
  token_lifetime_days: 30

metrics:
  enabled: true
  port: 9090
  path: "/metrics"

logging:
  level: "info"
  format: "json"
  output: "/var/log/mitosis/coordinator.log"
```

**Node Manager Configuration:**

```yaml
# /etc/mitosis/manager.yaml

coordinator:
  url: "https://api.example.com"
  websocket_url: "wss://api.example.com/ws/managers"
  token_file: "/etc/mitosis/token.txt"

manager:
  name: "gpu-node-01"
  tags:
    - "gpu"
    - "linux"
    - "cuda:12.0"
    - "datacenter:us-west"
  max_workers: 16
  heartbeat_interval_seconds: 30

worker_defaults:
  cpu_binding: "RoundRobin"
  restart_on_failure: true
  max_restarts: 3

ipc:
  service_prefix: "mitosis_mgr"
  cleanup_on_exit: true

hooks:
  timeout_seconds: 300
  env:
    PATH: "/usr/local/bin:/usr/bin:/bin"

logging:
  level: "info"
  format: "json"
  output: "/var/log/mitosis/manager.log"
```

**Environment Variables:**

```bash
# Coordinator
export DATABASE_URL="postgresql://..."
export ENABLE_TASK_SUITES=true
export ENABLE_NODE_MANAGERS=true
export LOG_LEVEL=info

# Manager
export COORDINATOR_URL="https://api.example.com"
export MANAGER_TOKEN_FILE="/etc/mitosis/token.txt"
export LOG_LEVEL=info
```

## Testing Checklist

Phase 9 focuses on documentation and deployment, so testing is primarily validation:

### Documentation Testing
- [ ] All API examples run successfully
- [ ] Code examples compile and execute
- [ ] OpenAPI spec validates
- [ ] Links in documentation are not broken
- [ ] Screenshots and diagrams are up to date

### Monitoring Testing
- [ ] All Prometheus metrics are exported
- [ ] Grafana dashboards load and display data
- [ ] Alert rules trigger correctly in test environment
- [ ] Alert notifications reach on-call engineers
- [ ] Metrics align with documented schema

### Deployment Testing
- [ ] Database migration runs successfully on staging
- [ ] Rollback procedure works on staging
- [ ] Feature flags control suite creation correctly
- [ ] Blue-green deployment switches traffic correctly
- [ ] Graceful shutdown completes running tasks
- [ ] Configuration changes apply without restart (where applicable)

### End-to-End Validation
- [ ] Create test suite via documented API
- [ ] Deploy test manager using documented procedure
- [ ] Execute tasks through managed workers
- [ ] Verify all metrics appear in dashboard
- [ ] Trigger test alerts and verify notifications
- [ ] Independent workers still function correctly

## Success Criteria

Phase 9 is complete when:

1. **Documentation Complete:**
   - [ ] User guide published and accessible
   - [ ] API documentation updated with all new endpoints
   - [ ] Internal architecture docs reviewed by team
   - [ ] Troubleshooting guide covers common issues
   - [ ] All code examples tested and working

2. **Monitoring Live:**
   - [ ] Prometheus metrics exporting for all components
   - [ ] Grafana dashboards deployed and accessible
   - [ ] Alert rules configured and tested
   - [ ] On-call runbook documented
   - [ ] Metrics retention policy configured

3. **Deployment Successful:**
   - [ ] Database migration completed
   - [ ] Coordinator deployed to production
   - [ ] Node managers deployed to all target machines
   - [ ] Feature enabled for 100% of traffic
   - [ ] No increase in error rates
   - [ ] Performance metrics meet targets

4. **System Health:**
   - [ ] Independent workers continue functioning
   - [ ] Managed workers executing tasks successfully
   - [ ] Suite state transitions working correctly
   - [ ] Environment hooks executing properly
   - [ ] WebSocket connections stable
   - [ ] Database performance stable

5. **Operational Readiness:**
   - [ ] Rollback procedure documented and tested
   - [ ] On-call engineers trained
   - [ ] Incident response plan documented
   - [ ] Backup and restore procedures verified
   - [ ] Configuration management in place

6. **User Adoption:**
   - [ ] Feature announced to users
   - [ ] Sample suites created by early adopters
   - [ ] Feedback collected and documented
   - [ ] Known issues documented
   - [ ] Support channels established

## Dependencies

**Required Phases:**
- Phase 1: Database Schema ✓
- Phase 2: API Schema and Coordinator Endpoints ✓
- Phase 3: WebSocket Manager ✓
- Phase 4: Node Manager Core ✓
- Phase 5: Environment Hooks ✓
- Phase 6: Worker Spawning and IPC ✓
- Phase 7: Managed Worker Mode ✓
- Phase 8: Integration and E2E Testing ✓

**External Dependencies:**
- PostgreSQL database (production instance)
- Prometheus server (for metrics collection)
- Grafana (for dashboards)
- Alert manager (for notifications)
- Documentation hosting platform
- Deployment infrastructure (CI/CD, orchestration)

## Next Phase

**Production Deployment Complete!**

After Phase 9, the Task Suite and Node Manager system is fully operational in production.

**Post-deployment activities:**
1. Monitor system health and performance
2. Gather user feedback
3. Address any issues or bugs
4. Plan incremental improvements
5. Measure adoption and impact

**Future Enhancements (outside this RFC):**
- Suite templates and presets
- Advanced scheduling strategies
- Cost optimization features
- Multi-region support
- Enhanced analytics and reporting

---

## Appendix: Quick Reference

### Key Metrics to Monitor

| Metric | Target | Alert Threshold |
|--------|--------|-----------------|
| Suite creation rate | Variable | N/A |
| Suite completion time | < 10min (typical) | > 1hr (warning) |
| Task fetch latency (P95) | < 50ms | > 1s |
| Manager online percentage | > 95% | < 90% |
| Environment hook success rate | > 99% | < 95% |
| WebSocket reconnection rate | < 0.1/min | > 1/min |
| Database connection pool usage | < 70% | > 90% |

### Common Commands

```bash
# Create suite
curl -X POST $API/suites -H "Authorization: Bearer $TOKEN" -d @suite.json

# Query suites
curl -X GET "$API/suites?state=Executing" -H "Authorization: Bearer $TOKEN"

# Register manager
curl -X POST $API/managers -H "Authorization: Bearer $TOKEN" -d @manager.json

# Check manager status
curl -X GET $API/managers -H "Authorization: Bearer $TOKEN"

# View logs
journalctl -u mitosis-coordinator -f
journalctl -u mitosis-manager -f

# Check metrics
curl http://localhost:9090/metrics | grep mitosis_
```

### Emergency Contacts

- On-call Engineer: [PagerDuty rotation]
- Database Team: [Contact info]
- Infrastructure Team: [Contact info]
- Product Owner: [Contact info]

### Related Documentation

- RFC: `/home/user/mitosis/rfc.md`
- Architecture: `docs/architecture/overview.md`
- API Reference: `docs/api/reference.md`
- Operations Runbook: `docs/operations/runbook.md`
