# Phase 1: Database Schema Implementation Guide

## Overview

This is the foundation phase for the Node Manager and Task Suite System. In this phase, we establish the database schema for all new tables, relationships, triggers, and SeaORM entity models. This phase has no dependencies and must be completed before API development can begin.

**Key Deliverables:**
- 5 new database tables with proper indexes
- 1 ALTER statement to extend `active_tasks`
- 2 database triggers for automatic state management
- 5 new SeaORM entity models
- 3 new enum types

**Philosophy:** The database schema is the single source of truth. Get this right, and the rest of the system will follow naturally.

## Prerequisites

- PostgreSQL database (version 12+)
- SeaORM migration tools installed
- Understanding of PostgreSQL triggers and indexes
- Familiarity with SeaORM entity model generation

## Timeline

**Duration:** 1 week

**Breakdown:**
- Days 1-2: Create migration files and SQL schema
- Day 3: Implement database triggers
- Days 4-5: Create SeaORM entity models and enums
- Day 6: Testing and validation
- Day 7: Buffer for issues and documentation

## Design References

This implementation is based on the following sections from the RFC:

### Section 6.1: New Database Tables

The system introduces five new tables:
1. **task_suites** - Core table for task suite lifecycle management
2. **node_managers** - Device-level services that manage worker pools
3. **group_node_manager** - Permission model (groups have roles on managers)
4. **task_suite_managers** - Assignment tracking (which managers work on which suites)
5. **task_execution_failures** - Failure tracking to prevent infinite retry loops

### Section 6.2: Update to active_tasks Table

Add `task_suite_id` foreign key to link tasks to suites.

### Section 6.3: Database Triggers

Two critical triggers:
1. **update_suite_task_counts()** - Automatically maintains `total_tasks` and `pending_tasks` counters
2. **auto_transition_suite_states()** - Background task to transition suite states (Open/Closed/Complete)

## Implementation Tasks

### Task 1.1: Create Migration Files

Create a new SeaORM migration file for all schema changes. This should be a single migration to ensure atomic deployment.

**File to create:** `migration/src/mXXXX_create_task_suite_system.rs`

**Migration structure:**
```rust
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // 1. Create task_suites table
        // 2. Create node_managers table
        // 3. Create group_node_manager table
        // 4. Create task_suite_managers table
        // 5. Create task_execution_failures table
        // 6. Alter active_tasks table
        // 7. Create triggers
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Reverse all changes
        Ok(())
    }
}
```

### Task 1.2: Implement task_suites Table

**Purpose:** Store task suite definitions, state, and lifecycle metadata.

**Full CREATE TABLE statement:**

```sql
CREATE TABLE task_suites (
    id BIGSERIAL PRIMARY KEY,
    uuid UUID UNIQUE NOT NULL,

    -- Optional human-readable identifiers (non-unique)
    name TEXT,
    description TEXT,

    -- Permissions
    group_id BIGINT NOT NULL REFERENCES groups(id) ON DELETE RESTRICT,
    creator_id BIGINT NOT NULL REFERENCES users(id) ON DELETE RESTRICT,

    -- Scheduling attributes
    tags TEXT[] NOT NULL DEFAULT '{}',
    labels JSONB NOT NULL DEFAULT '[]',  -- array of strings for querying
    priority INTEGER NOT NULL DEFAULT 0,

    -- Worker allocation plan (JSON)
    worker_schedule JSONB NOT NULL,
    -- Example: {"worker_count": 16, "cpu_binding": {"cores": [0,1,2,3], "strategy": "RoundRobin"}, "task_prefetch_count": 32}

    -- Lifecycle hooks (TaskSpec-like, JSON)
    env_preparation JSONB,
    -- Example: {"args": ["./setup.sh"], "envs": {"DATA_DIR": "/mnt/data"}, "resources": [...], "timeout": "5m"}
    env_cleanup JSONB,
    -- Example: {"args": ["./cleanup.sh"], "envs": {}, "resources": [], "timeout": "2m"}

    -- State management
    state INTEGER NOT NULL DEFAULT 0,  -- Open=0, Closed=1, Complete=2, Cancelled=3
    last_task_submitted_at TIMESTAMPTZ,
    total_tasks INTEGER NOT NULL DEFAULT 0,  -- Total tasks ever submitted to this suite
    pending_tasks INTEGER NOT NULL DEFAULT 0,  -- Currently pending/active tasks

    -- Timestamps
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at TIMESTAMPTZ
);
```

