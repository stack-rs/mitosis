# Phase 1: Database Schema Implementation Guide

## Overview

This is the foundation phase for the Node Manager and Task Suite System. In this phase, we establish the database schema for all new tables, relationships, and SeaORM entity models. We also implement coordinator-based state management logic to replace traditional database triggers.

**Implementation Philosophy:**

This phase is broken down into **small, self-contained pieces** where each piece is:
- Independently runnable and testable
- Self-complete (can compile and run on its own)
- Doesn't need to fully implement the whole phase
- Can be reviewed and modified individually
- When all pieces complete, the phase is fully implemented

**Key Deliverables:**
- 5 new database tables with proper indexes
- 1 ALTER statement to extend `active_tasks`
- 5 new SeaORM entity models
- 4 new enum types
- Coordinator code for suite task count updates (replaces database triggers)
- Coordinator background job for suite state transitions

**No Database Triggers:** We use coordinator processing code instead of database triggers for better control, testability, and debugging.

## Prerequisites

- PostgreSQL database (version 12+)
- SeaORM migration tools installed
- Familiarity with SeaORM entity model generation
- Access to coordinator codebase for implementing processing logic

## Timeline

**Duration:** 1-2 weeks

**Breakdown:**
- Pieces 1.1-1.6: Database tables and migrations (3-5 days)
- Piece 1.7: State enums (1 day)
- Piece 1.8: Task count update logic (1-2 days)
- Piece 1.9: State transition background job (1-2 days)
- Testing and validation (2-3 days)

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

### Section 6.3: State Management

Instead of database triggers, we implement state management in the coordinator:
1. **Task count updates** - Increment/decrement suite counters on task submission/completion
2. **State transitions** - Background job to transition suite states (Open/Closed/Complete)

---

## Implementation Pieces

### Piece 1.1: Create task_suites Table Migration

**Goal:** Create migration file with just the CREATE TABLE statement for task_suites.

**File to create:** `migration/src/mXXXX_create_task_suites.rs`

**Migration code:**

```rust
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(TaskSuites::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(TaskSuites::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(TaskSuites::Uuid)
                            .uuid()
                            .not_null()
                            .unique_key(),
                    )
                    .col(ColumnDef::new(TaskSuites::Name).text())
                    .col(ColumnDef::new(TaskSuites::Description).text())
                    .col(ColumnDef::new(TaskSuites::GroupId).big_integer().not_null())
                    .col(ColumnDef::new(TaskSuites::CreatorId).big_integer().not_null())
                    .col(
                        ColumnDef::new(TaskSuites::Tags)
                            .array(ColumnType::Text)
                            .not_null()
                            .default(Expr::val("{}"))
                    )
                    .col(
                        ColumnDef::new(TaskSuites::Labels)
                            .json_binary()
                            .not_null()
                            .default(Expr::val("[]"))
                    )
                    .col(
                        ColumnDef::new(TaskSuites::Priority)
                            .integer()
                            .not_null()
                            .default(0)
                    )
                    .col(ColumnDef::new(TaskSuites::WorkerSchedule).json_binary().not_null())
                    .col(ColumnDef::new(TaskSuites::EnvPreparation).json_binary())
                    .col(ColumnDef::new(TaskSuites::EnvCleanup).json_binary())
                    .col(
                        ColumnDef::new(TaskSuites::State)
                            .integer()
                            .not_null()
                            .default(0)
                    )
                    .col(ColumnDef::new(TaskSuites::LastTaskSubmittedAt).timestamp_with_time_zone())
                    .col(
                        ColumnDef::new(TaskSuites::TotalTasks)
                            .integer()
                            .not_null()
                            .default(0)
                    )
                    .col(
                        ColumnDef::new(TaskSuites::PendingTasks)
                            .integer()
                            .not_null()
                            .default(0)
                    )
                    .col(
                        ColumnDef::new(TaskSuites::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp())
                    )
                    .col(
                        ColumnDef::new(TaskSuites::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp())
                    )
                    .col(ColumnDef::new(TaskSuites::CompletedAt).timestamp_with_time_zone())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_task_suites_group")
                            .from(TaskSuites::Table, TaskSuites::GroupId)
                            .to(Groups::Table, Groups::Id)
                            .on_delete(ForeignKeyAction::Restrict)
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_task_suites_creator")
                            .from(TaskSuites::Table, TaskSuites::CreatorId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Restrict)
                    )
                    .to_owned(),
            )
            .await?;

        // Create indexes
        manager
            .create_index(
                Index::create()
                    .name("idx_task_suites_group_id")
                    .table(TaskSuites::Table)
                    .col(TaskSuites::GroupId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_task_suites_creator_id")
                    .table(TaskSuites::Table)
                    .col(TaskSuites::CreatorId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_task_suites_state")
                    .table(TaskSuites::Table)
                    .col(TaskSuites::State)
                    .to_owned(),
            )
            .await?;

        // GIN indexes for array/JSONB columns (use raw SQL)
        manager
            .get_connection()
            .execute_unprepared("CREATE INDEX idx_task_suites_tags ON task_suites USING GIN(tags)")
            .await?;

        manager
            .get_connection()
            .execute_unprepared("CREATE INDEX idx_task_suites_labels ON task_suites USING GIN(labels)")
            .await?;

        // Partial index for auto-close timeout queries
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE INDEX idx_task_suites_auto_close ON task_suites(last_task_submitted_at) WHERE state = 0"
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(TaskSuites::Table).to_owned())
            .await
    }
}

#[derive(Iden)]
enum TaskSuites {
    Table,
    Id,
    Uuid,
    Name,
    Description,
    GroupId,
    CreatorId,
    Tags,
    Labels,
    Priority,
    WorkerSchedule,
    EnvPreparation,
    EnvCleanup,
    State,
    LastTaskSubmittedAt,
    TotalTasks,
    PendingTasks,
    CreatedAt,
    UpdatedAt,
    CompletedAt,
}

#[derive(Iden)]
enum Groups {
    Table,
    Id,
}

#[derive(Iden)]
enum Users {
    Table,
    Id,
}
```

