//! `SeaORM` Entity for hook_tasks table
//!
//! One row per hook (provision / cleanup / background) executed within a suite
//! agent job. A hook task is task-shaped: it carries its own `uuid`, and its
//! logs are stored in the shared `artifacts` table keyed by that uuid
//! (`artifacts.task_id = hook_task.uuid`). Rows cascade-delete with their
//! job, so retention is handled at the job level.
use std::fmt::Display;

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use super::state::HookExecState;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "hook_tasks")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// Globally-unique handle; indexes this hook's artifacts in the shared
    /// `artifacts` table. Must not collide with task uuids.
    #[sea_orm(unique)]
    pub uuid: Uuid,
    pub suite_agent_job_id: i64,
    pub hook_type: HookType,
    #[sea_orm(column_type = "JsonBinary")]
    pub spec: Json,
    pub state: HookExecState,
    #[sea_orm(column_type = "JsonBinary", nullable)]
    pub result: Option<Json>,
    pub started_at: Option<TimeDateTimeWithTimeZone>,
    pub completed_at: Option<TimeDateTimeWithTimeZone>,
    pub created_at: TimeDateTimeWithTimeZone,
    pub updated_at: TimeDateTimeWithTimeZone,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::suite_agent_jobs::Entity",
        from = "Column::SuiteAgentJobId",
        to = "super::suite_agent_jobs::Column::Id",
        on_update = "Cascade",
        on_delete = "Cascade"
    )]
    SuiteAgentJobs,
}

impl Related<super::suite_agent_jobs::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::SuiteAgentJobs.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}

/// Types of suite hooks that can be executed by agents
#[derive(EnumIter, DeriveActiveEnum, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Copy)]
#[sea_orm(rs_type = "i32", db_type = "Integer")]
pub enum HookType {
    /// Environment provision hook (setup before task execution)
    Provision = 0,
    /// Environment cleanup hook (teardown after suite completion)
    Cleanup = 1,
    /// Background/sidecar process
    Background = 2,
}

impl Display for HookType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HookType::Provision => write!(f, "Provision"),
            HookType::Cleanup => write!(f, "Cleanup"),
            HookType::Background => write!(f, "Background"),
        }
    }
}
