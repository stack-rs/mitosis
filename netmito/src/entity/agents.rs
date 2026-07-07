//! `SeaORM` Entity for agents table
//!
//! An agent row is the durable identity of a registered machine instance. It
//! is **not** deleted when the agent goes online/offline; `state` only reflects
//! the live session. The subordinate `machines` row (physical machine code and
//! metadata) points back at the agent (`machines.agent_id`), so an agent row
//! cannot be deleted while its machine row still exists — the machine row must
//! be removed first. See docs/plans/2026-07-03-suite-entity-design.md.

use sea_orm::entity::prelude::*;

use super::state::AgentState;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "agents")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    #[sea_orm(unique)]
    pub uuid: Uuid,
    pub creator_id: i64,
    pub tags: Vec<String>,
    pub labels: Vec<String>,
    pub state: AgentState,
    pub last_heartbeat: TimeDateTimeWithTimeZone,
    pub assigned_task_suite_id: Option<i64>,
    pub created_at: TimeDateTimeWithTimeZone,
    pub updated_at: TimeDateTimeWithTimeZone,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::users::Entity",
        from = "Column::CreatorId",
        to = "super::users::Column::Id",
        on_update = "Cascade",
        on_delete = "Restrict"
    )]
    Users,
    #[sea_orm(
        belongs_to = "super::task_suites::Entity",
        from = "Column::AssignedTaskSuiteId",
        to = "super::task_suites::Column::Id",
        on_update = "Cascade",
        on_delete = "Restrict"
    )]
    TaskSuites,
    /// The subordinate machine row (1:1); it holds the FK back to this agent.
    #[sea_orm(has_one = "super::machines::Entity")]
    Machines,
}

impl Related<super::users::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Users.def()
    }
}

impl Related<super::task_suites::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::TaskSuites.def()
    }
}

impl Related<super::machines::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Machines.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