**Indexes:**

```sql
-- Indices for efficient queries
CREATE INDEX idx_task_suites_group_id ON task_suites(group_id);
CREATE INDEX idx_task_suites_creator_id ON task_suites(creator_id);
CREATE INDEX idx_task_suites_state ON task_suites(state);
CREATE INDEX idx_task_suites_tags ON task_suites USING GIN(tags);
CREATE INDEX idx_task_suites_labels ON task_suites USING GIN(labels);

-- Index for auto-close timeout queries (fixed 3-minute timeout)
CREATE INDEX idx_task_suites_auto_close
    ON task_suites(last_task_submitted_at)
    WHERE state = 0;

-- Index for label-based queries
-- Supports queries like: labels @> '["ml-training"]'::jsonb
```

**Key Design Notes:**
- `uuid` is the external identifier (exposed in API)
- `id` is the internal primary key (used in foreign keys)
- `tags` is a PostgreSQL array (TEXT[]) for manager matching
- `labels` is JSONB for flexible querying
- `worker_schedule`, `env_preparation`, and `env_cleanup` are JSONB for flexibility
- `state` is an integer enum (0=Open, 1=Closed, 2=Complete, 3=Cancelled)
- `total_tasks` and `pending_tasks` are maintained by triggers (see Task 1.7)

### Task 1.3: Implement node_managers Table

**Purpose:** Store node manager registrations, state, and current assignments.

**Full CREATE TABLE statement:**

```sql
CREATE TABLE node_managers (
    id BIGSERIAL PRIMARY KEY,
    uuid UUID UNIQUE NOT NULL,
    creator_id BIGINT NOT NULL REFERENCES users(id) ON DELETE RESTRICT,

    -- Capabilities
    tags TEXT[] NOT NULL DEFAULT '{}',
    labels JSONB NOT NULL DEFAULT '[]',  -- array of strings for querying

    -- State
    state INTEGER NOT NULL DEFAULT 0,  -- Idle=0, Preparing=1, Executing=2, Cleanup=3, Offline=4
    last_heartbeat TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    -- Current assignment
    assigned_task_suite_id BIGINT REFERENCES task_suites(id) ON DELETE SET NULL,
    lease_expires_at TIMESTAMPTZ,

    -- Metadata
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
```

**Indexes:**

```sql
-- Indices
CREATE INDEX idx_node_managers_creator_id ON node_managers(creator_id);
CREATE INDEX idx_node_managers_state ON node_managers(state);
CREATE INDEX idx_node_managers_tags ON node_managers USING GIN(tags);
CREATE INDEX idx_node_managers_labels ON node_managers USING GIN(labels);
CREATE INDEX idx_node_managers_assigned_suite ON node_managers(assigned_task_suite_id)
    WHERE assigned_task_suite_id IS NOT NULL;
CREATE INDEX idx_node_managers_heartbeat ON node_managers(last_heartbeat);
```

**Key Design Notes:**
- `state` tracks the manager's current lifecycle state
- `assigned_task_suite_id` is nullable (NULL when idle)
- `lease_expires_at` provides timeout mechanism for suite assignments
- `last_heartbeat` is used for offline detection
- GIN indexes on `tags` and `labels` support efficient tag matching

### Task 1.4: Implement Join Tables

#### 1.4.1: group_node_manager Table

**Purpose:** Permission model - groups have roles ON managers (same pattern as group_worker).

**Full CREATE TABLE statement:**

```sql
CREATE TABLE group_node_manager (
    id BIGSERIAL PRIMARY KEY,
    group_id BIGINT NOT NULL REFERENCES groups(id) ON DELETE RESTRICT,
    manager_id BIGINT NOT NULL REFERENCES node_managers(id) ON DELETE CASCADE,
    role INTEGER NOT NULL,  -- 0=Read, 1=Write, 2=Admin (same as GroupWorkerRole)

    UNIQUE(group_id, manager_id)
);

CREATE INDEX idx_group_node_manager_group ON group_node_manager(group_id);
CREATE INDEX idx_group_node_manager_manager ON group_node_manager(manager_id);
```

**Role Definitions:**
- **Read (0)**: Reserved for future use (view manager status)
- **Write (1)**: Group can submit task suites to manager (required for execution)
- **Admin (2)**: Group can manage manager ACL and settings

