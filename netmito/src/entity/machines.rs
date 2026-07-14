//! `SeaORM` Entity for machines table
//!
//! Appendix information for an [`agents`](super::agents) row: the physical
//! machine code and metadata of the box an agent instance runs on.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "machines")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    #[sea_orm(unique)]
    pub agent_id: i64,
    #[sea_orm(column_type = "Text", unique)]
    pub machine_code: String,
    #[sea_orm(column_type = "JsonBinary", nullable)]
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
