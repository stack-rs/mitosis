//! `SeaORM` Entity for suite_hook_executions table
//!
//! Tracks individual hook execution attempts (provision, cleanup, background)
//! with full lifecycle state. Each row represents one execution attempt
//! of a hook by an agent for a suite.

use sea_orm::entity::prelude::*;

use super::state::{HookExecState, HookType};

// TODO: this have to be some temporary tasks executed by the agent. We record them for better
// observability and debugging. We should add an expiry mechanism to wipe out outdated records to
// save storage. We can also add a TTL index on `completed_at` to automatically delete old records
// after certain period of time. More fields might be required to better serve this purpose.
// TODO: I'm not sure if we have to support some kinds of 'temp tasks' for some checks, for example
// check for some system information.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "suite_hook_executions")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub task_suite_id: i64,
    pub agent_id: i64,
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
        on_delete = "Cascade"
    )]
    Agents,
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

impl ActiveModelBehavior for ActiveModel {}
