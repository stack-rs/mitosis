//! `SeaORM` Entity for suite_agent_runs table
//!
//! Each row represents one attempt of an agent to run a task suite
//! (accept → provision → execute → cleanup → terminal state).
//! Hook executions of an attempt reference the run via
//! `suite_hook_executions.suite_agent_run_id`.
//! See docs/plans/2026-06-10-suite-agent-run-design.md.

use sea_orm::entity::prelude::*;

use super::state::SuiteRunState;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "suite_agent_runs")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub task_suite_id: i64,
    /// User-facing run number, ascending per suite (across agents).
    /// Allocated as max(run_id)+1 inside the accept transaction.
    pub run_id: i32,
    /// FK to the live agent row; NULL once the agent is removed
    pub agent_id: Option<i64>,
    /// Denormalized registration identity; survives agent deletion
    pub agent_uuid: Uuid,
    /// Durable machine identity, copied from the agent at accept time.
    /// Always present: machine_code is mandatory at agent registration.
    pub machine_id: i64,
    pub state: SuiteRunState,
    /// Incremented live as tasks commit; reconciled on run completion
    pub tasks_completed: i32,
    /// Incremented live as tasks commit; reconciled on run completion
    pub tasks_failed: i32,
    /// `RunFailureReason` ({ kind, message }) summary of abnormal termination;
    /// full hook output lives in suite_hook_executions
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
    #[sea_orm(
        belongs_to = "super::machines::Entity",
        from = "Column::MachineId",
        to = "super::machines::Column::Id",
        on_update = "Cascade",
        on_delete = "Restrict"
    )]
    Machines,
    #[sea_orm(has_many = "super::suite_hook_executions::Entity")]
    SuiteHookExecutions,
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

impl Related<super::machines::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Machines.def()
    }
}

impl Related<super::suite_hook_executions::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::SuiteHookExecutions.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