**Deliverable Criteria:**
- ✅ Migration file compiles
- ✅ `sea-orm-cli migrate up` runs successfully
- ✅ Table exists in database: `\d+ task_suites`
- ✅ All indexes created (6 indexes total)
- ✅ Foreign keys enforce constraints

**Testing Instructions:**

```bash
# Run migration
sea-orm-cli migrate up

# Verify table exists
psql -d your_db -c "\d+ task_suites"

# Verify indexes
psql -d your_db -c "\di task_suites*"

# Test foreign key constraints
psql -d your_db -c "INSERT INTO task_suites (uuid, group_id, creator_id, worker_schedule) VALUES (gen_random_uuid(), 99999, 1, '{}');"
# Should fail with foreign key violation

# Test rollback
sea-orm-cli migrate down
sea-orm-cli migrate up
```

---

### Piece 1.2: Create TaskSuite SeaORM Entity

**Goal:** Generate or write entity model for task_suites with all field mappings.

**File to create:** `entity/src/task_suites.rs`

**Entity code:**

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

impl Related<super::groups::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Group.def()
    }
}

impl Related<super::users::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Creator.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
```

**Update `entity/src/lib.rs`:**

```rust
// Add to mod declarations
pub mod task_suites;
```

**Deliverable Criteria:**
- ✅ Entity file compiles
- ✅ Can import and use entity in code
- ✅ Basic CRUD operations work

**Testing Instructions:**

```rust
// Test in integration test or coordinator code
use entity::task_suites;
use sea_orm::*;

#[tokio::test]
async fn test_task_suite_crud() {
    let db = /* your db connection */;

    // Create
    let suite = task_suites::ActiveModel {
        uuid: Set(Uuid::new_v4()),
        group_id: Set(1),
        creator_id: Set(1),
        worker_schedule: Set(json!({"worker_count": 16})),
        tags: Set(vec!["test".to_string()]),
        labels: Set(json!(["label1"])),
        ..Default::default()
    };

    let inserted = suite.insert(&db).await.unwrap();
    assert!(inserted.id > 0);

    // Read
    let found = task_suites::Entity::find_by_id(inserted.id)
        .one(&db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.uuid, inserted.uuid);

    // Update
    let mut active: task_suites::ActiveModel = found.into();
    active.state = Set(1);
    let updated = active.update(&db).await.unwrap();
    assert_eq!(updated.state, 1);

    // Delete
    task_suites::Entity::delete_by_id(inserted.id)
        .exec(&db)
        .await
        .unwrap();
}
```

---

### Piece 1.3: Create node_managers Table and Entity

**Goal:** Create migration for node_managers table and corresponding SeaORM entity.

**Migration file:** `migration/src/mXXXX_create_node_managers.rs`

**Migration code:**

```rust
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(NodeManagers::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(NodeManagers::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(NodeManagers::Uuid)
                            .uuid()
                            .not_null()
                            .unique_key(),
                    )
                    .col(ColumnDef::new(NodeManagers::CreatorId).big_integer().not_null())
                    .col(
                        ColumnDef::new(NodeManagers::Tags)
                            .array(ColumnType::Text)
                            .not_null()
                            .default(Expr::val("{}"))
                    )
                    .col(
                        ColumnDef::new(NodeManagers::Labels)
                            .json_binary()
                            .not_null()
                            .default(Expr::val("[]"))
                    )
                    .col(
                        ColumnDef::new(NodeManagers::State)
                            .integer()
                            .not_null()
                            .default(0)
                    )
                    .col(
                        ColumnDef::new(NodeManagers::LastHeartbeat)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp())
                    )
                    .col(ColumnDef::new(NodeManagers::AssignedTaskSuiteId).big_integer())
                    .col(ColumnDef::new(NodeManagers::LeaseExpiresAt).timestamp_with_time_zone())
                    .col(
                        ColumnDef::new(NodeManagers::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp())
                    )
                    .col(
                        ColumnDef::new(NodeManagers::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp())
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_node_managers_creator")
                            .from(NodeManagers::Table, NodeManagers::CreatorId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Restrict)
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_node_managers_assigned_suite")
                            .from(NodeManagers::Table, NodeManagers::AssignedTaskSuiteId)
                            .to(TaskSuites::Table, TaskSuites::Id)
                            .on_delete(ForeignKeyAction::SetNull)
                    )
                    .to_owned(),
            )
            .await?;

        // Create indexes
        manager
            .create_index(
                Index::create()
                    .name("idx_node_managers_creator_id")
                    .table(NodeManagers::Table)
                    .col(NodeManagers::CreatorId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_node_managers_state")
                    .table(NodeManagers::Table)
                    .col(NodeManagers::State)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_node_managers_heartbeat")
                    .table(NodeManagers::Table)
                    .col(NodeManagers::LastHeartbeat)
                    .to_owned(),
            )
            .await?;

        // GIN indexes for tags and labels
        manager
            .get_connection()
            .execute_unprepared("CREATE INDEX idx_node_managers_tags ON node_managers USING GIN(tags)")
            .await?;

        manager
            .get_connection()
            .execute_unprepared("CREATE INDEX idx_node_managers_labels ON node_managers USING GIN(labels)")
            .await?;

        // Partial index for assigned managers
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE INDEX idx_node_managers_assigned_suite ON node_managers(assigned_task_suite_id) WHERE assigned_task_suite_id IS NOT NULL"
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(NodeManagers::Table).to_owned())
            .await
    }
}

