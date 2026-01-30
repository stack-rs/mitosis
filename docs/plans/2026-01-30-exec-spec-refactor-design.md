# ExecSpec Refactor and Suite Execution Failures Design

**Date:** 2026-01-30
**Status:** Approved
**Scope:** Unify execution specification schema, refactor suite hooks, repurpose execution failures table

## Overview

This design refactors the execution specification schema to provide a unified, extensible structure for both task execution and suite hooks. It also repurposes the `task_execution_failures` table to track agent-level suite execution failures rather than individual task failures.

## Goals

1. **Unified execution spec**: Create a common `ExecSpec` schema for tasks and hooks
2. **Flexible suite hooks**: Merge `env_provision`, `env_cleanup`, `env_background`, `env_timeout` into a single `exec_hooks` JSON field
3. **Agent-level failure tracking**: Repurpose failures table to track when agents fail to execute suite hooks (provision, cleanup, etc.)
4. **Future extensibility**: JSON fields allow adding new options without schema migrations

## Schema Types

### ExecSpec

Core specification for executing a command, shared by tasks and suite hooks:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecSpec {
    /// Command and arguments to execute
    pub args: Vec<String>,
    /// Environment variables to set
    #[serde(default)]
    pub envs: HashMap<String, String>,
    /// Remote resources to download before execution
    #[serde(default)]
    pub resources: Vec<RemoteResourceDownload>,
    /// Execution timeout (stored as seconds in DB, uses humantime in JSON API)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(with = "humantime_serde")]
    pub timeout: Option<std::time::Duration>,
    /// Whether to capture terminal output
    #[serde(default)]
    pub terminal_output: bool,
}
```

### TaskExecOptions

Task-specific execution options (not part of core ExecSpec):

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TaskExecOptions {
    /// Watch another task's state before executing
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watch: Option<(Uuid, TaskExecState)>,
}
```

### ExecHooks

Execution hooks for a task suite:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ExecHooks {
    /// Environment preparation hook (runs before workers start)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provision: Option<ExecSpec>,
    /// Environment cleanup hook (runs after suite completes)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleanup: Option<ExecSpec>,
    /// Background process hook (runs alongside workers)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<ExecSpec>,
}
```

### SuiteFailureType

Types of agent-level suite execution failures:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, EnumIter, DeriveActiveEnum)]
#[sea_orm(rs_type = "i32", db_type = "Integer")]
pub enum SuiteFailureType {
    #[sea_orm(num_value = 0)]
    Provision,   // env provision hook failed
    #[sea_orm(num_value = 1)]
    Cleanup,     // env cleanup hook failed
    #[sea_orm(num_value = 2)]
    Background,  // background process failed
    #[sea_orm(num_value = 3)]
    Timeout,     // hook execution timed out
    #[sea_orm(num_value = 99)]
    Other,
}
```

## Database Changes

### task_suites Table

| Action | Column | Type | Notes |
|--------|--------|------|-------|
| Remove | `env_provision` | JSON | Merged into exec_hooks |
| Remove | `env_cleanup` | JSON | Merged into exec_hooks |
| Remove | `env_background` | JSON | Merged into exec_hooks |
| Remove | `env_timeout` | i64 | Merged into exec_hooks (each hook has its own timeout) |
| Add | `exec_hooks` | JSON (nullable) | Contains ExecHooks struct |

**Entity (after):**
```rust
pub struct Model {
    pub id: i64,
    pub uuid: Uuid,
    pub name: Option<String>,
    pub description: Option<String>,
    pub group_id: i64,
    pub creator_id: i64,
    pub tags: Vec<String>,
    pub labels: Vec<String>,
    pub priority: i32,
    pub worker_schedule: Json,
    pub exec_hooks: Option<Json>,  // NEW: replaces env_* fields
    pub state: TaskSuiteState,
    pub last_task_submitted_at: Option<TimeDateTimeWithTimeZone>,
    pub total_tasks: i32,
    pub pending_tasks: i32,
    pub created_at: TimeDateTimeWithTimeZone,
    pub updated_at: TimeDateTimeWithTimeZone,
    pub completed_at: Option<TimeDateTimeWithTimeZone>,
}
```

### active_tasks / archived_tasks Tables

| Action | Column | Type | Notes |
|--------|--------|------|-------|
| Remove | `timeout` | i64 | Moved into spec JSON |
| Modify | `spec` | JSON | Now stores ExecSpec (includes timeout) |
| Add | `exec_options` | JSON (nullable) | Contains TaskExecOptions (watch) |

**Entity (after):**
```rust
pub struct Model {
    pub id: i64,
    pub creator_id: i64,
    pub group_id: i64,
    pub task_id: i64,
    pub uuid: Uuid,
    pub tags: Vec<String>,
    pub labels: Vec<String>,
    pub created_at: TimeDateTimeWithTimeZone,
    pub updated_at: TimeDateTimeWithTimeZone,
    pub state: TaskState,
    pub assigned_worker: Option<i64>,
    pub priority: i32,
    pub spec: Json,                    // ExecSpec (now includes timeout)
    pub exec_options: Option<Json>,    // NEW: TaskExecOptions
    pub result: Option<Json>,
    pub upstream_task_uuid: Option<Uuid>,
    pub downstream_task_uuid: Option<Uuid>,
    pub task_suite_id: Option<i64>,
    // Note: timeout column removed, now in spec
}
```

### task_execution_failures → suite_execution_failures Table

**Rename table** from `task_execution_failures` to `suite_execution_failures`.