#### 1.4.2: task_suite_managers Table

**Purpose:** Track which managers are assigned to which suites (user-specified or auto-matched).

**Full CREATE TABLE statement:**

```sql
CREATE TABLE task_suite_managers (
    id BIGSERIAL PRIMARY KEY,
    task_suite_id BIGINT NOT NULL REFERENCES task_suites(id) ON DELETE CASCADE,
    manager_id BIGINT NOT NULL REFERENCES node_managers(id) ON DELETE CASCADE,

    -- Selection method
    selection_type INTEGER NOT NULL,  -- 0=UserSpecified, 1=TagMatched

    -- For tag-matched, record which tags matched
    matched_tags TEXT[],

    -- Audit
    added_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    added_by_user_id BIGINT REFERENCES users(id),

    UNIQUE(task_suite_id, manager_id)
);

CREATE INDEX idx_task_suite_managers_suite ON task_suite_managers(task_suite_id);
CREATE INDEX idx_task_suite_managers_manager ON task_suite_managers(manager_id);
CREATE INDEX idx_task_suite_managers_selection_type ON task_suite_managers(selection_type);
```

**Key Design Notes:**
- `selection_type` distinguishes between user-specified (0) and tag-matched (1) assignments
- `matched_tags` records which tags were used for auto-matching (NULL for user-specified)
- `added_by_user_id` tracks audit trail
- UNIQUE constraint prevents duplicate assignments

### Task 1.5: Implement task_execution_failures Table

**Purpose:** Track task failures per manager to prevent infinite retry loops.

**Full CREATE TABLE statement:**

```sql
CREATE TABLE task_execution_failures (
    id BIGSERIAL PRIMARY KEY,
    task_id BIGINT NOT NULL,
    task_uuid UUID NOT NULL,
    task_suite_id BIGINT REFERENCES task_suites(id) ON DELETE CASCADE,
    manager_id BIGINT NOT NULL REFERENCES node_managers(id) ON DELETE CASCADE,

    -- Failure tracking
    failure_count INTEGER NOT NULL DEFAULT 1,
    last_failure_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    error_messages TEXT[],  -- History of error messages
    worker_local_id INTEGER,  -- Which worker failed

    -- Metadata
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    UNIQUE(task_uuid, manager_id)
);

CREATE INDEX idx_task_failures_task_uuid ON task_execution_failures(task_uuid);
CREATE INDEX idx_task_failures_manager ON task_execution_failures(manager_id);
CREATE INDEX idx_task_failures_suite ON task_execution_failures(task_suite_id);
```

**Key Design Notes:**
- Tracks failures per (task, manager) pair
- `failure_count` increments on each failure
- `error_messages` is an array to preserve failure history
- `worker_local_id` identifies which worker within the manager failed
- UNIQUE constraint on (task_uuid, manager_id) ensures one row per combination

### Task 1.6: Alter active_tasks Table

**Purpose:** Link tasks to suites for managed task execution.

**Full ALTER TABLE statement:**

```sql
ALTER TABLE active_tasks
ADD COLUMN task_suite_id BIGINT REFERENCES task_suites(id) ON DELETE SET NULL;

CREATE INDEX idx_active_tasks_suite ON active_tasks(task_suite_id)
    WHERE task_suite_id IS NOT NULL;
```

**Behavior:**
- If `task_suite_id` is NULL: Independent task (current behavior, unchanged)
- If `task_suite_id` is set: Managed task (part of suite)
- ON DELETE SET NULL: If suite is deleted, task becomes independent
- Partial index only on non-NULL values for efficiency

### Task 1.7: Create Database Triggers

#### 1.7.1: Auto-Update Suite Task Counts

**Purpose:** Automatically maintain `total_tasks` and `pending_tasks` counters on `task_suites` table.

**Complete trigger implementation:**