#[derive(Iden)]
enum NodeManagers {
    Table,
    Id,
    Uuid,
    CreatorId,
    Tags,
    Labels,
    State,
    LastHeartbeat,
    AssignedTaskSuiteId,
    LeaseExpiresAt,
    CreatedAt,
    UpdatedAt,
}

#[derive(Iden)]
enum Users {
    Table,
    Id,
}

#[derive(Iden)]
enum TaskSuites {
    Table,
    Id,
}
```

**Entity file:** `entity/src/node_managers.rs`

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

impl Related<super::users::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Creator.def()
    }
}

impl Related<super::task_suites::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::AssignedTaskSuite.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
```

**Deliverable Criteria:**
- ✅ Migration compiles and runs
- ✅ Table created with all columns and indexes
- ✅ Entity compiles and basic CRUD works
- ✅ Can create/query managers in database

**Testing Instructions:**

```bash
# Run migration
sea-orm-cli migrate up

# Verify table
psql -d your_db -c "\d+ node_managers"

# Test CRUD
cargo test test_node_manager_crud
```

---

### Piece 1.4: Create Join Tables (group_node_manager, task_suite_managers)

**Goal:** Create migrations and entities for both join tables with proper relations.

**Migration file:** `migration/src/mXXXX_create_join_tables.rs`

**Migration code:**

```rust
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Create group_node_manager table
        manager
            .create_table(
                Table::create()
                    .table(GroupNodeManager::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(GroupNodeManager::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(GroupNodeManager::GroupId).big_integer().not_null())
                    .col(ColumnDef::new(GroupNodeManager::ManagerId).big_integer().not_null())
                    .col(ColumnDef::new(GroupNodeManager::Role).integer().not_null())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_group_node_manager_group")
                            .from(GroupNodeManager::Table, GroupNodeManager::GroupId)
                            .to(Groups::Table, Groups::Id)
                            .on_delete(ForeignKeyAction::Restrict)
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_group_node_manager_manager")
                            .from(GroupNodeManager::Table, GroupNodeManager::ManagerId)
                            .to(NodeManagers::Table, NodeManagers::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                    )
                    .to_owned(),
            )
            .await?;

        // Unique constraint on (group_id, manager_id)
        manager
            .create_index(
                Index::create()
                    .name("idx_group_node_manager_unique")
                    .table(GroupNodeManager::Table)
                    .col(GroupNodeManager::GroupId)
                    .col(GroupNodeManager::ManagerId)
                    .unique()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_group_node_manager_group")
                    .table(GroupNodeManager::Table)
                    .col(GroupNodeManager::GroupId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_group_node_manager_manager")
                    .table(GroupNodeManager::Table)
                    .col(GroupNodeManager::ManagerId)
                    .to_owned(),
            )
            .await?;

        // Create task_suite_managers table
        manager
            .create_table(
                Table::create()
                    .table(TaskSuiteManagers::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(TaskSuiteManagers::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(TaskSuiteManagers::TaskSuiteId).big_integer().not_null())
                    .col(ColumnDef::new(TaskSuiteManagers::ManagerId).big_integer().not_null())
                    .col(ColumnDef::new(TaskSuiteManagers::SelectionType).integer().not_null())
                    .col(ColumnDef::new(TaskSuiteManagers::MatchedTags).array(ColumnType::Text))
                    .col(
                        ColumnDef::new(TaskSuiteManagers::AddedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp())
                    )
                    .col(ColumnDef::new(TaskSuiteManagers::AddedByUserId).big_integer())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_task_suite_managers_suite")
                            .from(TaskSuiteManagers::Table, TaskSuiteManagers::TaskSuiteId)
                            .to(TaskSuites::Table, TaskSuites::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_task_suite_managers_manager")
                            .from(TaskSuiteManagers::Table, TaskSuiteManagers::ManagerId)
                            .to(NodeManagers::Table, NodeManagers::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_task_suite_managers_user")
                            .from(TaskSuiteManagers::Table, TaskSuiteManagers::AddedByUserId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::SetNull)
                    )
                    .to_owned(),
            )
            .await?;

        // Unique constraint on (task_suite_id, manager_id)
        manager
            .create_index(
                Index::create()
                    .name("idx_task_suite_managers_unique")
                    .table(TaskSuiteManagers::Table)
                    .col(TaskSuiteManagers::TaskSuiteId)
                    .col(TaskSuiteManagers::ManagerId)
                    .unique()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_task_suite_managers_suite")
                    .table(TaskSuiteManagers::Table)
                    .col(TaskSuiteManagers::TaskSuiteId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_task_suite_managers_manager")
                    .table(TaskSuiteManagers::Table)
                    .col(TaskSuiteManagers::ManagerId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_task_suite_managers_selection_type")
                    .table(TaskSuiteManagers::Table)
                    .col(TaskSuiteManagers::SelectionType)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(TaskSuiteManagers::Table).to_owned())
            .await?;

        manager
            .drop_table(Table::drop().table(GroupNodeManager::Table).to_owned())
            .await
    }
}

#[derive(Iden)]
enum GroupNodeManager {
    Table,
    Id,
    GroupId,
    ManagerId,
    Role,
}

#[derive(Iden)]
enum TaskSuiteManagers {
    Table,
    Id,
    TaskSuiteId,
    ManagerId,
    SelectionType,
    MatchedTags,
    AddedAt,
    AddedByUserId,
}

#[derive(Iden)]
enum Groups {
    Table,
    Id,
}

#[derive(Iden)]
enum NodeManagers {
    Table,
    Id,
}

#[derive(Iden)]
enum TaskSuites {
    Table,
    Id,
}

#[derive(Iden)]
enum Users {
    Table,
    Id,
}
```