| Action | Column | Type | Notes |
|--------|--------|------|-------|
| Remove | `task_id` | i64 | Not tracking task failures |
| Remove | `task_uuid` | Uuid | Not tracking task failures |
| Modify | `task_suite_id` | i64 | Make NOT NULL (was nullable) |
| Add | `failure_type` | i32 | SuiteFailureType enum |

**Purpose:** Track when an agent fails to execute suite-level hooks (provision, cleanup, background). This prevents reassigning a failed suite to the same agent.

**Entity (after):**
```rust
pub struct Model {
    pub id: i64,
    pub task_suite_id: i64,           // Required - which suite
    pub agent_id: i64,                // Required - which agent failed
    pub failure_type: SuiteFailureType, // NEW: what type of failure
    pub failure_count: i32,
    pub last_failure_at: TimeDateTimeWithTimeZone,
    pub error_messages: Option<Vec<String>>,
    pub created_at: TimeDateTimeWithTimeZone,
    pub updated_at: TimeDateTimeWithTimeZone,
}
```

**Unique constraint:** `(task_suite_id, agent_id, failure_type)`

**Usage:**
- When fetching suites for an agent, exclude suites where the agent has failure records
- Upon suite completion, failure records can be removed upon user request
- Users can manually clear failure records to allow retry

## Migration Files to Modify

Since migrations `m20251117_*` have not been executed in production, we modify them directly:

| File | Changes |
|------|---------|
| `m20251117_100000_create_task_suites.rs` | Create with `exec_hooks` instead of `env_*` fields |
| `m20251117_130000_create_task_execution_failures.rs` | Create as `suite_execution_failures` with new schema |
| `m20251117_140000_alter_active_tasks.rs` | Add `exec_options`, migrate `timeout` into `spec`, drop `timeout` |
| `m20251117_140001_alter_archived_tasks.rs` | Same as active_tasks |

## Entity Files to Update

| File | Changes |
|------|---------|
| `entity/task_suites.rs` | Replace `env_*` fields with `exec_hooks` |
| `entity/task_execution_failures.rs` | Rename to `suite_execution_failures.rs`, update schema |
| `entity/active_tasks.rs` | Remove `timeout`, add `exec_options` |
| `entity/archived_tasks.rs` | Same as active_tasks |
| `entity/mod.rs` | Update module references |
| `entity/state.rs` | Add `SuiteFailureType` enum |

## Schema File Changes

Update `schema.rs`:

| Remove | Add |
|--------|-----|
| `TaskSpec` | `ExecSpec` |
| `EnvHookSpec` | `ExecHooks` |
| - | `TaskExecOptions` |
| - | `SuiteFailureType` |

Update request/response types:
- `CreateTaskSuiteReq`: Replace `env_preparation`, `env_cleanup` with `exec_hooks`
- `SubmitTaskReq`: Replace `task_spec: TaskSpec` with `spec: ExecSpec`, add `exec_options`
- `WorkerTaskResp`: Update to use new types
- `TaskSuiteSpec`, `TaskSuiteInfo`, `ParsedTaskSuiteInfo`: Update fields

## API Changes (Breaking)

### Task Submission

**Before:**
```json
{
  "group_name": "my-group",
  "timeout": "5m",
  "task_spec": {
    "args": ["echo", "hello"],
    "envs": {},
    "resources": [],
    "terminal_output": false,
    "watch": null
  }
}
```

**After:**
```json
{
  "group_name": "my-group",
  "spec": {
    "args": ["echo", "hello"],
    "envs": {},
    "resources": [],
    "timeout": "5m",
    "terminal_output": false
  },
  "exec_options": {
    "watch": null
  }
}
```

### Suite Creation

**Before:**
```json
{
  "group_name": "my-group",
  "worker_schedule": { "type": "FixedWorkers", "worker_count": 4 },
  "env_preparation": { "args": ["./setup.sh"], "timeout": "5m" },
  "env_cleanup": { "args": ["./cleanup.sh"], "timeout": "2m" }
}
```

**After:**
```json
{
  "group_name": "my-group",
  "worker_schedule": { "type": "FixedWorkers", "worker_count": 4 },
  "exec_hooks": {
    "provision": { "args": ["./setup.sh"], "timeout": "5m" },
    "cleanup": { "args": ["./cleanup.sh"], "timeout": "2m" }
  }
}
```

## Implementation Checklist

1. [ ] Update `entity/state.rs` - Add `SuiteFailureType` enum
2. [ ] Update `entity/task_suites.rs` - Replace env fields with `exec_hooks`
3. [ ] Rename `entity/task_execution_failures.rs` → `entity/suite_execution_failures.rs`
4. [ ] Update `entity/active_tasks.rs` - Remove `timeout`, add `exec_options`
5. [ ] Update `entity/archived_tasks.rs` - Same changes
6. [ ] Update `entity/mod.rs` - Fix module references
7. [ ] Update `schema.rs` - New types and API schemas
8. [ ] Update `m20251117_100000_create_task_suites.rs` - New schema
9. [ ] Update `m20251117_130000_create_task_execution_failures.rs` - Rename and new schema
10. [ ] Update `m20251117_140000_alter_active_tasks.rs` - Add exec_options, migrate timeout
11. [ ] Update `m20251117_140001_alter_archived_tasks.rs` - Same changes
12. [ ] Update service layer (`service/suite.rs`, `service/task.rs`) - Use new types
13. [ ] Update API layer (`api/suites.rs`) - Use new request/response types
14. [ ] Update agent code - Use new types for suite execution