```sql
CREATE OR REPLACE FUNCTION update_suite_task_counts()
RETURNS TRIGGER AS $$
BEGIN
    IF TG_OP = 'INSERT' AND NEW.task_suite_id IS NOT NULL THEN
        -- New task added to suite
        UPDATE task_suites
        SET total_tasks = total_tasks + 1,
            pending_tasks = pending_tasks + 1,
            last_task_submitted_at = NOW(),
            updated_at = NOW()
        WHERE id = NEW.task_suite_id;

    ELSIF TG_OP = 'UPDATE' AND NEW.task_suite_id IS NOT NULL THEN
        IF OLD.state != NEW.state AND NEW.state IN (3, 4) THEN
            -- Task finished (Finished=3) or cancelled (Cancelled=4)
            UPDATE task_suites
            SET pending_tasks = GREATEST(pending_tasks - 1, 0),
                updated_at = NOW()
            WHERE id = NEW.task_suite_id;
        END IF;

    ELSIF TG_OP = 'DELETE' AND OLD.task_suite_id IS NOT NULL THEN
        -- Task deleted (rare)
        UPDATE task_suites
        SET pending_tasks = GREATEST(pending_tasks - 1, 0),
            updated_at = NOW()
        WHERE id = OLD.task_suite_id;
    END IF;

    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER task_suite_count_trigger
AFTER INSERT OR UPDATE OR DELETE ON active_tasks
FOR EACH ROW
EXECUTE FUNCTION update_suite_task_counts();
```

**Trigger Behavior:**
- **INSERT**: Increments both `total_tasks` and `pending_tasks`, updates `last_task_submitted_at`
- **UPDATE**: When task state changes to Finished(3) or Cancelled(4), decrements `pending_tasks`
- **DELETE**: Decrements `pending_tasks` (rare case)
- Uses `GREATEST(pending_tasks - 1, 0)` to prevent negative counts

**Critical Note:** This trigger is AFTER trigger, so it fires after the row is inserted/updated/deleted in `active_tasks`.

#### 1.7.2: Auto-Transition Suite States

**Purpose:** Background function to transition suite states based on inactivity timeout and task completion.

**Complete function implementation:**

```sql
CREATE OR REPLACE FUNCTION auto_transition_suite_states()
RETURNS void AS $$
BEGIN
    -- Transition OPEN → CLOSED (3-minute inactivity timeout)
    UPDATE task_suites
    SET state = 1,  -- Closed
        updated_at = NOW()
    WHERE state = 0  -- Open
      AND last_task_submitted_at IS NOT NULL
      AND EXTRACT(EPOCH FROM (NOW() - last_task_submitted_at)) > 180  -- Fixed 3-minute timeout
      AND pending_tasks > 0;

    -- Transition OPEN/CLOSED → COMPLETE (all tasks finished)
    UPDATE task_suites
    SET state = 2,  -- Complete
        updated_at = NOW(),
        completed_at = NOW()
    WHERE state IN (0, 1)  -- Open or Closed
      AND pending_tasks = 0;
END;
$$ LANGUAGE plpgsql;
```

**Function Behavior:**
- **OPEN → CLOSED**: After 3 minutes (180 seconds) of no new task submissions, if pending tasks remain
- **OPEN/CLOSED → COMPLETE**: When all pending tasks are finished (`pending_tasks = 0`)

**Scheduling:** This function should be called by:
- Option 1: pg_cron extension (if available): `SELECT cron.schedule('suite-state-transitions', '*/30 * * * * *', 'SELECT auto_transition_suite_states()');`
- Option 2: Coordinator background task (every 30 seconds)

**Note:** This is a stored procedure, not a trigger. It must be invoked by an external scheduler.

### Task 1.8: Create SeaORM Entity Models

Create SeaORM entity models for all new tables. These should be generated using SeaORM's entity generator and then manually refined.

#### 1.8.1: task_suites.rs Entity

**File:** `entity/src/task_suites.rs`

**Basic structure:**

```rust
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "task_suites")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,

    #[sea_orm(unique)]
    pub uuid: Uuid,

    pub name: Option<String>,
    pub description: Option<String>,

    pub group_id: i64,
    pub creator_id: i64,

    pub tags: Vec<String>,
    pub labels: Json,  // JSONB in database
    pub priority: i32,

    pub worker_schedule: Json,  // JSONB in database
    pub env_preparation: Option<Json>,
    pub env_cleanup: Option<Json>,

    pub state: i32,
    pub last_task_submitted_at: Option<DateTimeWithTimeZone>,
    pub total_tasks: i32,
    pub pending_tasks: i32,

    pub created_at: DateTimeWithTimeZone,
    pub updated_at: DateTimeWithTimeZone,
    pub completed_at: Option<DateTimeWithTimeZone>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::groups::Entity",
        from = "Column::GroupId",
        to = "super::groups::Column::Id"
    )]
    Group,

    #[sea_orm(
        belongs_to = "super::users::Entity",
        from = "Column::CreatorId",
        to = "super::users::Column::Id"
    )]
    Creator,

    #[sea_orm(has_many = "super::active_tasks::Entity")]
    ActiveTasks,

    #[sea_orm(has_many = "super::task_suite_managers::Entity")]
    TaskSuiteManagers,

    #[sea_orm(has_many = "super::task_execution_failures::Entity")]
    TaskExecutionFailures,
}

impl ActiveModelBehavior for ActiveModel {}
```