**Entity 1:** `entity/src/group_node_manager.rs`

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

impl Related<super::groups::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Group.def()
    }
}

impl Related<super::node_managers::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Manager.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
```

**Entity 2:** `entity/src/task_suite_managers.rs`

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

impl Related<super::task_suites::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::TaskSuite.def()
    }
}

impl Related<super::node_managers::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Manager.def()
    }
}

impl Related<super::users::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::AddedByUser.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
```

**Deliverable Criteria:**
- ✅ Both tables created with proper foreign keys
- ✅ Unique constraints enforced
- ✅ Can create associations between groups/managers and suites/managers
- ✅ CASCADE and RESTRICT behaviors work correctly

**Testing Instructions:**

```bash
# Run migration
sea-orm-cli migrate up

# Verify tables
psql -d your_db -c "\d+ group_node_manager"
psql -d your_db -c "\d+ task_suite_managers"

# Test unique constraint
# Should fail on duplicate (group_id, manager_id)
```

---

### Piece 1.5: Create task_execution_failures Table and Entity

**Goal:** Create migration and entity for tracking task failures per manager.

**Migration file:** `migration/src/mXXXX_create_task_execution_failures.rs`

**Migration code:**

```rust
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(TaskExecutionFailures::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(TaskExecutionFailures::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(TaskExecutionFailures::TaskId).big_integer().not_null())
                    .col(ColumnDef::new(TaskExecutionFailures::TaskUuid).uuid().not_null())
                    .col(ColumnDef::new(TaskExecutionFailures::TaskSuiteId).big_integer())
                    .col(ColumnDef::new(TaskExecutionFailures::ManagerId).big_integer().not_null())
                    .col(
                        ColumnDef::new(TaskExecutionFailures::FailureCount)
                            .integer()
                            .not_null()
                            .default(1)
                    )
                    .col(
                        ColumnDef::new(TaskExecutionFailures::LastFailureAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp())
                    )
                    .col(
                        ColumnDef::new(TaskExecutionFailures::ErrorMessages)
                            .array(ColumnType::Text)
                    )
                    .col(ColumnDef::new(TaskExecutionFailures::WorkerLocalId).integer())
                    .col(
                        ColumnDef::new(TaskExecutionFailures::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp())
                    )
                    .col(
                        ColumnDef::new(TaskExecutionFailures::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp())
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_task_failures_suite")
                            .from(TaskExecutionFailures::Table, TaskExecutionFailures::TaskSuiteId)
                            .to(TaskSuites::Table, TaskSuites::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_task_failures_manager")
                            .from(TaskExecutionFailures::Table, TaskExecutionFailures::ManagerId)
                            .to(NodeManagers::Table, NodeManagers::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                    )
                    .to_owned(),
            )
            .await?;

        // Unique constraint on (task_uuid, manager_id)
        manager
            .create_index(
                Index::create()
                    .name("idx_task_failures_unique")
                    .table(TaskExecutionFailures::Table)
                    .col(TaskExecutionFailures::TaskUuid)
                    .col(TaskExecutionFailures::ManagerId)
                    .unique()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_task_failures_task_uuid")
                    .table(TaskExecutionFailures::Table)
                    .col(TaskExecutionFailures::TaskUuid)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_task_failures_manager")
                    .table(TaskExecutionFailures::Table)
                    .col(TaskExecutionFailures::ManagerId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_task_failures_suite")
                    .table(TaskExecutionFailures::Table)
                    .col(TaskExecutionFailures::TaskSuiteId)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(TaskExecutionFailures::Table).to_owned())
            .await
    }
}

#[derive(Iden)]
enum TaskExecutionFailures {
    Table,
    Id,
    TaskId,
    TaskUuid,
    TaskSuiteId,
    ManagerId,
    FailureCount,
    LastFailureAt,
    ErrorMessages,
    WorkerLocalId,
    CreatedAt,
    UpdatedAt,
}

#[derive(Iden)]
enum TaskSuites {
    Table,
    Id,
}

#[derive(Iden)]
enum NodeManagers {
    Table,
    Id,
}
```

**Entity file:** `entity/src/task_execution_failures.rs`

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
    pub error_messages: Option<Vec<String>>,
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

impl Related<super::task_suites::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::TaskSuite.def()
    }
}

