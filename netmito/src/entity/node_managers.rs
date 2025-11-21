//! `SeaORM` Entity for node_managers table

use sea_orm::entity::prelude::*;

use super::state::NodeManagerState;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "node_managers")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    #[sea_orm(unique)]
    pub uuid: Uuid,
    pub creator_id: i64,
    pub tags: Vec<String>,
    pub labels: Vec<String>,
    pub state: NodeManagerState,
    pub last_heartbeat: TimeDateTimeWithTimeZone,
    pub assigned_task_suite_id: Option<i64>,
    // TODO: may remove this filed as I don't know what this is for
    pub lease_expires_at: Option<TimeDateTimeWithTimeZone>,
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
        on_delete = "SetNull"
    )]
    TaskSuites,
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

impl ActiveModelBehavior for ActiveModel {}
