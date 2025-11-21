# Phase 5: Environment Hooks Implementation Guide

## Overview

Phase 5 implements environment lifecycle hooks for Task Suites, enabling automated setup and teardown workflows. Environment hooks (`env_preparation` and `env_cleanup`) execute once per suite assignment, allowing shared resource initialization and cleanup across all workers in a suite.

**Timeline:** 1 week

**Key Features:**
- Parse and validate `EnvHookSpec` from JSONB database fields
- Download remote S3 resources (attachments/artifacts) before hook execution
- Execute preparation/cleanup commands with proper environment variable injection
- Handle timeouts and failures appropriately
- Integrate hooks into Node Manager state machine (PREPARING and CLEANUP states)

## Prerequisites

- **Phase 1-4 completed:**
  - Phase 1: Database schema with `task_suites` table containing `env_preparation` and `env_cleanup` JSONB fields
  - Phase 2: API schema with `EnvHookSpec`, `RemoteResource`, and `RemoteResourceDownload` types
  - Phase 3: WebSocket manager communication established
  - Phase 4: Node Manager core state machine (`NodeManagerState` enum with Preparing/Cleanup states)

- **Technical Knowledge:**
  - Process spawning with `tokio::process::Command`
  - S3 operations (presigned URL download)
  - JSONB parsing and deserialization
  - Timeout handling with `tokio::time::timeout`

## Timeline

**1 week** breakdown:
- Days 1-2: EnvHookSpec parsing, S3 resource download
- Days 3-4: Hook execution with timeout and env vars
- Days 5-6: State machine integration (PREPARING → EXECUTING, EXECUTING → CLEANUP)
- Day 7: Error handling, logging, and testing

## Piece-by-Piece Breakdown

Each piece is independently testable and delivers incremental value:

**Piece 5.1: EnvHookSpec parsing from database**
- Parse JSONB to EnvHookSpec struct
- Validate fields
- Deliverable: Can load hook specs from database

**Piece 5.2: S3 resource download**
- Download resources from S3 before hook
- Save to local paths
- Deliverable: Resources downloaded successfully

**Piece 5.3: Basic hook execution**
- Spawn process with args
- Set environment variables
- Wait for completion
- Deliverable: Can run simple hooks

**Piece 5.4: Hook timeout handling**
- Timeout enforcement
- Kill process on timeout
- Deliverable: Timeouts work correctly

**Piece 5.5: Context variable injection**
- Inject all MITOSIS_* env vars
- Suite UUID, name, group, worker count
- Deliverable: Hooks see correct context

**Piece 5.6: Integrate env_preparation into manager**
- Call during Preparing state
- Handle success/failure
- Deliverable: Preparation hooks run before workers spawn

**Piece 5.7: Integrate env_cleanup into manager**
- Call during Cleanup state
- Handle failures gracefully
- Deliverable: Cleanup hooks run after suite completes

## Design References

### Section 4.1: Task Suite - EnvHookSpec Definition

From RFC Section 4.1:

```rust
pub struct EnvHookSpec {
    pub args: Vec<String>,                 // Command to execute
    pub envs: HashMap<String, String>,     // Environment variables
    pub resources: Vec<RemoteResourceDownload>, // S3 files to download
    pub timeout: Duration,
}

pub struct RemoteResourceDownload {
    pub remote_file: RemoteResource,
    /// The relative local file path of the resource downloaded to at the cache directory.
    pub local_path: PathBuf,
}

pub enum RemoteResource {
    Artifact {
        uuid: Uuid,
        content_type: ArtifactContentType,
    },
    Attachment {
        key: String,
    },
}
```

**Database Representation (JSONB):**

From RFC Section 6.1:

```sql
-- Lifecycle hooks (TaskSpec-like, JSON)
env_preparation JSONB,
-- Example: {"args": ["./setup.sh"], "envs": {"DATA_DIR": "/mnt/data"}, "resources": [...], "timeout": "5m"}

env_cleanup JSONB,
-- Example: {"args": ["./cleanup.sh"], "envs": {}, "resources": [], "timeout": "2m"}
```

**Example JSON:**

```json
{
  "args": ["./setup.sh", "--download-dataset"],
  "envs": {"DATA_DIR": "/mnt/data"},
  "resources": [
    {"remote_file": {"Attachment": {"key": "setup.sh"}}, "local_path": "setup.sh"}
  ],
  "timeout": "5m"
}
```

### Section 4.1: Context Variables for Hooks

From RFC Section 4.1, environment hooks execute with these context variables:

```bash
MITOSIS_TASK_SUITE_UUID=550e8400-e29b-41d4-a716-446655440000
MITOSIS_TASK_SUITE_NAME="ML Training Campaign"
MITOSIS_GROUP_NAME="ml-team"
MITOSIS_WORKER_COUNT=16
MITOSIS_NODE_MANAGER_ID=abcd1234-...
```

**Context Variable Definitions:**
- `MITOSIS_TASK_SUITE_UUID`: UUID of the assigned task suite
- `MITOSIS_TASK_SUITE_NAME`: Optional display name of the suite (empty string if None)
- `MITOSIS_GROUP_NAME`: Name of the group that owns the suite
- `MITOSIS_WORKER_COUNT`: Number of workers to be spawned (from `WorkerSchedulePlan`)
- `MITOSIS_NODE_MANAGER_ID`: UUID of the executing node manager