#### 1.8.2: node_managers.rs Entity

**File:** `entity/src/node_managers.rs`

**Basic structure:**

```rust
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "node_managers")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,

    #[sea_orm(unique)]
    pub uuid: Uuid,

    pub creator_id: i64,

    pub tags: Vec<String>,
    pub labels: Json,

    pub state: i32,
    pub last_heartbeat: DateTimeWithTimeZone,

    pub assigned_task_suite_id: Option<i64>,
    pub lease_expires_at: Option<DateTimeWithTimeZone>,

    pub created_at: DateTimeWithTimeZone,
    pub updated_at: DateTimeWithTimeZone,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::users::Entity",
        from = "Column::CreatorId",
        to = "super::users::Column::Id"
    )]
    Creator,

    #[sea_orm(
        belongs_to = "super::task_suites::Entity",
        from = "Column::AssignedTaskSuiteId",
        to = "super::task_suites::Column::Id"
    )]
    AssignedTaskSuite,

    #[sea_orm(has_many = "super::group_node_manager::Entity")]
    GroupNodeManagers,

    #[sea_orm(has_many = "super::task_suite_managers::Entity")]
    TaskSuiteManagers,

    #[sea_orm(has_many = "super::task_execution_failures::Entity")]
    TaskExecutionFailures,
}

impl ActiveModelBehavior for ActiveModel {}
```

#### 1.8.3: group_node_manager.rs Entity

**File:** `entity/src/group_node_manager.rs`

**Basic structure:**

```rust
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "group_node_manager")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,

    pub group_id: i64,
    pub manager_id: i64,
    pub role: i32,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::groups::Entity",
        from = "Column::GroupId",
        to = "super::groups::Column::Id"
    )]
    Group,

    #[sea_orm(
        belongs_to = "super::node_managers::Entity",
        from = "Column::ManagerId",
        to = "super::node_managers::Column::Id"
    )]
    Manager,
}

impl ActiveModelBehavior for ActiveModel {}
```

#### 1.8.4: task_suite_managers.rs Entity

**File:** `entity/src/task_suite_managers.rs`

**Basic structure:**

```rust
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "task_suite_managers")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,

    pub task_suite_id: i64,
    pub manager_id: i64,

    pub selection_type: i32,
    pub matched_tags: Option<Vec<String>>,

    pub added_at: DateTimeWithTimeZone,
    pub added_by_user_id: Option<i64>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::task_suites::Entity",
        from = "Column::TaskSuiteId",
        to = "super::task_suites::Column::Id"
    )]
    TaskSuite,

    #[sea_orm(
        belongs_to = "super::node_managers::Entity",
        from = "Column::ManagerId",
        to = "super::node_managers::Column::Id"
    )]
    Manager,

    #[sea_orm(
        belongs_to = "super::users::Entity",
        from = "Column::AddedByUserId",
        to = "super::users::Column::Id"
    )]
    AddedByUser,
}

impl ActiveModelBehavior for ActiveModel {}
```

#### 1.8.5: task_execution_failures.rs Entity

**File:** `entity/src/task_execution_failures.rs`

**Basic structure:**

```rust
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "task_execution_failures")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,

    pub task_id: i64,
    pub task_uuid: Uuid,
    pub task_suite_id: Option<i64>,
    pub manager_id: i64,

    pub failure_count: i32,
    pub last_failure_at: DateTimeWithTimeZone,
    pub error_messages: Vec<String>,
    pub worker_local_id: Option<i32>,

    pub created_at: DateTimeWithTimeZone,
    pub updated_at: DateTimeWithTimeZone,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::task_suites::Entity",
        from = "Column::TaskSuiteId",
        to = "super::task_suites::Column::Id"
    )]
    TaskSuite,

    #[sea_orm(
        belongs_to = "super::node_managers::Entity",
        from = "Column::ManagerId",
        to = "super::node_managers::Column::Id"
    )]
    Manager,
}

impl ActiveModelBehavior for ActiveModel {}
```

