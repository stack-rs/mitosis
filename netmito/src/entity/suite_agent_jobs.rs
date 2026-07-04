//! `SeaORM` Entity for suite_agent_jobs table
//!
//! Each row is one **job**: an attempt of an agent running a task suite
//! (accept → provision → execute → cleanup → terminal state). Because agent
//! rows are durable, the job references the agent directly (`agent_id`) and
//! reaches the machine/uuid by join — no denormalized identity. We do not
//! expose an agent-deletion endpoint, but an admin may manually delete an
//! agent row via SQL (at their own risk); to preserve job history when that
//! happens, `agent_id` is nullable and the FK is `SetNull` rather than
//! `RESTRICT`. Hook tasks of a job reference it via
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
    /// Allocated as max(job_number)+1 inside the accept transaction.
    pub job_number: i32,
    /// Owning agent; the machine and registration uuid are reached by join.
    /// Agent rows are not reaped by the system, but an admin may manually
    /// delete one via SQL; the FK is `SetNull`, so this becomes NULL rather
    /// than blocking the delete, keeping the job's history intact.
    pub agent_id: Option<i64>,
    pub state: SuiteJobState,
    /// Incremented live as tasks commit; reconciled on job completion
    pub tasks_completed: i32,
    /// Incremented live as tasks commit; reconciled on job completion
    pub tasks_failed: i32,
    /// `{ kind, message }` summary of abnormal termination (including
    /// agent-lost); full hook output lives in the related hook task.
    pub failure_reason: Option<Json>,
    pub created_at: TimeDateTimeWithTimeZone,
    pub started_at: Option<TimeDateTimeWithTimeZone>,
    pub finished_at: Option<TimeDateTimeWithTimeZone>,
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
        on_delete = "SetNull"
    )]
    Agents,
    #[sea_orm(has_many = "super::hook_task::Entity")]
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

impl Related<super::hook_task::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::HookTasks.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