impl Related<super::node_managers::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Manager.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
```

**Deliverable Criteria:**
- ✅ Table created with unique constraint on (task_uuid, manager_id)
- ✅ Entity compiles and can record failures
- ✅ Can increment failure_count on repeated failures
- ✅ Error messages array persists history

**Testing Instructions:**

```bash
# Run migration
sea-orm-cli migrate up

# Verify table
psql -d your_db -c "\d+ task_execution_failures"

# Test failure tracking
cargo test test_failure_tracking
```

---

### Piece 1.6: Add task_suite_id to active_tasks

**Goal:** Add task_suite_id foreign key to active_tasks table and update entity.

**Migration file:** `migration/src/mXXXX_alter_active_tasks.rs`

**Migration code:**

```rust
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Add task_suite_id column
        manager
            .alter_table(
                Table::alter()
                    .table(ActiveTasks::Table)
                    .add_column(ColumnDef::new(ActiveTasks::TaskSuiteId).big_integer())
                    .to_owned(),
            )
            .await?;

        // Add foreign key
        manager
            .alter_table(
                Table::alter()
                    .table(ActiveTasks::Table)
                    .add_foreign_key(
                        TableForeignKey::new()
                            .name("fk_active_tasks_suite")
                            .from_tbl(ActiveTasks::Table)
                            .from_col(ActiveTasks::TaskSuiteId)
                            .to_tbl(TaskSuites::Table)
                            .to_col(TaskSuites::Id)
                            .on_delete(ForeignKeyAction::SetNull)
                    )
                    .to_owned(),
            )
            .await?;

        // Create partial index on task_suite_id (only non-NULL values)
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE INDEX idx_active_tasks_suite ON active_tasks(task_suite_id) WHERE task_suite_id IS NOT NULL"
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Drop index first
        manager
            .drop_index(
                Index::drop()
                    .name("idx_active_tasks_suite")
                    .table(ActiveTasks::Table)
                    .to_owned(),
            )
            .await?;

        // Drop foreign key
        manager
            .alter_table(
                Table::alter()
                    .table(ActiveTasks::Table)
                    .drop_foreign_key(Alias::new("fk_active_tasks_suite"))
                    .to_owned(),
            )
            .await?;

        // Drop column
        manager
            .alter_table(
                Table::alter()
                    .table(ActiveTasks::Table)
                    .drop_column(ActiveTasks::TaskSuiteId)
                    .to_owned(),
            )
            .await
    }
}

#[derive(Iden)]
enum ActiveTasks {
    Table,
    TaskSuiteId,
}

#[derive(Iden)]
enum TaskSuites {
    Table,
    Id,
}
```

**Update entity:** `entity/src/active_tasks.rs`

```rust
// Add to Model struct:
pub task_suite_id: Option<i64>,

// Add to Relation enum:
#[sea_orm(
    belongs_to = "super::task_suites::Entity",
    from = "Column::TaskSuiteId",
    to = "super::task_suites::Column::Id"
)]
TaskSuite,

// Add Related implementation:
impl Related<super::task_suites::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::TaskSuite.def()
    }
}
```

**Deliverable Criteria:**
- ✅ Column added to active_tasks table
- ✅ Foreign key constraint works
- ✅ Partial index created
- ✅ Can link tasks to suites
- ✅ ON DELETE SET NULL behavior works (task becomes independent if suite deleted)

**Testing Instructions:**

```bash
# Run migration
sea-orm-cli migrate up

# Verify column exists
psql -d your_db -c "\d+ active_tasks"

# Test linking task to suite
# Test suite deletion (task_suite_id should become NULL)
```

---

### Piece 1.7: Add State Enums (TaskSuiteState, NodeManagerState)

**Goal:** Create enum types in code with to/from i32 conversions for database storage.

**File 1:** `common/src/task_suite_state.rs`

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

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Complete | Self::Cancelled)
    }

    pub fn can_accept_tasks(self) -> bool {
        matches!(self, Self::Open | Self::Closed)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_enum_conversion() {
        assert_eq!(TaskSuiteState::Open.to_i32(), 0);
        assert_eq!(TaskSuiteState::from_i32(0), Some(TaskSuiteState::Open));
        assert_eq!(TaskSuiteState::from_i32(99), None);
    }

    #[test]
    fn test_state_predicates() {
        assert!(TaskSuiteState::Complete.is_terminal());
        assert!(!TaskSuiteState::Open.is_terminal());
        assert!(TaskSuiteState::Open.can_accept_tasks());
        assert!(!TaskSuiteState::Complete.can_accept_tasks());
    }
}
```

**File 2:** `common/src/node_manager_state.rs`

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

    pub fn is_available(self) -> bool {
        matches!(self, Self::Idle)
    }

    pub fn is_busy(self) -> bool {
        matches!(self, Self::Preparing | Self::Executing | Self::Cleanup)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_enum_conversion() {
        assert_eq!(NodeManagerState::Idle.to_i32(), 0);
        assert_eq!(NodeManagerState::from_i32(0), Some(NodeManagerState::Idle));
        assert_eq!(NodeManagerState::from_i32(99), None);
    }

    #[test]
    fn test_state_predicates() {
        assert!(NodeManagerState::Idle.is_available());
        assert!(NodeManagerState::Executing.is_busy());
        assert!(!NodeManagerState::Offline.is_available());
    }
}
```

**File 3:** `common/src/selection_type.rs`

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

**File 4:** `common/src/group_node_manager_role.rs`

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

    pub fn has_write_access(self) -> bool {
        matches!(self, Self::Write | Self::Admin)
    }

    pub fn has_admin_access(self) -> bool {
        matches!(self, Self::Admin)
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

**Update `common/src/lib.rs`:**

```rust
pub mod task_suite_state;
pub mod node_manager_state;
pub mod selection_type;
pub mod group_node_manager_role;
```

**Deliverable Criteria:**
- ✅ All 4 enums compile
- ✅ Conversion functions (from_i32/to_i32) work correctly
- ✅ Display trait formats correctly
- ✅ Can be used with SeaORM entities
- ✅ All tests pass

**Testing Instructions:**

```bash
# Run tests
cargo test -p common