#### 1.8.6: Update active_tasks.rs Entity

Add the new `task_suite_id` field to the existing `active_tasks` entity:

```rust
// In entity/src/active_tasks.rs
// Add to Model struct:
pub task_suite_id: Option<i64>,

// Add to Relation enum:
#[sea_orm(
    belongs_to = "super::task_suites::Entity",
    from = "Column::TaskSuiteId",
    to = "super::task_suites::Column::Id"
)]
TaskSuite,
```

### Task 1.9: Add Enums

Create enum types for state management and selection types.

#### 1.9.1: TaskSuiteState Enum

**File:** `common/src/task_suite_state.rs` (or appropriate location)

```rust
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(i32)]
pub enum TaskSuiteState {
    Open = 0,
    Closed = 1,
    Complete = 2,
    Cancelled = 3,
}

impl TaskSuiteState {
    pub fn from_i32(value: i32) -> Option<Self> {
        match value {
            0 => Some(Self::Open),
            1 => Some(Self::Closed),
            2 => Some(Self::Complete),
            3 => Some(Self::Cancelled),
            _ => None,
        }
    }

    pub fn to_i32(self) -> i32 {
        self as i32
    }
}

impl fmt::Display for TaskSuiteState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Open => write!(f, "Open"),
            Self::Closed => write!(f, "Closed"),
            Self::Complete => write!(f, "Complete"),
            Self::Cancelled => write!(f, "Cancelled"),
        }
    }
}
```

**State Transitions:**
```
Create Suite → Open (on first task submission)

Open → Closed (3-minute inactivity timeout, no new tasks)
Open → Complete (all tasks finished)
Open → Cancelled (user cancellation)

Closed → Open (new task submitted)
Closed → Complete (all pending tasks finished)
Closed → Cancelled (user cancellation)

Complete → Open (new task submitted, suite reopens)
Complete → Cancelled (user cancellation)

Cancelled → (terminal state)
```

#### 1.9.2: NodeManagerState Enum

**File:** `common/src/node_manager_state.rs`

```rust
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(i32)]
pub enum NodeManagerState {
    Idle = 0,
    Preparing = 1,
    Executing = 2,
    Cleanup = 3,
    Offline = 4,
}

impl NodeManagerState {
    pub fn from_i32(value: i32) -> Option<Self> {
        match value {
            0 => Some(Self::Idle),
            1 => Some(Self::Preparing),
            2 => Some(Self::Executing),
            3 => Some(Self::Cleanup),
            4 => Some(Self::Offline),
            _ => None,
        }
    }

    pub fn to_i32(self) -> i32 {
        self as i32
    }
}

impl fmt::Display for NodeManagerState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Idle => write!(f, "Idle"),
            Self::Preparing => write!(f, "Preparing"),
            Self::Executing => write!(f, "Executing"),
            Self::Cleanup => write!(f, "Cleanup"),
            Self::Offline => write!(f, "Offline"),
        }
    }
}
```

#### 1.9.3: SelectionType Enum

**File:** `common/src/selection_type.rs`

```rust
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(i32)]
pub enum SelectionType {
    UserSpecified = 0,
    TagMatched = 1,
}

impl SelectionType {
    pub fn from_i32(value: i32) -> Option<Self> {
        match value {
            0 => Some(Self::UserSpecified),
            1 => Some(Self::TagMatched),
            _ => None,
        }
    }

    pub fn to_i32(self) -> i32 {
        self as i32
    }
}

impl fmt::Display for SelectionType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UserSpecified => write!(f, "UserSpecified"),
            Self::TagMatched => write!(f, "TagMatched"),
        }
    }
}
```

#### 1.9.4: GroupNodeManagerRole Enum

**File:** `common/src/group_node_manager_role.rs`

```rust
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(i32)]
pub enum GroupNodeManagerRole {
    Read = 0,   // Reserved for future use (view manager status)
    Write = 1,  // Group can submit task suites to manager
    Admin = 2,  // Group can manage manager ACL and settings
}

impl GroupNodeManagerRole {
    pub fn from_i32(value: i32) -> Option<Self> {
        match value {
            0 => Some(Self::Read),
            1 => Some(Self::Write),
            2 => Some(Self::Admin),
            _ => None,
        }
    }

    pub fn to_i32(self) -> i32 {
        self as i32
    }
}

impl fmt::Display for GroupNodeManagerRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read => write!(f, "Read"),
            Self::Write => write!(f, "Write"),
            Self::Admin => write!(f, "Admin"),
        }
    }
}
```

