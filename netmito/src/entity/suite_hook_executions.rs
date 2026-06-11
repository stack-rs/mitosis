//! `SeaORM` Entity for suite_hook_executions table
//!
//! Tracks individual hook execution attempts (provision, cleanup, background)
//! with full lifecycle state. Each row represents one execution attempt of a
//! hook within a suite agent run (`suite_agent_runs`). Records are deleted
//! together with their run (cascade), so retention is handled at the run
//! level. See docs/plans/2026-06-10-suite-agent-run-design.md.

// TODO: I'm not sure if we have to support some kinds of 'temp tasks' for some checks, for example
// check for some system information.

use sea_orm::entity::prelude::*;

use super::state::{HookExecState, HookType};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "suite_hook_executions")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub suite_agent_run_id: i64,
    pub hook_type: HookType,
    pub spec: Json,
    pub state: HookExecState,
    pub result: Option<Json>,
    pub started_at: Option<TimeDateTimeWithTimeZone>,
    pub completed_at: Option<TimeDateTimeWithTimeZone>,
    pub created_at: TimeDateTimeWithTimeZone,
    pub updated_at: TimeDateTimeWithTimeZone,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::suite_agent_runs::Entity",
        from = "Column::SuiteAgentRunId",
        to = "super::suite_agent_runs::Column::Id",
        on_update = "Cascade",
        on_delete = "Cascade"
    )]
    SuiteAgentRuns,
}

impl Related<super::suite_agent_runs::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::SuiteAgentRuns.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