# Test in integration context
# Create suite with state=Open (0)
# Verify can convert between enum and i32
```

---

### Piece 1.8: Implement Coordinator Code to Update Suite Task Counts

**Goal:** Write Rust code in coordinator to increment/decrement suite task counters on task submission/completion. This replaces database triggers.

**File:** `coordinator/src/suite_task_counter.rs`

```rust
use entity::{active_tasks, task_suites};
use sea_orm::*;

/// Increments total_tasks and pending_tasks when a task is submitted to a suite
pub async fn on_task_submitted(
    db: &DatabaseConnection,
    task_suite_id: i64,
) -> Result<(), DbErr> {
    // Update counters atomically
    task_suites::Entity::update_many()
        .col_expr(
            task_suites::Column::TotalTasks,
            Expr::col(task_suites::Column::TotalTasks).add(1),
        )
        .col_expr(
            task_suites::Column::PendingTasks,
            Expr::col(task_suites::Column::PendingTasks).add(1),
        )
        .col_expr(
            task_suites::Column::LastTaskSubmittedAt,
            Expr::value(chrono::Utc::now()),
        )
        .col_expr(
            task_suites::Column::UpdatedAt,
            Expr::value(chrono::Utc::now()),
        )
        .filter(task_suites::Column::Id.eq(task_suite_id))
        .exec(db)
        .await?;

    Ok(())
}

/// Decrements pending_tasks when a task completes or is cancelled
pub async fn on_task_completed(
    db: &DatabaseConnection,
    task_suite_id: i64,
) -> Result<(), DbErr> {
    // Decrement pending_tasks (use GREATEST to prevent negative values)
    let result = db
        .execute_unprepared(&format!(
            "UPDATE task_suites
             SET pending_tasks = GREATEST(pending_tasks - 1, 0),
                 updated_at = NOW()
             WHERE id = {}",
            task_suite_id
        ))
        .await?;

    if result.rows_affected() == 0 {
        tracing::warn!(
            task_suite_id = task_suite_id,
            "Attempted to decrement pending_tasks for non-existent suite"
        );
    }

    Ok(())
}

/// Decrements pending_tasks when a task is deleted (rare case)
pub async fn on_task_deleted(
    db: &DatabaseConnection,
    task_suite_id: i64,
) -> Result<(), DbErr> {
    // Same as completion
    on_task_completed(db, task_suite_id).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::*;

    #[tokio::test]
    async fn test_task_counter_updates() {
        let db = /* your test db connection */;

        // Create a test suite
        let suite = task_suites::ActiveModel {
            uuid: Set(uuid::Uuid::new_v4()),
            group_id: Set(1),
            creator_id: Set(1),
            worker_schedule: Set(serde_json::json!({"worker_count": 4})),
            ..Default::default()
        };
        let suite = suite.insert(&db).await.unwrap();

        // Verify initial state
        assert_eq!(suite.total_tasks, 0);
        assert_eq!(suite.pending_tasks, 0);

        // Submit a task
        on_task_submitted(&db, suite.id).await.unwrap();

        // Verify counts incremented
        let suite = task_suites::Entity::find_by_id(suite.id)
            .one(&db)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(suite.total_tasks, 1);
        assert_eq!(suite.pending_tasks, 1);

        // Complete a task
        on_task_completed(&db, suite.id).await.unwrap();

        // Verify pending decremented
        let suite = task_suites::Entity::find_by_id(suite.id)
            .one(&db)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(suite.total_tasks, 1);
        assert_eq!(suite.pending_tasks, 0);

        // Attempt to decrement below zero (should stay at 0)
        on_task_completed(&db, suite.id).await.unwrap();
        let suite = task_suites::Entity::find_by_id(suite.id)
            .one(&db)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(suite.pending_tasks, 0); // Should not go negative
    }
}
```

**Integration points:**

You need to hook this into your existing task submission and completion handlers:

**In task submission handler:**

```rust
// After inserting task into active_tasks
if let Some(suite_id) = task_model.task_suite_id {
    suite_task_counter::on_task_submitted(&db, suite_id).await?;
}
```

**In task completion handler:**

```rust
// When task state transitions to Finished(3) or Cancelled(4)
if let Some(suite_id) = old_task.task_suite_id {
    if new_task.state == TaskState::Finished || new_task.state == TaskState::Cancelled {
        suite_task_counter::on_task_completed(&db, suite_id).await?;
    }
}
```

**In task deletion handler:**

```rust
// When deleting a task
if let Some(suite_id) = task.task_suite_id {
    suite_task_counter::on_task_deleted(&db, suite_id).await?;
}
```

**Deliverable Criteria:**
- ✅ Functions compile and integrate with coordinator
- ✅ Task counts update correctly on insert/update/delete
- ✅ Counts never go negative (GREATEST check)
- ✅ All tests pass
- ✅ Logging/tracing in place for debugging

**Testing Instructions:**

```bash
# Run unit tests
cargo test -p coordinator test_task_counter_updates