## Testing Checklist

### Database Schema Tests

- [ ] Migration runs successfully without errors
- [ ] Migration can be rolled back (down migration works)
- [ ] All tables created with correct schema
  - [ ] `task_suites` table exists with all columns
  - [ ] `node_managers` table exists with all columns
  - [ ] `group_node_manager` table exists with all columns
  - [ ] `task_suite_managers` table exists with all columns
  - [ ] `task_execution_failures` table exists with all columns
  - [ ] `active_tasks` table updated with `task_suite_id` column

### Index Tests

- [ ] All indexes created successfully
- [ ] GIN indexes on tags and labels work correctly
- [ ] Partial indexes created with WHERE clauses
- [ ] Query performance validated (EXPLAIN ANALYZE)

### Constraint Tests

- [ ] UNIQUE constraints enforced
  - [ ] `task_suites.uuid` is unique
  - [ ] `node_managers.uuid` is unique
  - [ ] `group_node_manager(group_id, manager_id)` is unique
  - [ ] `task_suite_managers(task_suite_id, manager_id)` is unique
  - [ ] `task_execution_failures(task_uuid, manager_id)` is unique

- [ ] Foreign key constraints enforced
  - [ ] Cannot insert task_suite with invalid group_id
  - [ ] Cannot insert node_manager with invalid creator_id
  - [ ] Cannot insert group_node_manager with invalid group_id or manager_id
  - [ ] ON DELETE CASCADE works for join tables
  - [ ] ON DELETE SET NULL works for `active_tasks.task_suite_id`
  - [ ] ON DELETE RESTRICT prevents deletion of referenced rows

- [ ] Default values set correctly
  - [ ] `task_suites.state` defaults to 0 (Open)
  - [ ] `task_suites.total_tasks` defaults to 0
  - [ ] `task_suites.pending_tasks` defaults to 0
  - [ ] `node_managers.state` defaults to 0 (Idle)
  - [ ] Timestamp fields use NOW() default

### Trigger Tests

- [ ] `update_suite_task_counts()` trigger works correctly
  - [ ] INSERT task with suite_id increments total_tasks and pending_tasks
  - [ ] INSERT task with suite_id updates last_task_submitted_at
  - [ ] UPDATE task state to Finished(3) decrements pending_tasks
  - [ ] UPDATE task state to Cancelled(4) decrements pending_tasks
  - [ ] DELETE task with suite_id decrements pending_tasks
  - [ ] Counters never go negative (GREATEST check)

- [ ] `auto_transition_suite_states()` function works correctly
  - [ ] Open suite transitions to Closed after 3 minutes of inactivity
  - [ ] Open suite transitions to Complete when pending_tasks = 0
  - [ ] Closed suite transitions to Complete when pending_tasks = 0
  - [ ] Function can be called manually: `SELECT auto_transition_suite_states();`

### SeaORM Entity Tests

- [ ] All entity models compile without errors
- [ ] Entity relationships defined correctly
  - [ ] task_suites belongs_to groups
  - [ ] task_suites belongs_to users (creator)
  - [ ] task_suites has_many active_tasks
  - [ ] node_managers belongs_to users (creator)
  - [ ] node_managers belongs_to task_suites (assigned)
  - [ ] Join table relations work correctly

- [ ] CRUD operations work
  - [ ] Can insert new task_suite
  - [ ] Can query task_suite by uuid
  - [ ] Can update task_suite state
  - [ ] Can delete task_suite (cascades to join tables)

- [ ] JSON/JSONB fields serialize/deserialize correctly
  - [ ] `worker_schedule` JSONB field
  - [ ] `env_preparation` JSONB field
  - [ ] `env_cleanup` JSONB field
  - [ ] `labels` JSONB field

### Enum Tests

- [ ] All enums compile and work correctly
  - [ ] TaskSuiteState enum (Open=0, Closed=1, Complete=2, Cancelled=3)
  - [ ] NodeManagerState enum (Idle=0, Preparing=1, Executing=2, Cleanup=3, Offline=4)
  - [ ] SelectionType enum (UserSpecified=0, TagMatched=1)
  - [ ] GroupNodeManagerRole enum (Read=0, Write=1, Admin=2)

