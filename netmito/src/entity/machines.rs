//! `SeaORM` Entity for machines table
//!
//! Appendix information for an [`agents`](super::agents) row: the physical
//! machine code and metadata of the box an agent instance runs on. The row is
//! owned by the agent via `agent_id` (1:1) with `on_delete = Restrict`, so the
//! machine row must be deleted before its agent row can be removed. Neither is
//! deleted through the normal HTTP/service path; removal is an admin SQL action.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "machines")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// Owning agent (1:1). RESTRICT: the agent row cannot be deleted while this
    /// machine row references it.
    #[sea_orm(unique)]
    pub agent_id: i64,
    #[sea_orm(unique)]
    pub machine_code: String,
    pub metadata: Option<Json>,
    pub first_seen_at: TimeDateTimeWithTimeZone,
    pub last_seen_at: TimeDateTimeWithTimeZone,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::agents::Entity",
        from = "Column::AgentId",
        to = "super::agents::Column::Id",
        on_update = "Cascade",
        on_delete = "Restrict"
    )]
    Agents,
}

impl Related<super::agents::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Agents.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