# Integration test:
# 1. Create suite
# 2. Submit 10 tasks with suite_id
# 3. Verify total_tasks=10, pending_tasks=10
# 4. Complete 5 tasks
# 5. Verify total_tasks=10, pending_tasks=5
# 6. Complete remaining 5
# 7. Verify total_tasks=10, pending_tasks=0
```

---

### Piece 1.9: Implement Coordinator Background Job for Suite State Transitions

**Goal:** Write periodic background task to check for suites that need state transitions (Open→Closed, any→Complete). Runs every 30 seconds.

**File:** `coordinator/src/suite_state_transitions.rs`

```rust
use chrono::{Duration, Utc};
use common::task_suite_state::TaskSuiteState;
use entity::task_suites;
use sea_orm::*;

/// Auto-transition suite states based on timeout and completion criteria
///
/// Transitions:
/// - Open → Closed: After 3 minutes of no new task submissions (if pending_tasks > 0)
/// - Open/Closed → Complete: When all tasks are finished (pending_tasks = 0)
pub async fn auto_transition_suite_states(db: &DatabaseConnection) -> Result<u64, DbErr> {
    let mut total_transitioned = 0;

    // Transition 1: Open → Closed (3-minute inactivity timeout)
    let three_minutes_ago = Utc::now() - Duration::minutes(3);
    let closed_count = task_suites::Entity::update_many()
        .col_expr(task_suites::Column::State, Expr::value(TaskSuiteState::Closed.to_i32()))
        .col_expr(task_suites::Column::UpdatedAt, Expr::value(Utc::now()))
        .filter(task_suites::Column::State.eq(TaskSuiteState::Open.to_i32()))
        .filter(task_suites::Column::LastTaskSubmittedAt.is_not_null())
        .filter(task_suites::Column::LastTaskSubmittedAt.lt(three_minutes_ago))
        .filter(task_suites::Column::PendingTasks.gt(0))
        .exec(db)
        .await?;

    total_transitioned += closed_count.rows_affected();

    if closed_count.rows_affected() > 0 {
        tracing::info!(
            count = closed_count.rows_affected(),
            "Transitioned suites from Open to Closed due to inactivity timeout"
        );
    }

    // Transition 2: Open/Closed → Complete (all tasks finished)
    let complete_count = task_suites::Entity::update_many()
        .col_expr(task_suites::Column::State, Expr::value(TaskSuiteState::Complete.to_i32()))
        .col_expr(task_suites::Column::UpdatedAt, Expr::value(Utc::now()))
        .col_expr(task_suites::Column::CompletedAt, Expr::value(Utc::now()))
        .filter(
            task_suites::Column::State
                .is_in([TaskSuiteState::Open.to_i32(), TaskSuiteState::Closed.to_i32()])
        )
        .filter(task_suites::Column::PendingTasks.eq(0))
        .exec(db)
        .await?;

    total_transitioned += complete_count.rows_affected();

    if complete_count.rows_affected() > 0 {
        tracing::info!(
            count = complete_count.rows_affected(),
            "Transitioned suites to Complete (all tasks finished)"
        );
    }

    Ok(total_transitioned)
}

