//! `SeaORM` Entity for suite_agent_jobs table
//!
//! Each row is one **job**: an attempt of an agent running a task suite
//! (accept → provision → execute → cleanup → terminal state). Because agent
//! rows are durable, the job references the agent directly (`agent_id`) and
//! reaches the machine/uuid by join — no denormalized identity. We do not
//! expose an agent-deletion endpoint, and the FK is `RESTRICT`, so an agent
//! row that still owns jobs cannot be deleted (its history stays anchored to
//! the agent). `agent_id` is nullable only to leave room for future
//! detach/reassignment. Hook tasks of a job reference it via
//! `hook_tasks.suite_agent_job_id`.
//! See docs/plans/2026-07-03-suite-entity-design.md.

use sea_orm::entity::prelude::*;

use super::state::SuiteJobState;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "suite_agent_jobs")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub task_suite_id: i64,
    /// User-facing job number, ascending per suite (across agents).
    /// Allocated as max(job_id)+1 inside the accept transaction.
    pub job_id: i32,
    pub agent_id: i64,
    pub state: SuiteJobState,
    pub created_at: TimeDateTimeWithTimeZone,
    pub updated_at: TimeDateTimeWithTimeZone,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::task_suites::Entity",
        from = "Column::TaskSuiteId",
        to = "super::task_suites::Column::Id",
        on_update = "Cascade",
        on_delete = "Cascade"
    )]
    TaskSuites,
    #[sea_orm(
        belongs_to = "super::agents::Entity",
        from = "Column::AgentId",
        to = "super::agents::Column::Id",
        on_update = "Cascade",
        on_delete = "Restrict"
    )]
    Agents,
    #[sea_orm(has_many = "super::hook_tasks::Entity")]
    HookTasks,
}

impl Related<super::task_suites::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::TaskSuites.def()
    }
}

impl Related<super::agents::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Agents.def()
    }
}

impl Related<super::hook_tasks::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::HookTasks.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