### Section 9.3: Environment Preparation Workflow

From RFC Section 9.3:

```
┌─ Environment Preparation ─────────────────────────────────┐
│                                                             │
│  1. Download resources from S3                             │
│     - suite.env_preparation.resources[]                    │
│                                                             │
│  2. Execute preparation command                            │
│     - Args: suite.env_preparation.args                     │
│     - Envs: suite.env_preparation.envs + context vars      │
│     - Timeout: suite.env_preparation.timeout               │
│                                                             │
│  3. Check exit status                                      │
│     - Exit 0: Success → Continue                           │
│     - Non-zero: Failure → Abort suite, back to IDLE        │
│                                                             │
└─────────────────────────────────────────────────────────────┘
```

**State Transition:**

From RFC Section 9.2:

| State | Description | Activities |
|-------|-------------|------------|
| **Preparing** | Running env_preparation | - Execute setup hook<br>- Download resources from S3<br>- Set environment variables<br>- If fails: abort suite, back to Idle<br>- If succeeds: spawn workers |

### Section 9.3: Environment Cleanup Workflow

From RFC Section 9.3:

```
┌─ Environment Cleanup ─────────────────────────────────────┐
│                                                             │
│  1. Gracefully shutdown all workers                        │
│     - Send SIGTERM to each worker                          │
│     - Wait up to 30 seconds for clean shutdown             │
│                                                             │
│  2. Force kill any remaining workers                       │
│                                                             │
│  3. Execute cleanup command                                │
│     - Args: suite.env_cleanup.args                         │
│     - Envs: suite.env_cleanup.envs + context vars          │
│     - Timeout: suite.env_cleanup.timeout                   │
│                                                             │
│  4. Report suite completion to coordinator                 │
│     - WS: SuiteCompleted {suite_uuid, stats}               │
│                                                             │
│  5. Transition: CLEANUP → IDLE                             │
│                                                             │
└─────────────────────────────────────────────────────────────┘
```

**State Transition:**

| State | Description | Activities |
|-------|-------------|------------|
| **Cleanup** | Running env_cleanup | - Shutdown all workers gracefully<br>- Execute cleanup hook<br>- Upload final artifacts<br>- Release resources |

### Section 10.1: Error Handling

From RFC Section 10.1:

```rust
#[derive(Debug, thiserror::Error)]
pub enum ManagerError {
    // ... other errors ...

    // Environment errors (non-retriable, abort suite)
    #[error("Environment preparation failed: {0}")]
    EnvPreparation(String),

    #[error("Environment cleanup failed: {0}")]
    EnvCleanup(String),
}

impl ManagerError {
    pub fn should_abort_suite(&self) -> bool {
        matches!(
            self,
            ManagerError::EnvPreparation(_)
                | ManagerError::EnvCleanup(_)
                | ManagerError::PermissionDenied(_)
        )
    }
}
```

