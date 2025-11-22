# API and CLI Interface Review

**Date**: 2025-11-22
**Project**: Mitosis - Unified Distributed Transport Evaluation Framework
**Reviewer**: Claude Code

---

## Table of Contents

1. [Executive Summary](#executive-summary)
2. [HTTP API Endpoints Review](#http-api-endpoints-review)
3. [CLI Interface Review](#cli-interface-review)
4. [Recommendations by Priority](#recommendations-by-priority)
5. [Implementation Guidance](#implementation-guidance)

---

## Executive Summary

### Overall Assessment

The Mitosis API and CLI are **well-designed** with solid RESTful foundations and a consistent hierarchical command structure. However, there are opportunities to improve consistency, add convenience endpoints, and simplify user interactions.

### Key Strengths

- ✅ Resource-oriented API design with proper HTTP verb usage
- ✅ UUID-based resource identification
- ✅ Comprehensive filtering and querying capabilities
- ✅ Well-structured CLI with logical command hierarchy
- ✅ Rich support for tags, labels, and metadata across resources

### Key Areas for Improvement

- ⚠️ Inconsistent batch operation naming (verbs in URL paths)
- ⚠️ Missing essential list/discovery endpoints
- ⚠️ Terminology inconsistencies ("cancel" vs "shutdown")
- ⚠️ Complex CLI argument formats for resources
- 🆕 Missing convenience endpoints for common workflows

---

## HTTP API Endpoints Review

### Current API Structure

```
Root Endpoints:
  GET  /health              - Health check
  POST /login               - User authentication
  GET  /auth                - Get current authenticated user
  GET  /redis               - Get Redis connection info

Resource Endpoints:
  /users/*                  - User management
  /groups/*                 - Group management
  /workers/*                - Worker management
  /tasks/*                  - Task management
  /admin/*                  - Administrative operations
```

---

### Issue 1: Non-RESTful Batch Operation Naming

**Severity**: HIGH
**Category**: RESTful Design

#### Problem

Batch operations use verbs in URL paths, violating RESTful conventions:

```
❌ POST /tasks/cancel              (verb in path)
❌ POST /tasks/submit              (verb in path)
❌ POST /tasks/download/artifacts  (verb in path)
❌ POST /tasks/delete/artifacts    (verb in path)
❌ POST /groups/{group_name}/download/attachments
❌ POST /groups/{group_name}/delete/attachments
❌ POST /workers/shutdown          (verb in path)
```

**Location**:
- `netmito/src/api/tasks.rs:35-50`
- `netmito/src/api/groups.rs:38-52`
- `netmito/src/api/workers.rs:57`

#### RESTful Alternatives

**Option A: Use Standard HTTP Methods with Filters**
```
✅ DELETE /tasks?filter={...}      (with query params or body)
✅ POST   /tasks/batch             (for batch creation)
✅ DELETE /tasks/batch             (for batch deletion with body)
```

**Option B: Google API Style (Custom Methods)**
```
✅ POST /tasks:batchCancel         (colon indicates custom method)
✅ POST /tasks/artifacts:batchDownload
✅ POST /tasks/artifacts:batchDelete
```

**Option C: Sub-resources for Batch Operations**
```
✅ POST   /tasks/batch/submit
✅ DELETE /tasks/batch/cancel
✅ POST   /artifacts/batch/download
```

#### Recommended Approach

**Use Option A for simple operations**, **Option B for complex batch operations**:

```rust
// tasks.rs - Recommended changes
pub fn tasks_router(st: InfraPool) -> Router<InfraPool> {
    Router::new()
        .route("/", post(submit_task))
        .route("/batch", post(batch_submit_tasks))  // ✅ Renamed from /submit
        .route(
            "/{uuid}",
            get(query_task).put(change_task).delete(cancel_task),
        )
        .route("/{uuid}/labels", put(change_task_labels))

        // Batch operations - use custom method style
        .route("/actions/cancel", post(cancel_tasks_batch))  // ✅ Changed
        .route("/query", post(query_tasks))  // Keep for complex filters

        // Artifact operations
        .route("/{uuid}/artifacts", post(upload_artifact))
        .route("/{uuid}/artifacts/{content_type}",
            get(download_artifact).delete(delete_artifact))
        .route("/artifacts:batchDownload", post(batch_download_artifacts))  // ✅ Changed
        .route("/artifacts:batchDelete", post(batch_delete_artifacts))      // ✅ Changed
        // ... rest of routes
}
```

---

### Issue 2: Missing Essential List Endpoints

**Severity**: HIGH
**Category**: API Completeness

#### Problem

No way to list/discover resources without prior knowledge:

```
❌ GET /groups                     - List all accessible groups (missing)
❌ GET /users                      - List users (missing, admin only)
❌ GET /users/me                   - Get current user details (missing)
```

Currently, you can only:
- `GET /users/groups` - Get groups for current user (indirect)
- `POST /groups/query` - Doesn't exist
- Must know group name to do `GET /groups/{group_name}`

#### Impact

- **Discoverability**: Users can't discover what groups they have access to
- **API Usability**: Requires out-of-band knowledge of resource names
- **Standard Compliance**: Most REST APIs provide collection endpoints

#### Recommended Implementation

**Step 1: Add GET /groups endpoint**

```rust
// netmito/src/api/groups.rs

pub fn groups_router(st: InfraPool) -> Router<InfraPool> {
    Router::new()
        .route("/", post(create_group).get(list_groups))  // ✅ Add list_groups
        .route("/{group_name}", get(get_group))
        // ... rest of routes
}

// New handler
pub async fn list_groups(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
) -> Result<Json<GroupsListResp>, ApiError> {
    // Reuse existing logic from query_groups in users.rs
    let groups = service::group::query_user_groups(u.id, &pool)
        .await
        .map_err(|e| match e {
            crate::error::Error::AuthError(err) => ApiError::AuthError(err),
            crate::error::Error::ApiError(e) => e,
            _ => {
                tracing::error!("{}", e);
                ApiError::InternalServerError
            }
        })?;
    Ok(Json(GroupsListResp { groups }))
}
```

**Step 2: Add GET /users/me endpoint**

```rust
// netmito/src/api/users.rs

pub fn users_router(st: InfraPool) -> Router<InfraPool> {
    Router::new()
        .route("/me", get(get_current_user))  // ✅ Add this
        .route("/{username}/password", post(change_password))
        .route("/groups", get(query_groups))
        // ... rest
}

pub async fn get_current_user(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
) -> Result<Json<UserInfoResp>, ApiError> {
    let user_info = service::user::get_user_by_id(u.id, &pool)
        .await
        .map_err(|e| match e {
            crate::error::Error::AuthError(err) => ApiError::AuthError(err),
            crate::error::Error::ApiError(e) => e,
            _ => {
                tracing::error!("{}", e);
                ApiError::InternalServerError
            }
        })?;
    Ok(Json(user_info))
}
```

**Step 3: Add admin GET /users endpoint**

```rust
// netmito/src/api/admin.rs

pub fn admin_router(st: InfraPool, cancel_token: CancellationToken) -> Router<InfraPool> {
    Router::new()
        .route("/users", post(admin_create_user).get(admin_list_users))  // ✅ Add GET
        // ... rest
}

pub async fn admin_list_users(
    Extension(_): Extension<AuthAdminUser>,
    State(pool): State<InfraPool>,
) -> Result<Json<UsersListResp>, ApiError> {
    let users = service::user::list_all_users(&pool.db)
        .await
        .map_err(|e| match e {
            crate::error::Error::ApiError(e) => e,
            _ => {
                tracing::error!("{}", e);
                ApiError::InternalServerError
            }
        })?;
    Ok(Json(UsersListResp { users }))
}
```

**Step 4: Add schema definitions**

```rust
// netmito/src/schema.rs

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GroupsListResp {
    pub groups: Vec<GroupInfo>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UserInfoResp {
    pub username: String,
    pub admin: bool,
    pub state: UserState,
    pub group_quota: i32,
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UsersListResp {
    pub users: Vec<UserInfo>,
}
```

---

### Issue 3: Inconsistent Endpoint Patterns

**Severity**: MEDIUM
**Category**: API Consistency

#### Problem

Similar operations have different patterns across resources:

**Groups User Management**:
```rust
PUT    /groups/{group_name}/users           - Update user roles
DELETE /groups/{group_name}/users           - Remove users (with query params)
POST   /groups/{group_name}/users/remove    - Remove users (with body)
```

**Workers Group Management**:
```rust
PUT    /workers/{uuid}/groups               - Update worker groups
DELETE /workers/{uuid}/groups               - Remove groups (with query params)
DELETE /workers/{uuid}/groups/remove        - Remove groups (with body)
```

**Location**:
- `netmito/src/api/groups.rs:23-27, 116-152`
- `netmito/src/api/workers.rs:50-54, 314-350`

#### Recommended Solution

**Standardize on one pattern** - Use HTTP methods without `/remove` path:

```rust
// groups.rs - Recommended changes
pub fn groups_router(st: InfraPool) -> Router<InfraPool> {
    Router::new()
        .route("/", post(create_group).get(list_groups))
        .route("/{group_name}", get(get_group))
        .route(
            "/{group_name}/users",
            put(update_user_group).delete(remove_user_group)  // ✅ Only these two
        )
        // ❌ Remove this:
        // .route("/{group_name}/users/remove", post(remove_user_group))
        // ... rest
}

// Support both query params AND body for DELETE
pub async fn remove_user_group(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Path(group_name): Path<String>,
    Query(params): Query<Option<RemoveUserGroupRoleParams>>,
    Json(body): Json<Option<RemoveUserGroupRoleReq>>,
) -> ApiResult<()> {
    // Accept users from either query params or body
    let users = if let Some(params) = params {
        params.users
    } else if let Some(body) = body {
        body.users
    } else {
        return Err(ApiError::InvalidRequest("users parameter required".to_string()));
    };

    service::group::remove_user_group_role(u.id, group_name, users, &pool)
        .await
        .map_err(|e| match e {
            crate::error::Error::AuthError(err) => ApiError::AuthError(err),
            crate::error::Error::ApiError(e) => e,
            _ => {
                tracing::error!("{}", e);
                ApiError::InternalServerError
            }
        })?;
    Ok(())
}
```

Apply the same pattern to workers.rs.

---

### Issue 4: Confusing Download vs Query Endpoints

**Severity**: LOW
**Category**: API Clarity

#### Problem

Two similar endpoints with unclear distinction:

```rust
GET /groups/{group_name}/attachments/{key}          - query_single_attachment
GET /groups/{group_name}/download/attachments/{key} - download_attachment
```

Both return `RemoteResourceDownloadResp`, but the semantics are unclear.

**Location**: `netmito/src/api/groups.rs:29-34, 210-226`

#### Analysis

Looking at the implementation:
- `query_single_attachment` → `service::s3::user_query_attachment` → Returns metadata
- `download_attachment` → `service::s3::user_get_attachment` → Returns download URL

#### Recommended Solution

**Rename for clarity**:

```rust
// Option 1: Make the distinction explicit in the path
GET /groups/{group_name}/attachments/{key}          - Get metadata
GET /groups/{group_name}/attachments/{key}/download - Get download URL

// Option 2: Use query parameter
GET /groups/{group_name}/attachments/{key}?download=true

// Option 3: Different response types (not recommended, inconsistent)
```

**Recommended: Use Option 1**

```rust
pub fn groups_router(st: InfraPool) -> Router<InfraPool> {
    Router::new()
        // ... other routes
        .route(
            "/{group_name}/attachments/{*key}",
            get(query_single_attachment)
                .post(upload_attachment)
                .delete(delete_attachment)
        )
        .route(
            "/{group_name}/attachments/{*key}/download",  // ✅ More explicit
            get(download_attachment)
        )
        // ... rest
}
```

---

### Issue 5: Missing Convenience Endpoints

**Severity**: MEDIUM
**Category**: API Usability

#### Problem

Common workflows require multiple API calls that could be simplified with convenience endpoints.

#### Recommended New Endpoints

**1. Group-Scoped Resource Queries**

```rust
// Get all tasks in a group
GET /groups/{group_name}/tasks
// Implementation: Filter tasks by group_name

// Get all workers in a group
GET /groups/{group_name}/workers
// Implementation: Filter workers by group_name

// Get group statistics
GET /groups/{group_name}/stats
// Implementation: Aggregate task counts, storage usage, worker counts
```

**Implementation Example**:

```rust
// netmito/src/api/groups.rs

pub fn groups_router(st: InfraPool) -> Router<InfraPool> {
    Router::new()
        // ... existing routes
        .route("/{group_name}/tasks", get(get_group_tasks))      // ✅ New
        .route("/{group_name}/workers", get(get_group_workers))  // ✅ New
        .route("/{group_name}/stats", get(get_group_stats))      // ✅ New
        // ... rest
}

pub async fn get_group_tasks(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Path(group_name): Path<String>,
) -> Result<Json<TasksQueryResp>, ApiError> {
    // Verify user has access to group
    service::group::verify_user_group_access(u.id, &group_name, &pool)
        .await
        .map_err(|e| match e {
            crate::error::Error::AuthError(err) => ApiError::AuthError(err),
            crate::error::Error::ApiError(e) => e,
            _ => ApiError::InternalServerError
        })?;

    // Query tasks for this group
    let req = TasksQueryReq {
        group_name: Some(group_name),
        ..Default::default()
    };

    let tasks = service::task::query_tasks_by_filter(u.id, &pool, req)
        .await
        .map_err(|e| match e {
            crate::error::Error::AuthError(err) => ApiError::AuthError(err),
            crate::error::Error::ApiError(e) => e,
            _ => ApiError::InternalServerError
        })?;
    Ok(Json(tasks))
}

pub async fn get_group_workers(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Path(group_name): Path<String>,
) -> Result<Json<WorkersQueryResp>, ApiError> {
    service::group::verify_user_group_access(u.id, &group_name, &pool)
        .await
        .map_err(|e| match e {
            crate::error::Error::AuthError(err) => ApiError::AuthError(err),
            crate::error::Error::ApiError(e) => e,
            _ => ApiError::InternalServerError
        })?;

    let req = WorkersQueryReq {
        group_name: Some(group_name),
        ..Default::default()
    };

    let workers = service::worker::query_workers_by_filter(u.id, &pool, req)
        .await
        .map_err(|e| match e {
            crate::error::Error::AuthError(err) => ApiError::AuthError(err),
            crate::error::Error::ApiError(e) => e,
            _ => ApiError::InternalServerError
        })?;
    Ok(Json(workers))
}

pub async fn get_group_stats(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Path(group_name): Path<String>,
) -> Result<Json<GroupStatsResp>, ApiError> {
    let stats = service::group::get_group_stats(u.id, group_name, &pool)
        .await
        .map_err(|e| match e {
            crate::error::Error::AuthError(err) => ApiError::AuthError(err),
            crate::error::Error::ApiError(e) => e,
            _ => ApiError::InternalServerError
        })?;
    Ok(Json(stats))
}
```

**Schema additions**:

```rust
// netmito/src/schema.rs

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GroupStatsResp {
    pub group_name: String,
    pub task_stats: TaskStats,
    pub worker_stats: WorkerStats,
    pub storage_stats: StorageStats,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TaskStats {
    pub total: i64,
    pub by_state: HashMap<TaskState, i64>,
    pub avg_duration: Option<f64>,  // in seconds
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct WorkerStats {
    pub total: i64,
    pub active: i64,
    pub idle: i64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct StorageStats {
    pub quota: i64,
    pub used: i64,
    pub available: i64,
}
```

**2. Task Relationship Endpoints**

```rust
// Get worker that executed/is executing a task
GET /tasks/{uuid}/worker

// Get task dependencies (watch relationships)
GET /tasks/{uuid}/dependencies

// Get all tasks depending on this task
GET /tasks/{uuid}/dependents
```

**Implementation**:

```rust
// netmito/src/api/tasks.rs

pub fn tasks_router(st: InfraPool) -> Router<InfraPool> {
    Router::new()
        // ... existing routes
        .route("/{uuid}/worker", get(get_task_worker))           // ✅ New
        .route("/{uuid}/dependencies", get(get_task_dependencies)) // ✅ New
        .route("/{uuid}/dependents", get(get_task_dependents))    // ✅ New
        // ... rest
}

pub async fn get_task_worker(
    Extension(_): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Path(uuid): Path<Uuid>,
) -> Result<Json<Option<WorkerQueryResp>>, ApiError> {
    let worker = service::task::get_task_worker(&pool, uuid)
        .await
        .map_err(|e| match e {
            crate::error::Error::ApiError(e) => e,
            _ => {
                tracing::error!("{}", e);
                ApiError::InternalServerError
            }
        })?;
    Ok(Json(worker))
}
```

**3. Task Retry/Clone Operations**

```rust
// Retry a failed task (creates new task with same spec)
POST /tasks/{uuid}/retry

// Clone a task with optional modifications
POST /tasks/{uuid}/clone
{
  "tags": ["new-tag"],  // Optional overrides
  "priority": 10
}
```

**Implementation**:

```rust
pub async fn retry_task(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Path(uuid): Path<Uuid>,
) -> Result<Json<SubmitTaskResp>, ApiError> {
    let resp = service::task::retry_task(&pool, u.id, uuid)
        .await
        .map_err(|e| match e {
            crate::error::Error::AuthError(err) => ApiError::AuthError(err),
            crate::error::Error::ApiError(e) => e,
            _ => {
                tracing::error!("{}", e);
                ApiError::InternalServerError
            }
        })?;
    Ok(Json(resp))
}

pub async fn clone_task(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Path(uuid): Path<Uuid>,
    Json(req): Json<CloneTaskReq>,
) -> Result<Json<SubmitTaskResp>, ApiError> {
    let resp = service::task::clone_task(&pool, u.id, uuid, req)
        .await
        .map_err(|e| match e {
            crate::error::Error::AuthError(err) => ApiError::AuthError(err),
            crate::error::Error::ApiError(e) => e,
            _ => {
                tracing::error!("{}", e);
                ApiError::InternalServerError
            }
        })?;
    Ok(Json(resp))
}
```

**4. Aggregation Endpoints**

```rust
// System-wide statistics
GET /stats
GET /admin/stats  // More detailed admin view

// Per-resource statistics
GET /tasks/stats
GET /workers/stats
```

**Implementation**:

```rust
// netmito/src/api/mod.rs

pub fn router(st: InfraPool, cancel_token: CancellationToken) -> Router {
    Router::new()
        // ... existing routes
        .route(
            "/stats",
            get(get_stats).layer(middleware::from_fn_with_state(
                st.clone(),
                user_auth_middleware,
            )),
        )
        // ... rest
}

pub async fn get_stats(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
) -> Result<Json<SystemStatsResp>, ApiError> {
    let stats = service::stats::get_user_stats(u.id, &pool)
        .await
        .map_err(|e| match e {
            crate::error::Error::ApiError(e) => e,
            _ => {
                tracing::error!("{}", e);
                ApiError::InternalServerError
            }
        })?;
    Ok(Json(stats))
}
```

---

### Issue 6: Inconsistent Query Parameter vs Body Handling

**Severity**: LOW
**Category**: API Consistency

#### Problem

Some endpoints accept parameters both as query params and in request body:
- `DELETE /groups/{group_name}/users` - Both query and body
- `DELETE /workers/{uuid}` - Query params for `op`

#### Recommendation

**Standardize based on HTTP semantics**:

1. **Simple flags/options** → Query parameters
   ```
   DELETE /workers/{uuid}?force=true
   ```

2. **Complex filters/arrays** → Request body
   ```
   DELETE /groups/{group_name}/users
   Body: {"users": ["user1", "user2"]}
   ```

3. **Make it consistent** - Pick one approach per endpoint type

**Implementation**:

```rust
// Use query params for simple options
#[derive(Debug, Deserialize)]
pub struct DeleteOptions {
    #[serde(default)]
    pub force: bool,
}

pub async fn user_shutdown_worker(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Path(uuid): Path<Uuid>,
    Query(opts): Query<DeleteOptions>,  // ✅ Simple option as query param
) -> Result<(), ApiError> {
    let op = if opts.force {
        WorkerShutdownOp::Force
    } else {
        WorkerShutdownOp::Graceful
    };
    // ... rest
}

// Use body for complex data
pub async fn remove_user_group(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Path(group_name): Path<String>,
    Json(req): Json<RemoveUserGroupRoleReq>,  // ✅ Complex data in body
) -> ApiResult<()> {
    // ... rest
}
```

---

## CLI Interface Review

### Current CLI Structure

```
mito
├── coordinator        - Run coordinator server
├── worker             - Run worker node
├── client             - Run client CLI
│   ├── admin          - Admin operations
│   ├── auth           - Authenticate current user
│   ├── login          - Login with credentials
│   ├── users          - User management
│   ├── groups         - Group management
│   ├── tasks          - Task management
│   ├── workers        - Worker management
│   ├── cmd            - Execute external command
│   └── quit/exit      - Exit interactive mode
└── manager            - Run worker manager
```

---

### Issue 7: Inconsistent Terminology - "Cancel" vs "Shutdown" vs "Delete"

**Severity**: HIGH
**Category**: CLI/API Consistency

#### Problem

Inconsistent terminology across CLI and API for stopping/removing resources:

**CLI Commands**:
```bash
mito client workers cancel <uuid>        # CLI uses "cancel"
mito client tasks cancel <uuid>          # CLI uses "cancel"
mito client admin users delete <user>    # CLI uses "delete"
```

**API Handlers & Types**:
```rust
// workers.rs
user_shutdown_worker()              // API uses "shutdown"
WorkersShutdownByFilterReq          // Schema uses "shutdown"
WorkerShutdownOp                    // Schema uses "shutdown"

// tasks.rs
cancel_task()                       // API uses "cancel"
TasksCancelByFilterReq              // Schema uses "cancel"

// admin.rs
admin_delete_user()                 // API uses "delete"
```

**Location**:
- `netmito/src/config/client/workers.rs:24`
- `netmito/src/api/workers.rs:235-255`
- `netmito/src/schema.rs` (WorkerShutdownOp, etc.)

#### Impact

- **Confusing Documentation**: API docs vs CLI docs use different terms
- **Code Inconsistency**: Internal code mixes terminology
- **User Experience**: Users unsure which term to use when

#### Recommended Solution

**Standardize terminology based on semantics**:

1. **Tasks** → Use "cancel" (represents stopping execution)
   - ✅ `cancel_task()`
   - ✅ CLI: `tasks cancel`
   - Reason: Tasks are execution units that can be cancelled mid-execution

2. **Workers** → Use "shutdown" (represents graceful/force stop)
   - ✅ `shutdown_worker()`
   - ✅ CLI: `workers shutdown`
   - Reason: Workers are services that need proper shutdown procedures

3. **Users/Groups** → Use "delete" (represents permanent removal)
   - ✅ `delete_user()`
   - ✅ CLI: `admin users delete`
   - Reason: User accounts are being permanently removed

**Step-by-Step Implementation**:

**Step 1: Update CLI command names**

```rust
// netmito/src/config/client/workers.rs

#[derive(Subcommand, Serialize, Debug, Deserialize, derive_more::From, Clone)]
pub enum WorkersCommands {
    /// Shutdown a worker                        // ✅ Changed from "Cancel"
    Shutdown(ShutdownWorkerArgs),                // ✅ Renamed
    /// Shutdown multiple workers subject to the filter
    ShutdownMany(ShutdownWorkersArgs),           // ✅ Renamed
    /// Shutdown multiple workers by UUIDs
    ShutdownList(ShutdownWorkersByUuidsArgs),    // ✅ Renamed
    /// Replace tags of a worker
    UpdateTags(WorkerUpdateTagsArgs),
    /// Replace labels of a worker
    UpdateLabels(WorkerUpdateLabelsArgs),
    // ... rest unchanged
}

// Rename all CancelWorker* to ShutdownWorker*
#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct ShutdownWorkerArgs {  // ✅ Renamed from CancelWorkerArgs
    /// The UUID of the worker
    pub uuid: Uuid,
    /// Whether to force the worker to shutdown.
    #[arg(short, long)]
    pub force: bool,
}

// Apply same renaming to ShutdownWorkersArgs, ShutdownWorkersByUuidsArgs
```

**Step 2: Update CLI client implementation**

```rust
// netmito/src/client/mod.rs or http.rs

// Find all references to "cancel_worker" and update to "shutdown_worker"
// Update method names to match new terminology
impl Client {
    pub async fn shutdown_worker(&self, args: ShutdownWorkerArgs) -> Result<()> {
        // ... implementation
    }

    pub async fn shutdown_workers(&self, args: ShutdownWorkersArgs) -> Result<()> {
        // ... implementation
    }
}
```

**Step 3: Keep API endpoint paths unchanged (for backward compatibility)**

```rust
// netmito/src/api/workers.rs
// Keep endpoint paths as-is to avoid breaking existing clients
// Only update handler names for internal clarity

pub fn workers_router(st: InfraPool) -> Router<InfraPool> {
    Router::new()
        // Keep path as /shutdown for backward compatibility
        .route("/shutdown", post(user_shutdown_workers))
        .route("/shutdown/list", post(user_shutdown_workers_by_uuids))
        // Or optionally support both paths during transition:
        // .route("/cancel", post(user_shutdown_workers))  // Deprecated
        // .route("/shutdown", post(user_shutdown_workers))  // New
        // ... rest
}

// Handlers can be renamed for clarity
pub async fn user_shutdown_worker(/* ... */) { /* ... */ }
pub async fn user_shutdown_workers(/* ... */) { /* ... */ }
```

**Step 4: Update documentation**

Update all documentation, help text, and error messages to use consistent terminology.

---

### Issue 8: Complex Resource Specification Format

**Severity**: MEDIUM
**Category**: CLI Usability

#### Problem

The `--resources` argument uses a custom, hard-to-remember format:

```bash
mito client tasks submit \
  --resources artifact=uuid:content_type:path,attachment=key:path \
  -- command args
```

**Issues**:
- Custom parsing logic required
- Easy to make syntax errors
- Different delimiters (`:` vs `,`)
- Not self-documenting
- Shell escaping can be problematic

**Location**: `netmito/src/config/client/mod.rs:152-192`

#### Recommended Solutions

**Option A: Separate Flags (Recommended)**

```bash
# More explicit and shell-friendly
mito client tasks submit \
  --artifact uuid:result:./output.txt \
  --artifact uuid2:exec-log:./exec.log \
  --attachment myfile:./local.txt \
  -- command args
```

**Implementation**:

```rust
// netmito/src/config/client/tasks.rs

#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct SubmitTaskArgs {
    // ... existing fields ...

    /// Artifacts to download before task execution.
    /// Format: UUID:CONTENT_TYPE:LOCAL_PATH
    /// Content types: result, exec-log, std-log
    #[arg(long = "artifact", num_args = 0.., value_parser = parse_artifact_resource)]
    pub artifacts: Vec<RemoteResourceDownload>,

    /// Attachments to download before task execution.
    /// Format: KEY:LOCAL_PATH
    #[arg(long = "attachment", num_args = 0.., value_parser = parse_attachment_resource)]
    pub attachments: Vec<RemoteResourceDownload>,

    // ❌ Remove old --resources field
}

// Simpler parsers
fn parse_artifact_resource(
    s: &str,
) -> Result<RemoteResourceDownload, Box<dyn std::error::Error + Send + Sync + 'static>> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 3 {
        return Err(format!(
            "artifact format: UUID:CONTENT_TYPE:PATH (got '{}')",
            s
        ).into());
    }

    let uuid = parts[0].parse::<Uuid>()?;
    let content_type = parse_artifact_content_type(parts[1])?;
    let local_path = PathBuf::from(parts[2]);

    Ok(RemoteResourceDownload {
        remote_file: RemoteResource::Artifact { uuid, content_type },
        local_path,
    })
}

fn parse_attachment_resource(
    s: &str,
) -> Result<RemoteResourceDownload, Box<dyn std::error::Error + Send + Sync + 'static>> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 2 {
        return Err(format!(
            "attachment format: KEY:PATH (got '{}')",
            s
        ).into());
    }

    let key = parts[0].to_string();
    let local_path = PathBuf::from(parts[1]);

    Ok(RemoteResourceDownload {
        remote_file: RemoteResource::Attachment { key },
        local_path,
    })
}
```

**Usage comparison**:

```bash
# Old (confusing)
--resources artifact=abc-123:result:/tmp/out.txt,attachment=myfile:/tmp/input.txt

# New (clearer)
--artifact abc-123:result:/tmp/out.txt --attachment myfile:/tmp/input.txt

# Multiple resources
--artifact abc-123:result:/tmp/out.txt \
--artifact def-456:exec-log:/tmp/exec.log \
--attachment file1:/tmp/input1.txt \
--attachment file2:/tmp/input2.txt
```

**Option B: JSON File (For complex cases)**

```bash
# For complex task submissions, use a config file
mito client tasks submit --config task-config.json
```

```json
{
  "group_name": "my-group",
  "tags": ["gpu", "high-memory"],
  "resources": {
    "artifacts": [
      {"uuid": "abc-123", "content_type": "result", "local_path": "/tmp/out.txt"},
      {"uuid": "def-456", "content_type": "exec-log", "local_path": "/tmp/exec.log"}
    ],
    "attachments": [
      {"key": "input-data", "local_path": "/tmp/input.txt"}
    ]
  },
  "command": ["python", "train.py"]
}
```

**Implementation for JSON option**:

```rust
#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct SubmitTaskArgs {
    /// Path to task configuration file (JSON)
    #[arg(long, conflicts_with_all = ["group_name", "tags", "command"])]
    pub config: Option<PathBuf>,

    // ... other fields ...
}
```

---

### Issue 9: Deep Command Nesting

**Severity**: MEDIUM
**Category**: CLI Usability

#### Problem

Some admin commands require very deep nesting:

```bash
# Very verbose
mito client admin groups attachments delete mygroup myfile.txt
mito client admin tasks artifacts delete task-uuid result
mito client admin users delete username
```

**Location**: `netmito/src/config/client/admin.rs:75-143`

#### Recommended Solution

**Add flat alternatives for common operations**:

**Option A: Add top-level delete command**

```bash
# Shorter alternatives
mito client admin delete user USERNAME
mito client admin delete group-attachment GROUP KEY
mito client admin delete task-artifact UUID TYPE
mito client admin delete worker UUID
```

**Implementation**:

```rust
// netmito/src/config/client/admin.rs

#[derive(Subcommand, Serialize, Debug, Deserialize, derive_more::From, Clone)]
pub enum AdminCommands {
    /// Manage users
    Users(AdminUsersArgs),
    /// Shutdown the coordinator
    Shutdown(ShutdownArgs),
    /// Manage groups
    Groups(AdminGroupsArgs),
    /// Manage tasks
    Tasks(AdminTasksArgs),
    /// Manage workers
    Workers(AdminWorkersArgs),

    /// Delete resources (convenience command)  // ✅ New
    Delete(AdminDeleteArgs),                    // ✅ New
}

#[derive(Serialize, Debug, Deserialize, Args, derive_more::From, Clone)]
pub struct AdminDeleteArgs {
    #[command(subcommand)]
    pub command: AdminDeleteCommands,
}

#[derive(Subcommand, Serialize, Debug, Deserialize, derive_more::From, Clone)]
pub enum AdminDeleteCommands {
    /// Delete a user
    User(AdminDeleteUserArgs),
    /// Delete a group attachment
    GroupAttachment {
        group_name: String,
        key: String,
    },
    /// Delete a task artifact
    TaskArtifact {
        uuid: Uuid,
        content_type: ArtifactContentType,
    },
    /// Shutdown a worker
    Worker {
        uuid: Uuid,
        #[arg(short, long)]
        force: bool,
    },
}
```

**Option B: Support resource-first syntax**

```bash
# Instead of: mito client admin groups storage-quota GROUP QUOTA
# Support:    mito client admin groups set-storage-quota --group GROUP --quota QUOTA

# Instead of: mito client admin users change-password USER PASS
# Support:    mito client admin users set-password --user USER --password PASS
```

This makes commands more uniform and easier to remember.

---

### Issue 10: Missing CLI Commands

**Severity**: MEDIUM
**Category**: CLI Completeness

#### Problem

Some useful operations are missing from CLI:

1. **List operations** - Can't easily discover resources
2. **Watch operations** - No real-time monitoring
3. **Convenience operations** - Common workflows require multiple commands

#### Recommended Additions

**1. Add List Commands**

```bash
# List accessible groups
mito client groups list

# List users (admin only)
mito client admin users list

# List attachments in a group
mito client groups attachments list GROUP
```

**Implementation**:

```rust
// netmito/src/config/client/groups.rs

#[derive(Subcommand, Serialize, Debug, Deserialize, derive_more::From, Clone)]
pub enum GroupsCommands {
    /// Create a new group
    Create(GroupCreateArgs),
    /// List all accessible groups  // ✅ New
    List,                            // ✅ New
    /// Get the information of a group
    Get(GroupGetArgs),
    // ... rest
}

// Handler in client
impl Client {
    pub async fn list_groups(&self) -> Result<Vec<GroupInfo>> {
        // Call GET /groups endpoint
        let resp: GroupsListResp = self.get("/groups").await?;
        Ok(resp.groups)
    }
}
```

**2. Add Watch Commands (Real-time Monitoring)**

```bash
# Watch task status until completion
mito client tasks watch UUID

# Watch worker status
mito client workers watch UUID

# Watch all tasks matching filter
mito client tasks watch-all --group mygroup --state Running
```

**Implementation**:

```rust
// netmito/src/config/client/tasks.rs

#[derive(Subcommand, Serialize, Debug, Deserialize, derive_more::From, Clone)]
pub enum TasksCommands {
    // ... existing commands ...

    /// Watch task status until completion  // ✅ New
    Watch(WatchTaskArgs),                   // ✅ New
}

#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct WatchTaskArgs {
    /// The UUID of the task to watch
    pub uuid: Uuid,

    /// Polling interval in seconds
    #[arg(short, long, default_value = "2")]
    pub interval: u64,

    /// Exit when task reaches this state
    #[arg(short, long)]
    pub until: Option<TaskState>,
}

// Handler implementation
impl Client {
    pub async fn watch_task(&self, args: WatchTaskArgs) -> Result<()> {
        loop {
            let task: TaskQueryResp = self
                .get(&format!("/tasks/{}", args.uuid))
                .await?;

            // Print task status
            println!("Task {} - State: {:?}", args.uuid, task.state);

            // Check exit condition
            if let Some(until_state) = &args.until {
                if &task.state == until_state {
                    break;
                }
            }
            if task.state.is_terminal() {
                break;
            }

            tokio::time::sleep(Duration::from_secs(args.interval)).await;
        }
        Ok(())
    }
}
```

**3. Add Retry/Clone Commands**

```bash
# Retry a failed task
mito client tasks retry UUID

# Clone a task with modifications
mito client tasks clone UUID --tags new-tag --priority 10
```

**Implementation** (requires corresponding API endpoint from Issue 5):

```rust
#[derive(Subcommand, Serialize, Debug, Deserialize, derive_more::From, Clone)]
pub enum TasksCommands {
    // ... existing commands ...

    /// Retry a failed task            // ✅ New
    Retry(RetryTaskArgs),               // ✅ New
    /// Clone a task with modifications // ✅ New
    Clone(CloneTaskArgs),               // ✅ New
}

#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct RetryTaskArgs {
    /// The UUID of the task to retry
    pub uuid: Uuid,
}

#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct CloneTaskArgs {
    /// The UUID of the task to clone
    pub uuid: Uuid,

    /// Override tags
    #[arg(short, long, num_args = 0.., value_delimiter = ',')]
    pub tags: Vec<String>,

    /// Override priority
    #[arg(short, long)]
    pub priority: Option<i32>,

    /// Override timeout
    #[arg(long, value_parser = humantime_serde::re::humantime::parse_duration)]
    pub timeout: Option<std::time::Duration>,
}
```

---

### Issue 11: Filter Argument Format

**Severity**: LOW
**Category**: CLI Usability

#### Problem

Comparison operators embedded in strings can be problematic:

```bash
# Current approach
mito client tasks query --exit-status ">=0" --priority "!=5"

# Issues:
# - Shell escaping problems
# - String parsing required
# - Not discoverable in --help
```

**Location**: `netmito/src/config/client/tasks.rs:103-108`

#### Recommended Solution

**Option A: Separate operator flags**

```bash
# More explicit
mito client tasks query \
  --exit-status 0 --exit-status-op gte \
  --priority 5 --priority-op ne
```

**Option B: Multiple comparison flags**

```bash
# Verbose but clear
mito client tasks query \
  --exit-status-gte 0 \
  --priority-ne 5
```

**Implementation (Option B - Recommended)**:

```rust
// netmito/src/config/client/tasks.rs

#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct QueryTasksArgs {
    // ... existing fields ...

    // ❌ Remove old string-based approach:
    // pub exit_status: Option<String>,
    // pub priority: Option<String>,

    // ✅ Add explicit comparison flags:
    /// Filter by exact exit status
    #[arg(long)]
    pub exit_status: Option<i32>,

    /// Filter by exit status not equal to
    #[arg(long)]
    pub exit_status_ne: Option<i32>,

    /// Filter by exit status less than
    #[arg(long)]
    pub exit_status_lt: Option<i32>,

    /// Filter by exit status less than or equal
    #[arg(long)]
    pub exit_status_lte: Option<i32>,

    /// Filter by exit status greater than
    #[arg(long)]
    pub exit_status_gt: Option<i32>,

    /// Filter by exit status greater than or equal
    #[arg(long)]
    pub exit_status_gte: Option<i32>,

    // Same for priority
    /// Filter by exact priority
    #[arg(long)]
    pub priority: Option<i32>,

    #[arg(long)]
    pub priority_ne: Option<i32>,

    #[arg(long)]
    pub priority_lt: Option<i32>,

    #[arg(long)]
    pub priority_lte: Option<i32>,

    #[arg(long)]
    pub priority_gt: Option<i32>,

    #[arg(long)]
    pub priority_gte: Option<i32>,

    // ... rest
}

impl From<QueryTasksArgs> for TasksQueryReq {
    fn from(args: QueryTasksArgs) -> Self {
        // Convert multiple comparison flags into string format for API
        let exit_status = if let Some(val) = args.exit_status {
            Some(format!("={}", val))
        } else if let Some(val) = args.exit_status_ne {
            Some(format!("!={}", val))
        } else if let Some(val) = args.exit_status_lt {
            Some(format!("<{}", val))
        } else if let Some(val) = args.exit_status_lte {
            Some(format!("<={}", val))
        } else if let Some(val) = args.exit_status_gt {
            Some(format!(">{}", val))
        } else if let Some(val) = args.exit_status_gte {
            Some(format!(">={}", val))
        } else {
            None
        };

        // Same for priority
        let priority = /* ... similar logic ... */;

        Self {
            exit_status,
            priority,
            // ... rest of fields ...
        }
    }
}
```

**Validation**: Add mutual exclusion between comparison flags:

```rust
// Ensure only one comparison operator is used
impl QueryTasksArgs {
    pub fn validate(&self) -> Result<(), String> {
        let exit_status_count = [
            self.exit_status.is_some(),
            self.exit_status_ne.is_some(),
            self.exit_status_lt.is_some(),
            self.exit_status_lte.is_some(),
            self.exit_status_gt.is_some(),
            self.exit_status_gte.is_some(),
        ].iter().filter(|&&x| x).count();

        if exit_status_count > 1 {
            return Err("Only one exit-status comparison can be specified".to_string());
        }

        // Same check for priority

        Ok(())
    }
}
```

---

## Recommendations by Priority

### High Priority (Should implement soon)

1. **✅ Add GET /groups endpoint**
   - **Why**: Essential for resource discovery
   - **Effort**: Low (1-2 hours)
   - **Files**: `netmito/src/api/groups.rs`, `netmito/src/schema.rs`
   - **See**: [Issue 2](#issue-2-missing-essential-list-endpoints)

2. **✅ Standardize batch operation naming**
   - **Why**: Improves API RESTfulness and consistency
   - **Effort**: Medium (4-6 hours, requires testing)
   - **Files**: `netmito/src/api/tasks.rs`, `groups.rs`, `workers.rs`
   - **See**: [Issue 1](#issue-1-non-restful-batch-operation-naming)

3. **✅ Unify "cancel" vs "shutdown" terminology**
   - **Why**: Eliminates user confusion
   - **Effort**: Medium (3-4 hours, mostly renaming)
   - **Files**: `netmito/src/config/client/workers.rs`, `src/client/`
   - **See**: [Issue 7](#issue-7-inconsistent-terminology---cancel-vs-shutdown-vs-delete)

4. **✅ Add GET /users/me endpoint**
   - **Why**: Standard REST pattern, useful for UIs
   - **Effort**: Low (1 hour)
   - **Files**: `netmito/src/api/users.rs`, `netmito/src/schema.rs`
   - **See**: [Issue 2](#issue-2-missing-essential-list-endpoints)

### Medium Priority (Nice to have)

5. **✅ Simplify CLI resource specification**
   - **Why**: Improves CLI usability significantly
   - **Effort**: Medium (3-4 hours)
   - **Files**: `netmito/src/config/client/tasks.rs`, `mod.rs`
   - **See**: [Issue 8](#issue-8-complex-resource-specification-format)

6. **✅ Add group-scoped resource queries**
   - **Why**: Common workflow, reduces API calls
   - **Effort**: Medium (4-5 hours)
   - **Files**: `netmito/src/api/groups.rs`, service layer
   - **See**: [Issue 5, Recommendation 1](#recommended-new-endpoints)

7. **✅ Add list commands to CLI**
   - **Why**: Improves discoverability
   - **Effort**: Low (2-3 hours)
   - **Files**: `netmito/src/config/client/groups.rs`, `users.rs`
   - **See**: [Issue 10, Recommendation 1](#recommended-additions)

8. **✅ Standardize endpoint patterns**
   - **Why**: Improves API consistency
   - **Effort**: Medium (3-4 hours)
   - **Files**: `netmito/src/api/groups.rs`, `workers.rs`
   - **See**: [Issue 3](#issue-3-inconsistent-endpoint-patterns)

### Low Priority (Future enhancements)

9. **✅ Add watch commands**
   - **Why**: Better monitoring experience
   - **Effort**: Medium (4-5 hours)
   - **Files**: `netmito/src/config/client/tasks.rs`, client impl
   - **See**: [Issue 10, Recommendation 2](#recommended-additions)

10. **✅ Add retry/clone operations**
    - **Why**: Quality of life improvement
    - **Effort**: Medium-High (5-6 hours, API + CLI)
    - **Files**: API and service layer, CLI config
    - **See**: [Issue 5, Recommendation 3](#recommended-new-endpoints)

11. **✅ Add statistics endpoints**
    - **Why**: Useful for dashboards and monitoring
    - **Effort**: High (6-8 hours, requires aggregation logic)
    - **Files**: New service module, API endpoints
    - **See**: [Issue 5, Recommendation 4](#recommended-new-endpoints)

12. **✅ Improve filter argument format**
    - **Why**: Better CLI UX
    - **Effort**: Medium (3-4 hours)
    - **Files**: `netmito/src/config/client/tasks.rs`, workers.rs
    - **See**: [Issue 11](#issue-11-filter-argument-format)

13. **✅ Add flat admin commands**
    - **Why**: Convenience for common operations
    - **Effort**: Low (2-3 hours)
    - **Files**: `netmito/src/config/client/admin.rs`
    - **See**: [Issue 9](#issue-9-deep-command-nesting)

14. **✅ Clarify download vs query endpoints**
    - **Why**: API clarity
    - **Effort**: Low (1-2 hours)
    - **Files**: `netmito/src/api/groups.rs`
    - **See**: [Issue 4](#issue-4-confusing-download-vs-query-endpoints)

---

## Implementation Guidance

### Phase 1: Quick Wins (1-2 weeks)

**Goal**: Address high-priority issues with low effort

#### Week 1: API Improvements

**Day 1-2: Add list endpoints**
1. Implement `GET /groups` in `netmito/src/api/groups.rs`
2. Implement `GET /users/me` in `netmito/src/api/users.rs`
3. Implement `GET /admin/users` in `netmito/src/api/admin.rs`
4. Add corresponding schema types in `netmito/src/schema.rs`
5. Update `openapi.yaml` with new endpoints
6. Write tests for new endpoints

**Day 3-4: Standardize terminology**
1. Rename CLI commands in `netmito/src/config/client/workers.rs`
   - `CancelWorkerArgs` → `ShutdownWorkerArgs`
   - Update all related commands
2. Update client implementation in `netmito/src/client/`
3. Update help text and documentation
4. Ensure backward compatibility in API

**Day 5: CLI list commands**
1. Add `List` command to `GroupsCommands`
2. Add `List` command to `AdminUsersCommands`
3. Implement client methods
4. Test interactive and non-interactive modes

#### Week 2: API Consistency

**Day 1-3: Refactor batch operations**
1. Design new URL structure (decide on Option A/B from Issue 1)
2. Update `netmito/src/api/tasks.rs`:
   - Rename `/submit` → `/batch`
   - Change `/cancel` → `/actions/cancel` or `:batchCancel`
   - Update `/download/artifacts` and `/delete/artifacts`
3. Update `netmito/src/api/groups.rs` similarly
4. Maintain backward compatibility (support old paths temporarily)
5. Update OpenAPI spec
6. Add deprecation warnings to old endpoints

**Day 4-5: Simplify resource specification**
1. Update `SubmitTaskArgs` in `netmito/src/config/client/tasks.rs`
2. Add separate `--artifact` and `--attachment` flags
3. Implement new parser functions
4. Update documentation and examples
5. Test with complex resource specifications

### Phase 2: Convenience Features (2-3 weeks)

**Goal**: Add commonly-used convenience endpoints

#### Week 1: Group-scoped queries

**Day 1-2: Implement endpoints**
```rust
GET /groups/{group_name}/tasks
GET /groups/{group_name}/workers
GET /groups/{group_name}/stats
```

1. Add routes to `groups_router()`
2. Implement handlers (reuse existing service functions with filters)
3. Add authorization checks
4. Update schema with response types

**Day 3-4: Add to CLI**
1. Add corresponding CLI commands:
   ```bash
   mito client groups tasks GROUP
   mito client groups workers GROUP
   mito client groups stats GROUP
   ```
2. Implement client methods
3. Format output nicely

**Day 5: Testing**
1. Write integration tests
2. Test with different user permissions
3. Update documentation

#### Week 2: Task relationships

**Day 1-3: Implement endpoints**
```rust
GET /tasks/{uuid}/worker
GET /tasks/{uuid}/dependencies
GET /tasks/{uuid}/dependents
```

1. Implement service layer functions
2. Add API handlers
3. Add schema types
4. Write tests

**Day 4-5: Add retry/clone**
```rust
POST /tasks/{uuid}/retry
POST /tasks/{uuid}/clone
```

1. Implement service layer logic
2. Add API handlers
3. Add CLI commands
4. Test edge cases (failed tasks, permissions)

#### Week 3: Watch and monitoring

**Day 1-3: Implement watch commands**
1. Add `Watch` command to `TasksCommands`
2. Implement polling logic in client
3. Add progress indicators
4. Handle interrupts gracefully

**Day 4-5: Add statistics endpoints**
1. Design aggregation logic
2. Implement service layer functions
3. Add API endpoints:
   ```rust
   GET /stats
   GET /tasks/stats
   GET /workers/stats
   GET /admin/stats
   ```
4. Add caching if needed

### Phase 3: Polish and Refinement (1-2 weeks)

**Goal**: Clean up inconsistencies and improve UX

#### Week 1: Consistency improvements

**Day 1-2: Standardize endpoint patterns**
1. Remove duplicate `/remove` endpoints
2. Standardize DELETE with both body and query param support
3. Update all affected endpoints
4. Test backward compatibility

**Day 3-4: Improve filter arguments**
1. Replace string-based operators with explicit flags
2. Update `QueryTasksArgs` and `WorkersQueryArgs`
3. Implement validation logic
4. Update help text

**Day 5: Documentation**
1. Update API documentation
2. Update CLI help text
3. Write migration guide for breaking changes
4. Update examples

#### Week 2: Final touches

**Day 1-2: Add flat admin commands**
1. Implement `AdminDeleteCommands`
2. Add shortcuts for common operations
3. Update help text

**Day 3-4: Testing**
1. Integration tests for all new endpoints
2. CLI command tests
3. Performance testing for aggregation endpoints
4. Security testing for permission checks

**Day 5: Release preparation**
1. Update CHANGELOG
2. Version bump
3. Update OpenAPI spec
4. Prepare release notes

---

### Testing Checklist

For each new feature, ensure:

- [ ] Unit tests for service layer functions
- [ ] Integration tests for API endpoints
- [ ] CLI command tests (both interactive and non-interactive)
- [ ] Permission/authorization tests
- [ ] Error handling tests
- [ ] Documentation updated
- [ ] OpenAPI spec updated
- [ ] Backward compatibility verified (if applicable)
- [ ] Performance acceptable (for aggregation queries)

---

### Migration Strategy

For breaking changes:

1. **Deprecation Phase** (Version X.Y)
   - Add new endpoints/commands
   - Keep old ones working
   - Add deprecation warnings
   - Update documentation

2. **Transition Phase** (Version X.Y+1)
   - Promote new APIs
   - Increase deprecation warning visibility
   - Provide migration tools/scripts

3. **Removal Phase** (Version X+1.0)
   - Remove deprecated endpoints
   - Clean up code
   - Major version bump

---

### Backward Compatibility Notes

**Changes that break compatibility**:
- Renaming CLI commands (cancel → shutdown)
- Changing resource specification format
- Removing `/remove` endpoints
- Changing filter argument format

**Mitigation strategies**:
1. **Support both old and new simultaneously** during transition
2. **Use feature flags** to enable new behavior
3. **Provide clear migration paths** in documentation
4. **Add deprecation warnings** to old APIs
5. **Version the API** (e.g., `/v1/tasks` vs `/v2/tasks`)

**Recommended versioning approach**:
```
v0.7.0 - Add new endpoints, keep old ones
v0.8.0 - Deprecate old endpoints
v1.0.0 - Remove deprecated endpoints (breaking change)
```

---

## Appendix

### Summary of File Changes

| File | Changes | Priority |
|------|---------|----------|
| `netmito/src/api/groups.rs` | Add GET /, /{group_name}/tasks, /{group_name}/workers, /{group_name}/stats; Refactor batch ops | HIGH |
| `netmito/src/api/users.rs` | Add GET /me | HIGH |
| `netmito/src/api/admin.rs` | Add GET /users, /stats | HIGH |
| `netmito/src/api/tasks.rs` | Refactor batch ops; Add /retry, /clone, /{uuid}/worker | MEDIUM |
| `netmito/src/api/workers.rs` | Standardize patterns; Refactor batch ops | MEDIUM |
| `netmito/src/schema.rs` | Add new request/response types | HIGH |
| `netmito/src/service/` | Add new service functions for stats, retry, clone | MEDIUM |
| `netmito/src/config/client/workers.rs` | Rename Cancel → Shutdown | HIGH |
| `netmito/src/config/client/tasks.rs` | Simplify resource spec; Add watch, retry, clone; Improve filters | MEDIUM |
| `netmito/src/config/client/groups.rs` | Add list, tasks, workers commands | MEDIUM |
| `netmito/src/config/client/admin.rs` | Add flat delete commands, list users | LOW |
| `netmito/src/client/` | Implement new client methods | ALL |
| `openapi.yaml` | Update with all API changes | ALL |

### Quick Reference: Current vs Proposed

**API Endpoints**:
```
Current                              | Proposed
-------------------------------------|----------------------------------------
❌ (missing)                         | ✅ GET /groups
❌ (missing)                         | ✅ GET /users/me
POST /tasks/submit                   | ✅ POST /tasks/batch
POST /tasks/cancel                   | ✅ POST /tasks:batchCancel
POST /tasks/download/artifacts       | ✅ POST /tasks/artifacts:batchDownload
❌ (missing)                         | ✅ GET /groups/{name}/tasks
❌ (missing)                         | ✅ GET /groups/{name}/workers
❌ (missing)                         | ✅ POST /tasks/{uuid}/retry
```

**CLI Commands**:
```
Current                              | Proposed
-------------------------------------|----------------------------------------
mito client workers cancel           | ✅ mito client workers shutdown
--resources artifact=...             | ✅ --artifact ... --attachment ...
--exit-status ">=0"                  | ✅ --exit-status-gte 0
❌ (missing)                         | ✅ mito client groups list
❌ (missing)                         | ✅ mito client tasks watch UUID
❌ (missing)                         | ✅ mito client tasks retry UUID
admin groups attachments delete      | ✅ admin delete group-attachment
```

---

## Conclusion

This review identifies **14 distinct issues** across your API and CLI interfaces, with **actionable recommendations** for each. The proposed changes are organized by priority and include:

- **4 High-priority items** that should be addressed soon
- **4 Medium-priority items** that significantly improve usability
- **6 Low-priority items** that provide long-term value

The implementation is structured into **3 phases** over 4-6 weeks, with clear steps, testing checklists, and migration strategies.

**Estimated Total Effort**:
- High Priority: 10-15 hours
- Medium Priority: 20-25 hours
- Low Priority: 20-30 hours
- **Total: 50-70 hours** (6-9 developer-weeks)

For questions or clarification on any recommendation, please refer to the specific issue sections above.
