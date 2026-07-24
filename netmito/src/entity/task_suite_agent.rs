//! `SeaORM` Entity for task_suite_agent table

use std::fmt::Display;

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "task_suite_agent")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub task_suite_id: i64,
    pub agent_id: i64,
    pub override_type: SuiteAgentOverrideType,
    /// Nullable so the creator FK can be `on_delete=SetNull`.
    pub creator_id: Option<i64>,
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
        on_delete = "Restrict"
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
    #[sea_orm(
        belongs_to = "super::users::Entity",
        from = "Column::CreatorId",
        to = "super::users::Column::Id",
        on_update = "Cascade",
        on_delete = "SetNull"
    )]
    Users,
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

impl Related<super::users::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Users.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}

/// User's manual override over whether an agent should be assoiated with a suite
#[derive(EnumIter, DeriveActiveEnum, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Copy)]
#[sea_orm(rs_type = "i32", db_type = "Integer")]
pub enum SuiteAgentOverrideType {
    /// Agent was manually selected by user
    UserIncluded = 0,
    /// Agent was manually excluded by user
    UserExcluded = 1,
}

impl Display for SuiteAgentOverrideType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SuiteAgentOverrideType::UserIncluded => write!(f, "UserIncluded"),
            SuiteAgentOverrideType::UserExcluded => write!(f, "UserExcluded"),
        }
    }
}