**Failure Behavior:**
- **env_preparation failure:** Abort suite, return to IDLE state, suite remains in queue for other managers
- **env_cleanup failure:** Log error but continue to report suite completion (don't fail the entire suite)

From RFC Section 15.3 FAQ:

> **Q: What if env_preparation times out?**
> A: Manager aborts the suite, returns to idle, and tries next eligible suite. Suite stays in queue for other managers.

## Implementation Tasks

### Task 5.1: Implement EnvHookSpec Parsing

**Objective:** Parse `env_preparation` and `env_cleanup` JSONB fields from database into Rust structs.

**Files to modify:**
- `netmito/src/schema.rs` - Add `EnvHookSpec` struct (may already exist from Phase 2)
- Database query code where `task_suites` are fetched

**Implementation Steps:**

1. **Define `EnvHookSpec` struct** (if not already present):

```rust
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct EnvHookSpec {
    pub args: Vec<String>,
    #[serde(default)]
    pub envs: HashMap<String, String>,
    #[serde(default)]
    pub resources: Vec<RemoteResourceDownload>,
    #[serde(with = "humantime_serde")]
    pub timeout: Duration,
}
```

2. **Parse from database JSONB:**

```rust
// When fetching task suite from database
let env_preparation: Option<EnvHookSpec> = row
    .try_get::<Option<sqlx::types::Json<EnvHookSpec>>, _>("env_preparation")
    .ok()
    .flatten()
    .map(|json| json.0);

let env_cleanup: Option<EnvHookSpec> = row
    .try_get::<Option<sqlx::types::Json<EnvHookSpec>>, _>("env_cleanup")
    .ok()
    .flatten()
    .map(|json| json.0);
```

3. **Validation:**
   - Ensure `args` is not empty
   - Validate timeout is reasonable (> 0, < 1 hour recommended)
   - Validate resource paths don't contain dangerous characters (e.g., `..`)

**Testing:**
- Parse valid EnvHookSpec JSON
- Handle missing optional fields (empty envs, resources)
- Reject invalid JSON gracefully
- Handle null/missing env_preparation and env_cleanup

---

### Task 5.2: Implement S3 Resource Download

**Objective:** Download remote resources (attachments/artifacts) from S3 before hook execution.

**Files to modify:**
- New file: `netmito/src/manager/resources.rs`
- `netmito/src/manager/mod.rs` - Add module declaration

**Implementation Steps:**

1. **Create resource download function:**

```rust
use crate::schema::{RemoteResource, RemoteResourceDownload, RemoteResourceDownloadResp};
use std::path::{Path, PathBuf};

/// Download all resources for a hook to a temporary directory
pub async fn download_hook_resources(
    client: &reqwest::Client,
    resources: &[RemoteResourceDownload],
    base_dir: &Path,
    suite_uuid: Uuid,
) -> Result<(), ManagerError> {
    // Create resource download directory: {base_dir}/suites/{suite_uuid}/resources/
    let resource_dir = base_dir.join("suites").join(suite_uuid.to_string()).join("resources");
    tokio::fs::create_dir_all(&resource_dir).await?;

    for resource in resources {
        download_single_resource(client, resource, &resource_dir).await?;
    }

    Ok(())
}

async fn download_single_resource(
    client: &reqwest::Client,
    resource: &RemoteResourceDownload,
    resource_dir: &Path,
) -> Result<(), ManagerError> {
    // 1. Get presigned URL from coordinator
    let download_resp = get_presigned_url(client, &resource.remote_file).await?;

    // 2. Download file to local_path
    let local_path = resource_dir.join(&resource.local_path);

    // Create parent directories if needed
    if let Some(parent) = local_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    // 3. Download with progress tracking
    download_file_from_s3(client, &download_resp.url, &local_path, download_resp.size).await?;

    tracing::info!(
        remote_file = ?resource.remote_file,
        local_path = ?local_path,
        size = download_resp.size,
        "Downloaded resource successfully"
    );

    Ok(())
}

async fn get_presigned_url(
    client: &reqwest::Client,
    remote_file: &RemoteResource,
) -> Result<RemoteResourceDownloadResp, ManagerError> {
    // Use existing worker download endpoints:
    // - GET /workers/tasks/{uuid}/artifacts/{content_type}
    // - GET /workers/tasks/{uuid}/attachments/{key}

    // Note: For manager hooks, we need suite-level attachment access
    // This may require new coordinator endpoints:
    // - GET /managers/suites/{suite_uuid}/attachments/{key}

    match remote_file {
        RemoteResource::Attachment { key } => {
            // TODO: Call manager-specific attachment download endpoint
            // For now, use task-based endpoint (requires task UUID)
            todo!("Implement manager attachment download")
        }
        RemoteResource::Artifact { uuid, content_type } => {
            let url = format!("/workers/tasks/{}/artifacts/{}", uuid, content_type);
            let resp = client.get(&url).send().await?;
            Ok(resp.json::<RemoteResourceDownloadResp>().await?)
        }
    }
}

async fn download_file_from_s3(
    client: &reqwest::Client,
    presigned_url: &str,
    local_path: &Path,
    expected_size: i64,
) -> Result<(), ManagerError> {
    use tokio::io::AsyncWriteExt;

    let mut response = client.get(presigned_url).send().await?;

    if !response.status().is_success() {
        return Err(ManagerError::S3Download(format!(
            "Failed to download from S3: {}",
            response.status()
        )));
    }

    let mut file = tokio::fs::File::create(local_path).await?;
    let mut downloaded: i64 = 0;

    while let Some(chunk) = response.chunk().await? {
        file.write_all(&chunk).await?;
        downloaded += chunk.len() as i64;
    }

    file.flush().await?;

    if downloaded != expected_size {
        return Err(ManagerError::S3Download(format!(
            "Size mismatch: expected {} bytes, got {} bytes",
            expected_size, downloaded
        )));
    }

    Ok(())
}
```

2. **Integration notes:**
   - Reuse existing S3 download infrastructure from worker implementation (see `netmito/src/worker.rs` lines 586-674)
   - May need new coordinator API endpoints for manager-level attachment downloads (attachments are group-scoped, not task-scoped)
   - Use timeout wrapper: resources should download within hook timeout period

**Testing:**
- Download Attachment resource
- Download Artifact resource
- Handle network failures
- Verify file size matches expected size
- Handle missing files (404)
- Timeout on slow downloads

---

### Task 5.3: Implement Hook Execution

**Objective:** Execute hook command with proper environment variables and timeout handling.

**Files to modify:**
- New file: `netmito/src/manager/hooks.rs`
- `netmito/src/manager/mod.rs` - Add module declaration

**Implementation Steps:**

1. **Create hook execution function:**

```rust
use tokio::process::Command;
use std::collections::HashMap;
use std::time::Duration;

pub struct HookContext {
    pub suite_uuid: Uuid,
    pub suite_name: Option<String>,
    pub group_name: String,
    pub worker_count: u32,
    pub manager_id: Uuid,
}

impl HookContext {
    pub fn to_env_vars(&self) -> HashMap<String, String> {
        let mut env_vars = HashMap::new();

        env_vars.insert(
            "MITOSIS_TASK_SUITE_UUID".to_string(),
            self.suite_uuid.to_string(),
        );

        env_vars.insert(
            "MITOSIS_TASK_SUITE_NAME".to_string(),
            self.suite_name.clone().unwrap_or_default(),
        );

        env_vars.insert(
            "MITOSIS_GROUP_NAME".to_string(),
            self.group_name.clone(),
        );

        env_vars.insert(
            "MITOSIS_WORKER_COUNT".to_string(),
            self.worker_count.to_string(),
        );

        env_vars.insert(
            "MITOSIS_NODE_MANAGER_ID".to_string(),
            self.manager_id.to_string(),
        );

        env_vars
    }
}

pub async fn execute_hook(
    hook_spec: &EnvHookSpec,
    context: &HookContext,
    working_dir: &Path,
) -> Result<(), ManagerError> {
    if hook_spec.args.is_empty() {
        return Err(ManagerError::EnvPreparation(
            "Hook args cannot be empty".to_string()
        ));
    }

    // Merge hook envs with context vars
    let mut env_vars = context.to_env_vars();
    env_vars.extend(hook_spec.envs.clone());

    // Build command
    let mut cmd = Command::new(&hook_spec.args[0]);
    cmd.args(&hook_spec.args[1..])
        .current_dir(working_dir)
        .envs(&env_vars)
        .kill_on_drop(true);

    tracing::info!(
        args = ?hook_spec.args,
        timeout = ?hook_spec.timeout,
        "Executing hook"
    );

    // Execute with timeout
    let output = tokio::time::timeout(
        hook_spec.timeout,
        cmd.output()
    ).await;

    match output {
        Ok(Ok(output)) => {
            if output.status.success() {
                tracing::info!(
                    exit_code = output.status.code(),
                    "Hook executed successfully"
                );

                // Log stdout/stderr for debugging
                if !output.stdout.is_empty() {
                    tracing::debug!(
                        stdout = %String::from_utf8_lossy(&output.stdout),
                        "Hook stdout"
                    );
                }
                if !output.stderr.is_empty() {
                    tracing::debug!(
                        stderr = %String::from_utf8_lossy(&output.stderr),
                        "Hook stderr"
                    );
                }

                Ok(())
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                Err(ManagerError::EnvPreparation(format!(
                    "Hook failed with exit code {}: {}",
                    output.status.code().unwrap_or(-1),
                    stderr
                )))
            }
        }
        Ok(Err(e)) => {
            Err(ManagerError::EnvPreparation(format!(
                "Failed to spawn hook process: {}",
                e
            )))
        }
        Err(_) => {
            Err(ManagerError::EnvPreparation(format!(
                "Hook timed out after {:?}",
                hook_spec.timeout
            )))
        }
    }
}
```

2. **Working directory setup:**
   - Use suite-specific directory: `{base_dir}/suites/{suite_uuid}/`
   - Downloaded resources are in: `{base_dir}/suites/{suite_uuid}/resources/`
   - Set current working directory to suite directory for hook execution

**Testing:**
- Execute successful hook (exit 0)
- Handle hook failure (non-zero exit)
- Timeout enforcement
- Environment variable injection (verify context vars are set)
- stdout/stderr capture and logging
- Process cleanup on timeout (kill_on_drop)

---

### Task 5.4: Implement Environment Preparation

**Objective:** Integrate env_preparation workflow into Node Manager state machine (IDLE → PREPARING → EXECUTING).

**Files to modify:**
- `netmito/src/manager.rs` (or wherever NodeManager is implemented)

**Implementation Steps:**

1. **Add method to Node Manager:**

```rust
impl NodeManager {
    /// Execute environment preparation hook
    /// Transition: PREPARING state
    async fn run_env_preparation(&mut self, suite: &TaskSuite) -> Result<(), ManagerError> {
        self.state = NodeManagerState::Preparing;

        // Check if env_preparation hook is specified
        let Some(env_prep) = &suite.env_preparation else {
            tracing::info!("No env_preparation hook specified, skipping");
            return Ok(());
        };

        tracing::info!(
            suite_uuid = %suite.uuid,
            "Running environment preparation"
        );

        // 1. Download resources from S3
        if !env_prep.resources.is_empty() {
            tracing::info!(
                resource_count = env_prep.resources.len(),
                "Downloading hook resources"
            );

            download_hook_resources(
                &self.http_client,
                &env_prep.resources,
                &self.config.cache_dir,
                suite.uuid,
            ).await.map_err(|e| {
                ManagerError::EnvPreparation(format!("Resource download failed: {}", e))
            })?;
        }

        // 2. Execute preparation command
        let context = HookContext {
            suite_uuid: suite.uuid,
            suite_name: suite.name.clone(),
            group_name: suite.group_name.clone(),
            worker_count: suite.worker_schedule.worker_count,
            manager_id: self.uuid,
        };

        let working_dir = self.config.cache_dir
            .join("suites")
            .join(suite.uuid.to_string());

        tokio::fs::create_dir_all(&working_dir).await?;

        execute_hook(env_prep, &context, &working_dir).await?;

        tracing::info!("Environment preparation completed successfully");
        Ok(())
    }

    /// Handle suite assignment from coordinator
    async fn start_suite(&mut self, suite: TaskSuite) -> Result<(), ManagerError> {
        tracing::info!(
            suite_uuid = %suite.uuid,
            suite_name = ?suite.name,
            "Starting suite execution"
        );

        self.assigned_suite_uuid = Some(suite.uuid);

        // Run environment preparation
        if let Err(e) = self.run_env_preparation(&suite).await {
            tracing::error!(
                suite_uuid = %suite.uuid,
                error = %e,
                "Environment preparation failed, aborting suite"
            );

            // Abort suite and return to idle
            self.abort_current_suite().await?;
            return Err(e);
        }

        // Transition to EXECUTING state
        self.state = NodeManagerState::Executing;

        // Spawn workers (implemented in Phase 6)
        self.spawn_workers(&suite).await?;

        Ok(())
    }
}
```

2. **Handle preparation failures:**

```rust
async fn abort_current_suite(&mut self) -> Result<(), ManagerError> {
    tracing::warn!(
        suite_uuid = ?self.assigned_suite_uuid,
        "Aborting current suite"
    );

    // Kill any spawned workers (if any)
    self.kill_all_workers().await;

    // Clean up suite resources
    if let Some(suite_uuid) = self.assigned_suite_uuid {
        let suite_dir = self.config.cache_dir.join("suites").join(suite_uuid.to_string());
        if suite_dir.exists() {
            tokio::fs::remove_dir_all(&suite_dir).await?;
        }
    }

    // Transition back to IDLE
    self.assigned_suite_uuid = None;
    self.state = NodeManagerState::Idle;

    Ok(())
}
```

**Testing:**
- Successful preparation → EXECUTING state
- Preparation failure → IDLE state (suite aborted)
- Preparation timeout → IDLE state (suite aborted)
- Skip preparation if env_preparation is None
- Resource download before command execution

---

### Task 5.5: Implement Environment Cleanup

**Objective:** Integrate env_cleanup workflow into Node Manager state machine (EXECUTING → CLEANUP → IDLE).

**Files to modify:**
- `netmito/src/manager.rs`

**Implementation Steps:**

1. **Add method to Node Manager:**

```rust
impl NodeManager {
    /// Execute environment cleanup hook
    /// Transition: CLEANUP state
    async fn run_env_cleanup(&mut self, suite: &TaskSuite) -> Result<(), ManagerError> {
        self.state = NodeManagerState::Cleanup;

        tracing::info!(
            suite_uuid = %suite.uuid,
            "Running environment cleanup"
        );

        // 1. Gracefully shutdown all workers (30 second timeout)
        self.shutdown_workers_gracefully(Duration::from_secs(30)).await;

        // 2. Force kill any remaining workers
        self.kill_all_workers().await;

        // 3. Execute cleanup command (if specified)
        if let Some(env_cleanup) = &suite.env_cleanup {
            tracing::info!("Executing cleanup hook");

            let context = HookContext {
                suite_uuid: suite.uuid,
                suite_name: suite.name.clone(),
                group_name: suite.group_name.clone(),
                worker_count: suite.worker_schedule.worker_count,
                manager_id: self.uuid,
            };

            let working_dir = self.config.cache_dir
                .join("suites")
                .join(suite.uuid.to_string());

            // Note: Cleanup failures are logged but don't fail the suite
            if let Err(e) = execute_hook(env_cleanup, &context, &working_dir).await {
                tracing::error!(
                    error = %e,
                    "Cleanup hook failed (continuing anyway)"
                );
            }
        }

        // 4. Clean up suite resources
        let suite_dir = self.config.cache_dir
            .join("suites")
            .join(suite.uuid.to_string());
        if suite_dir.exists() {
            if let Err(e) = tokio::fs::remove_dir_all(&suite_dir).await {
                tracing::warn!(
                    error = %e,
                    suite_dir = ?suite_dir,
                    "Failed to remove suite directory"
                );
            }
        }

        Ok(())
    }

    /// Handle suite completion
    async fn complete_suite(&mut self) -> Result<(), ManagerError> {
        let Some(suite_uuid) = self.assigned_suite_uuid else {
            return Ok(());
        };

        tracing::info!(
            suite_uuid = %suite_uuid,
            "Suite completed, running cleanup"
        );

        // Get suite data (needed for cleanup hook)
        let suite = self.get_assigned_suite().await?;

        // Run cleanup
        self.run_env_cleanup(&suite).await?;

        // Report completion to coordinator
        self.send_ws_message(ManagerMessage::SuiteCompleted {
            suite_uuid,
            stats: self.collect_suite_stats(),
        }).await?;

        // Transition back to IDLE
        self.assigned_suite_uuid = None;
        self.state = NodeManagerState::Idle;

        tracing::info!("Suite cleanup completed, returning to idle");

        Ok(())
    }

    async fn shutdown_workers_gracefully(&mut self, timeout: Duration) {
        tracing::info!(
            worker_count = self.workers.len(),
            timeout = ?timeout,
            "Shutting down workers gracefully"
        );

        // Send SIGTERM to all workers
        for worker in &mut self.workers {
            if let Err(e) = worker.process.start_kill() {
                tracing::warn!(
                    worker_id = worker.local_id,
                    error = %e,
                    "Failed to send SIGTERM to worker"
                );
            }
        }

        // Wait for workers to exit (up to timeout)
        let deadline = tokio::time::Instant::now() + timeout;

        for worker in &mut self.workers {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());

            if remaining.is_zero() {
                break;
            }

            match tokio::time::timeout(remaining, worker.process.wait()).await {
                Ok(Ok(status)) => {
                    tracing::info!(
                        worker_id = worker.local_id,
                        exit_status = ?status,
                        "Worker exited gracefully"
                    );
                }
                Ok(Err(e)) => {
                    tracing::warn!(
                        worker_id = worker.local_id,
                        error = %e,
                        "Error waiting for worker"
                    );
                }
                Err(_) => {
                    tracing::warn!(
                        worker_id = worker.local_id,
                        "Worker did not exit within timeout"
                    );
                }
            }
        }
    }

    async fn kill_all_workers(&mut self) {
        tracing::info!(
            worker_count = self.workers.len(),
            "Force killing all workers"
        );

        for worker in &mut self.workers {
            if let Err(e) = worker.process.kill().await {
                tracing::warn!(
                    worker_id = worker.local_id,
                    error = %e,
                    "Failed to kill worker"
                );
            }
        }

        self.workers.clear();
    }
}
```

2. **Cleanup failure handling:**
   - Unlike preparation, cleanup failures should **not** prevent suite completion
   - Log errors but continue with reporting completion to coordinator
   - Always clean up local resources (suite directory)

**Testing:**
- Successful cleanup → IDLE state
- Cleanup hook failure → log error, continue to IDLE
- Cleanup timeout → log error, continue to IDLE
- Worker shutdown (graceful then force kill)
- Resource cleanup (suite directory removal)
- Skip cleanup if env_cleanup is None

---

### Task 5.6: Implement Context Variable Injection

**Objective:** Ensure all `MITOSIS_*` environment variables are correctly injected with proper values.

**Files to modify:**
- `netmito/src/manager/hooks.rs` (HookContext implementation)

**Implementation Steps:**

1. **Verify HookContext completeness:**

Ensure all context variables are implemented in `HookContext::to_env_vars()`:

```rust
impl HookContext {
    pub fn to_env_vars(&self) -> HashMap<String, String> {
        let mut env_vars = HashMap::new();

        // Required variables
        env_vars.insert(
            "MITOSIS_TASK_SUITE_UUID".to_string(),
            self.suite_uuid.to_string(),
        );

        env_vars.insert(
            "MITOSIS_TASK_SUITE_NAME".to_string(),
            self.suite_name.clone().unwrap_or_default(),
        );

        env_vars.insert(
            "MITOSIS_GROUP_NAME".to_string(),
            self.group_name.clone(),
        );

        env_vars.insert(
            "MITOSIS_WORKER_COUNT".to_string(),
            self.worker_count.to_string(),
        );

        env_vars.insert(
            "MITOSIS_NODE_MANAGER_ID".to_string(),
            self.manager_id.to_string(),
        );

        env_vars
    }
}
```

2. **Variable source mapping:**

| Variable | Source |
|----------|--------|
| `MITOSIS_TASK_SUITE_UUID` | `suite.uuid` |
| `MITOSIS_TASK_SUITE_NAME` | `suite.name` (empty string if None) |
| `MITOSIS_GROUP_NAME` | `suite.group_name` |
| `MITOSIS_WORKER_COUNT` | `suite.worker_schedule.worker_count` |
| `MITOSIS_NODE_MANAGER_ID` | `manager.uuid` |

3. **Environment variable precedence:**
   - Context variables (`MITOSIS_*`) are set first
   - User-provided `hook_spec.envs` can override context variables
   - Merge: `context_vars.extend(hook_spec.envs)`

**Testing:**
- Verify all 5 context variables are present
- Verify correct values for each variable
- Test with suite_name = None (should be empty string)
- Test env override precedence
- Write integration test that inspects environment from within hook script

---

### Task 5.7: Error Handling and Logging

**Objective:** Implement comprehensive error handling and logging for hook execution.

**Files to modify:**
- `netmito/src/error.rs` - Add ManagerError variants (may already exist from Phase 4)
- All hook-related files

**Implementation Steps:**

1. **Define error types** (add to existing ManagerError):

```rust
#[derive(Debug, thiserror::Error)]
pub enum ManagerError {
    // ... existing errors ...

    #[error("Environment preparation failed: {0}")]
    EnvPreparation(String),

    #[error("Environment cleanup failed: {0}")]
    EnvCleanup(String),

    #[error("S3 resource download failed: {0}")]
    S3Download(String),

    #[error("Hook timeout after {0:?}")]
    HookTimeout(Duration),
}

impl ManagerError {
    pub fn should_abort_suite(&self) -> bool {
        matches!(
            self,
            ManagerError::EnvPreparation(_)
                | ManagerError::PermissionDenied(_)
        )
    }
}
```

2. **Logging strategy:**

| Event | Log Level | Details |
|-------|-----------|---------|
| Hook start | INFO | args, timeout, suite_uuid |
| Hook success | INFO | exit_code, duration |
| Hook failure | ERROR | exit_code, stderr, suite_uuid |
| Hook timeout | ERROR | timeout duration, suite_uuid |
| Resource download start | INFO | resource count, suite_uuid |
| Resource download success | INFO | file size, local_path |
| Resource download failure | ERROR | error message, remote_file |
| Preparation abort | WARN | reason, suite_uuid |
| Cleanup failure | ERROR | error message (but continue) |

3. **Example logging:**

```rust
tracing::info!(
    suite_uuid = %suite.uuid,
    hook_type = "env_preparation",
    args = ?hook_spec.args,
    timeout = ?hook_spec.timeout,
    resource_count = hook_spec.resources.len(),
    "Starting environment preparation"
);

tracing::error!(
    suite_uuid = %suite.uuid,
    hook_type = "env_preparation",
    exit_code = ?output.status.code(),
    stderr = %String::from_utf8_lossy(&output.stderr),
    "Environment preparation failed"
);
```

4. **Error recovery:**

| Error Type | Recovery Action |
|------------|-----------------|
| Preparation resource download failure | Abort suite, return to IDLE |
| Preparation command failure | Abort suite, return to IDLE |
| Preparation timeout | Abort suite, return to IDLE |
| Cleanup resource download failure | Log error, continue |
| Cleanup command failure | Log error, continue |
| Cleanup timeout | Log error, continue |

**Testing:**
- Verify all error types are handled
- Verify logging output for each scenario
- Test error recovery (abort vs continue)
- Test that cleanup errors don't prevent suite completion

---

## Testing Checklist

### Unit Tests

- [ ] `EnvHookSpec` parsing from JSON
  - [ ] Valid JSON with all fields
  - [ ] Missing optional fields (envs, resources)
  - [ ] Invalid timeout format
  - [ ] Empty args array (should fail)

- [ ] Context variable generation
  - [ ] All 5 variables present
  - [ ] Correct UUID formatting
  - [ ] Handle None suite_name (empty string)
  - [ ] Correct worker_count value

- [ ] Hook execution
  - [ ] Successful execution (exit 0)
  - [ ] Failed execution (non-zero exit)
  - [ ] Timeout enforcement
  - [ ] stdout/stderr capture
  - [ ] Environment variable injection

### Integration Tests

- [ ] **End-to-end preparation flow:**
  1. Fetch suite with env_preparation
  2. Download resources from S3
  3. Execute preparation hook
  4. Verify PREPARING → EXECUTING transition
  5. Verify resources downloaded to correct paths

- [ ] **End-to-end cleanup flow:**
  1. Complete suite execution
  2. Graceful worker shutdown
  3. Execute cleanup hook
  4. Verify CLEANUP → IDLE transition
  5. Verify suite directory removed

- [ ] **Preparation failure scenarios:**
  - [ ] Resource download failure → abort suite
  - [ ] Hook command not found → abort suite
  - [ ] Hook timeout → abort suite
  - [ ] Hook non-zero exit → abort suite

- [ ] **Cleanup failure scenarios:**
  - [ ] Resource download failure → log, continue
  - [ ] Hook command not found → log, continue
  - [ ] Hook timeout → log, continue
  - [ ] Hook non-zero exit → log, continue

- [ ] **Resource download:**
  - [ ] Download Attachment from S3
  - [ ] Download Artifact from S3
  - [ ] Multiple resources in single hook
  - [ ] Nested directory paths (local_path with subdirs)

- [ ] **State machine transitions:**
  - [ ] IDLE → PREPARING → EXECUTING (success)
  - [ ] IDLE → PREPARING → IDLE (preparation failure)
  - [ ] EXECUTING → CLEANUP → IDLE (success)
  - [ ] EXECUTING → CLEANUP → IDLE (cleanup failure)

### Manual Testing

- [ ] Create suite with env_preparation hook
- [ ] Submit tasks to suite
- [ ] Observe manager executing preparation
- [ ] Verify workers spawn after preparation
- [ ] Verify cleanup executes after last task
- [ ] Test with missing env_preparation (should skip)
- [ ] Test with missing env_cleanup (should skip)

### Performance Testing

- [ ] Large resource downloads (>1GB)
- [ ] Many resources (100+ files)
- [ ] Long-running hooks (near timeout)
- [ ] Concurrent suite executions on different managers

---

## Success Criteria

1. **Functional Requirements:**
   - [ ] Node Manager successfully parses `env_preparation` and `env_cleanup` from database
   - [ ] Resources download correctly from S3 before hook execution
   - [ ] Hooks execute with proper timeout enforcement
   - [ ] All 5 context variables (`MITOSIS_*`) are injected correctly
   - [ ] Preparation failures abort suite and return to IDLE
   - [ ] Cleanup failures log errors but continue to completion
   - [ ] State machine transitions correctly (PREPARING, CLEANUP states)

2. **Error Handling:**
   - [ ] Preparation timeout aborts suite
   - [ ] Resource download failures abort preparation
   - [ ] Hook command failures are logged with exit code and stderr
   - [ ] Cleanup errors don't prevent suite completion reporting

3. **Resource Management:**
   - [ ] Suite directories created/removed correctly
   - [ ] Downloaded resources cleaned up after suite completion
   - [ ] No resource leaks (processes, file handles)

4. **Logging and Observability:**
   - [ ] All hook executions logged with relevant context
   - [ ] Failures include actionable error messages
   - [ ] Resource downloads tracked with progress indicators

5. **Integration:**
   - [ ] Hooks integrate seamlessly into manager lifecycle
   - [ ] No disruption to workers during preparation/cleanup
   - [ ] Coordinator receives suite completion after cleanup

---

## Dependencies

**Prerequisites:**
- Phase 1: Database schema complete
- Phase 2: API schema with EnvHookSpec types
- Phase 3: WebSocket manager communication
- Phase 4: Node Manager core state machine

**Blocks:**
- Phase 6: Worker Spawning and IPC (depends on preparation completing successfully)
- Phase 7: Integration testing (depends on full hook lifecycle)

---

## Next Phase

**Phase 6: Worker Spawning and IPC (2 weeks)**

After completing Phase 5, the Node Manager can:
- Download resources for suite setup
- Execute preparation hooks before workers start
- Execute cleanup hooks after workers finish

Phase 6 will implement:
- Worker process spawning based on `WorkerSchedulePlan`
- IPC communication between manager and workers (iceoryx2 or Unix sockets)
- Worker health monitoring and auto-respawn
- Task prefetching and distribution to workers

---

## Use Case Examples from RFC

### Use Case 1: ML Training Campaign (Section 3.1)

**Scenario:** User wants to run distributed ML training with shared dataset setup.

**Suite Configuration:**

```json
{
  "name": "ML Training Campaign",
  "group_name": "ml-team",
  "tags": ["gpu"],
  "worker_schedule": {
    "worker_count": 16,
    "cpu_binding": {"cores": [0,1,2,3], "strategy": "RoundRobin"},
    "task_prefetch_count": 32
  },
  "env_preparation": {
    "args": ["./setup.sh", "--download-dataset"],
    "envs": {"DATA_DIR": "/mnt/data"},
    "resources": [
      {"remote_file": {"Attachment": {"key": "setup.sh"}}, "local_path": "setup.sh"}
    ],
    "timeout": "5m"
  },
  "env_cleanup": {
    "args": ["./cleanup.sh"],
    "envs": {},
    "resources": [],
    "timeout": "2m"
  }
}
```

**Workflow:**
1. Manager receives suite assignment
2. **PREPARING state:** Download `setup.sh`, execute with `DATA_DIR=/mnt/data`
3. Setup script downloads shared dataset to `/mnt/data`
4. **EXECUTING state:** Spawn 16 workers, execute training tasks
5. All tasks complete
6. **CLEANUP state:** Execute `cleanup.sh` to remove dataset
7. **IDLE state:** Ready for next suite

**Benefits:**
- Dataset downloaded once (not per task)
- All 16 workers share dataset
- Automatic cleanup after campaign

### Use Case 2: CI/CD Pipeline (Section 3.1)

**Scenario:** Run integration tests with database setup/teardown.

**Suite Configuration:**

```json
{
  "name": "Integration Tests",
  "group_name": "ci-cd",
  "tags": ["linux"],
  "worker_schedule": {
    "worker_count": 4,
    "cpu_binding": {"cores": [0,1,2,3], "strategy": "Exclusive"},
    "task_prefetch_count": 8
  },
  "env_preparation": {
    "args": ["docker", "compose", "up", "-d"],
    "envs": {"COMPOSE_PROJECT_NAME": "integration-tests"},
    "resources": [
      {"remote_file": {"Attachment": {"key": "docker-compose.yml"}}, "local_path": "docker-compose.yml"}
    ],
    "timeout": "2m"
  },
  "env_cleanup": {
    "args": ["docker", "compose", "down", "-v"],
    "envs": {"COMPOSE_PROJECT_NAME": "integration-tests"},
    "resources": [],
    "timeout": "1m"
  }
}
```

**Workflow:**
1. **PREPARING:** Download `docker-compose.yml`, start database containers
2. **EXECUTING:** Run 100 integration test tasks across 4 workers
3. **CLEANUP:** Stop and remove containers
4. Clean suite directory

**Benefits:**
- Database setup once (not per test)
- Guaranteed cleanup even if tests fail
- Isolated test environment per suite

---

## Implementation Notes

### Resource Download Endpoint Considerations

The current worker API has task-scoped attachment downloads:
- `GET /workers/tasks/{uuid}/attachments/{key}`

For suite-level attachments in hooks, we may need:
- `GET /managers/suites/{suite_uuid}/attachments/{key}`
  - Or: `GET /managers/groups/{group_name}/attachments/{key}`

**Recommendation:** Reuse group-level attachment download (attachments are group-scoped). Pass group name from suite to resource download function.

### Timeout Recommendations

From RFC and best practices:

| Hook Type | Recommended Timeout | Max Timeout |
|-----------|---------------------|-------------|
| env_preparation | 5-10 minutes | 30 minutes |
| env_cleanup | 1-2 minutes | 10 minutes |

Preparation can be longer (dataset downloads, container builds), but cleanup should be quick.

### Working Directory Structure

```
{cache_dir}/
└── suites/
    └── {suite_uuid}/
        ├── resources/          # Downloaded S3 resources
        │   ├── setup.sh
        │   └── data/
        │       └── dataset.tar.gz
        └── (hook working directory)
```

Hooks execute with CWD = `{cache_dir}/suites/{suite_uuid}/`

Resources available at: `./resources/setup.sh`, `./resources/data/dataset.tar.gz`

### Logging Best Practices

Use structured logging with `tracing`:

```rust
tracing::info!(
    suite_uuid = %suite.uuid,
    suite_name = ?suite.name,
    manager_id = %self.uuid,
    state = ?self.state,
    "State transition"
);
```

**Key fields to include:**
- `suite_uuid` - Always include in suite-related logs
- `manager_id` - Always include in manager logs
- `worker_id` - Include in worker-related logs
- `state` - Include in state transitions
- `error` - Include in error logs

### Security Considerations

1. **Command injection prevention:**
   - Do not shell-expand `args` (use `Command::new` + `args`)
   - Validate resource paths (no `..`, absolute paths)

2. **Resource limits:**
   - Enforce timeout on all hook executions
   - Limit resource download size (configurable max)
   - Clean up resources after suite completion

3. **Permissions:**
   - Hooks run with manager's user permissions
   - Ensure manager has appropriate file system access
   - Consider containerized execution for isolation (future enhancement)