- [ ] Enum conversion functions work
  - [ ] `from_i32()` handles all valid values
  - [ ] `from_i32()` returns None for invalid values
  - [ ] `to_i32()` returns correct integer value
  - [ ] `Display` trait formats correctly

### Integration Tests

- [ ] End-to-end scenario: Create suite → Submit task → Task finishes
  - [ ] Suite counters update correctly
  - [ ] Suite state transitions correctly
  - [ ] Task-suite relationship maintained

- [ ] Tag matching queries work
  - [ ] Find managers with tags ⊇ suite.tags
  - [ ] GIN index used (check EXPLAIN)

- [ ] Permission checks
  - [ ] group_node_manager role enforcement
  - [ ] Foreign key cascades work correctly

## Success Criteria

Phase 1 is considered complete when:

1. **All Migrations Pass**
   - Migration runs successfully on clean database
   - Migration can be rolled back without errors
   - Database schema matches RFC specification exactly

2. **All Tables Created**
   - 5 new tables created: `task_suites`, `node_managers`, `group_node_manager`, `task_suite_managers`, `task_execution_failures`
   - 1 table altered: `active_tasks` (added `task_suite_id` column)
   - All indexes created and optimized

3. **Triggers Work Correctly**
   - `update_suite_task_counts()` trigger fires on INSERT/UPDATE/DELETE
   - `auto_transition_suite_states()` function can be called and transitions states
   - Manual testing confirms counters are accurate

4. **SeaORM Entities Generated**
   - 5 new entity files created and compile successfully
   - All relationships defined correctly
   - CRUD operations work for all entities

5. **Enums Defined**
   - 4 enum types created and compile successfully
   - Conversion functions work correctly
   - Can be used in API schema (Phase 2)

6. **All Tests Pass**
   - Unit tests for database operations
   - Integration tests for triggers and constraints
   - Performance tests for indexes (EXPLAIN ANALYZE)

7. **Documentation Complete**
   - Migration files well-commented
   - Entity models documented
   - Testing results recorded

## Dependencies

**None** - This is Phase 1 (foundation phase).

Phase 1 must be completed before starting:
- Phase 2: API Schema and Coordinator Endpoints
- Phase 3: WebSocket Manager
- All subsequent phases

## Next Phase

**Phase 2: API Schema and Coordinator Endpoints** (1 week)

Once the database schema is in place, Phase 2 will:
- Define Rust types in `schema.rs` for API requests/responses
- Implement suite management APIs (POST/GET/DELETE /suites)
- Implement manager APIs (POST/GET /managers)
- Update task submission API to accept `suite_uuid` parameter

---

## Additional Notes

### Best Practices

1. **Test Incrementally**: Don't wait until all tables are created. Test each table as you go.
2. **Use Transactions**: All migration operations should be in a single transaction for atomicity.
3. **Backup First**: Always backup database before running migrations.
4. **Check Constraints**: Use `\d+ table_name` in psql to verify schema matches RFC.

### Common Pitfalls

1. **Array vs JSONB**: PostgreSQL arrays (TEXT[]) are not the same as JSONB arrays. Tags use TEXT[], labels use JSONB.
2. **Trigger Ordering**: AFTER triggers fire after the operation completes. Ensure logic accounts for this.
3. **NULL Handling**: Use `GREATEST(pending_tasks - 1, 0)` to prevent negative counts.
4. **Index Types**: Use GIN indexes for array and JSONB columns, B-tree for scalar columns.

### Debugging Tips

1. **Check Trigger Execution**: `SELECT * FROM pg_trigger WHERE tgname = 'task_suite_count_trigger';`
2. **Verify Index Usage**: `EXPLAIN ANALYZE SELECT * FROM task_suites WHERE tags @> ARRAY['gpu'];`
3. **Monitor Counter Accuracy**: Periodically verify `total_tasks` and `pending_tasks` match actual counts
4. **Foreign Key Violations**: Use `\d+ table_name` to see all constraints and their names

### Performance Considerations

- GIN indexes on `tags` and `labels` support fast containment queries (@>)
- Partial indexes reduce index size and improve write performance
- Auto-close index only on Open suites (WHERE state = 0) for efficiency
- Consider partitioning `task_execution_failures` table if it grows large

---

**End of Phase 1 Implementation Guide**