/// Background task runner - runs state transitions every 30 seconds
pub async fn run_state_transition_loop(db: DatabaseConnection) {
    let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(30));

    loop {
        interval.tick().await;

        match auto_transition_suite_states(&db).await {
            Ok(count) => {
                if count > 0 {
                    tracing::debug!(
                        transitioned = count,
                        "Suite state transition check completed"
                    );
                }
            }
            Err(e) => {
                tracing::error!(
                    error = ?e,
                    "Failed to auto-transition suite states"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use sea_orm::*;

    #[tokio::test]
    async fn test_open_to_closed_transition() {
        let db = /* your test db connection */;

        // Create suite with old last_task_submitted_at
        let old_time = Utc::now() - Duration::minutes(5);
        let suite = task_suites::ActiveModel {
            uuid: Set(uuid::Uuid::new_v4()),
            group_id: Set(1),
            creator_id: Set(1),
            worker_schedule: Set(serde_json::json!({"worker_count": 4})),
            state: Set(TaskSuiteState::Open.to_i32()),
            last_task_submitted_at: Set(Some(old_time)),
            pending_tasks: Set(5), // Still has pending tasks
            ..Default::default()
        };
        let suite = suite.insert(&db).await.unwrap();

        // Run transition
        let count = auto_transition_suite_states(&db).await.unwrap();
        assert_eq!(count, 1);

        // Verify state changed to Closed
        let suite = task_suites::Entity::find_by_id(suite.id)
            .one(&db)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(suite.state, TaskSuiteState::Closed.to_i32());
    }

    #[tokio::test]
    async fn test_to_complete_transition() {
        let db = /* your test db connection */;

        // Create suite with pending_tasks = 0
        let suite = task_suites::ActiveModel {
            uuid: Set(uuid::Uuid::new_v4()),
            group_id: Set(1),
            creator_id: Set(1),
            worker_schedule: Set(serde_json::json!({"worker_count": 4})),
            state: Set(TaskSuiteState::Closed.to_i32()),
            pending_tasks: Set(0), // All tasks finished
            total_tasks: Set(10),
            ..Default::default()
        };
        let suite = suite.insert(&db).await.unwrap();

        // Run transition
        let count = auto_transition_suite_states(&db).await.unwrap();
        assert_eq!(count, 1);

        // Verify state changed to Complete
        let suite = task_suites::Entity::find_by_id(suite.id)
            .one(&db)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(suite.state, TaskSuiteState::Complete.to_i32());
        assert!(suite.completed_at.is_some());
    }

    #[tokio::test]
    async fn test_no_transition_when_recent_activity() {
        let db = /* your test db connection */;

        // Create suite with recent activity
        let recent_time = Utc::now() - Duration::seconds(30); // Only 30 seconds ago
        let suite = task_suites::ActiveModel {
            uuid: Set(uuid::Uuid::new_v4()),
            group_id: Set(1),
            creator_id: Set(1),
            worker_schedule: Set(serde_json::json!({"worker_count": 4})),
            state: Set(TaskSuiteState::Open.to_i32()),
            last_task_submitted_at: Set(Some(recent_time)),
            pending_tasks: Set(5),
            ..Default::default()
        };
        let suite = suite.insert(&db).await.unwrap();

        // Run transition
        let count = auto_transition_suite_states(&db).await.unwrap();
        assert_eq!(count, 0); // Should not transition

        // Verify state still Open
        let suite = task_suites::Entity::find_by_id(suite.id)
            .one(&db)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(suite.state, TaskSuiteState::Open.to_i32());
    }
}
```

**Integration:** Add to coordinator main/startup:

```rust
// In coordinator main.rs or similar
use coordinator::suite_state_transitions;

#[tokio::main]
async fn main() {
    // ... existing setup ...

    let db = /* your db connection */;

    // Spawn background task for suite state transitions
    let db_clone = db.clone();
    tokio::spawn(async move {
        tracing::info!("Starting suite state transition background task");
        suite_state_transitions::run_state_transition_loop(db_clone).await;
    });

    // ... rest of coordinator startup ...
}
```

**Deliverable Criteria:**
- ✅ Function compiles and integrates with coordinator
- ✅ Open→Closed transition works after 3-minute timeout
- ✅ Open/Closed→Complete transition works when pending_tasks=0
- ✅ Background task runs every 30 seconds
- ✅ All tests pass
- ✅ Logging shows transition activity

**Testing Instructions:**

```bash
# Run unit tests
cargo test -p coordinator test_state_transitions

# Manual integration test:
# 1. Create suite, submit tasks
# 2. Wait 3+ minutes without new tasks
# 3. Verify suite transitions to Closed
# 4. Complete all tasks
# 5. Wait 30 seconds
# 6. Verify suite transitions to Complete

# Check logs for transition activity:
# "Transitioned suites from Open to Closed due to inactivity timeout"
# "Transitioned suites to Complete (all tasks finished)"
```

---

## Success Criteria

Phase 1 is considered complete when:

1. **All Migrations Pass**
   - All migration files compile and run successfully
   - Can be rolled back without errors
   - Database schema matches RFC specification

2. **All Tables Created**
   - 5 new tables: `task_suites`, `node_managers`, `group_node_manager`, `task_suite_managers`, `task_execution_failures`
   - 1 table altered: `active_tasks` (added `task_suite_id` column)
   - All indexes created and optimized

3. **SeaORM Entities Work**
   - 5 new entity files compile successfully
   - All relationships defined correctly
   - CRUD operations work for all entities

4. **Enums Defined**
   - 4 enum types created and compile successfully
   - Conversion functions work correctly
   - Can be used in API schema (Phase 2)

5. **Coordinator Logic Works**
   - Task count updates work on task submission/completion
   - State transition background job runs correctly
   - All tests pass
   - No database triggers needed

6. **All Pieces Independently Verified**
   - Each piece (1.1-1.9) tested individually
   - All deliverable criteria met
   - No breaking changes to existing functionality

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

## Best Practices

1. **Test Each Piece Independently**: Don't move to next piece until current piece is verified
2. **Use Transactions**: Ensure atomicity of database operations
3. **Backup First**: Always backup database before running migrations
4. **Check Incrementally**: Verify schema after each migration
5. **Log Everything**: Add tracing/logging to coordinator logic for debugging

## Common Pitfalls

1. **Forgetting to call counter functions**: Task counts won't update if you forget to hook in piece 1.8
2. **Race conditions**: Use database-level atomic operations (UPDATE ... SET col = col + 1)
3. **Negative counts**: Always use GREATEST(count - 1, 0) to prevent negative values
4. **Background task not running**: Make sure piece 1.9 is spawned on coordinator startup
5. **Array vs JSONB**: PostgreSQL arrays (TEXT[]) are not the same as JSONB arrays

## Debugging Tips

1. **Check table schemas**: `\d+ table_name` in psql
2. **Verify indexes**: `\di table_name*` in psql
3. **Monitor counters**: Add logging to verify total_tasks/pending_tasks match reality
4. **Check background task**: Look for "Starting suite state transition background task" in logs
5. **Foreign key violations**: Use `\d+ table_name` to see all constraints

## Performance Considerations

- GIN indexes on `tags` and `labels` support fast containment queries (@>)
- Partial indexes reduce index size and improve write performance
- Background job runs every 30 seconds (configurable if needed)
- Atomic UPDATE operations prevent race conditions on counters

---

**End of Phase 1 Implementation Guide**
