//! `SeaORM` Entity for task_suite_node_manager table

use sea_orm::entity::prelude::*;

use super::state::SelectionType;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "task_suite_node_manager")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub task_suite_id: i64,
    pub manager_id: i64,
    pub selection_type: SelectionType,
    pub matched_tags: Option<Vec<String>>,
    pub created_at: TimeDateTimeWithTimeZone,
    pub creator_id: Option<i64>,
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
        belongs_to = "super::node_managers::Entity",
        from = "Column::ManagerId",
        to = "super::node_managers::Column::Id",
        on_update = "Cascade",
        on_delete = "Cascade"
    )]
    Managers,
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

impl Related<super::node_managers::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Managers.def()
    }
}

impl Related<super::users::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Users.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
