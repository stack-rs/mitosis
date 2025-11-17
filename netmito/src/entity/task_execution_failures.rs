//! `SeaORM` Entity for task_execution_failures table

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "task_execution_failures")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub task_id: i64,
    // TODO: these fields may be removed as they are redundant
    // But if they are removed finally upon completion, I think it is also fine.
    pub task_uuid: Uuid,
    pub task_suite_id: Option<i64>,
    pub manager_id: i64,
    pub failure_count: i32,
    pub last_failure_at: TimeDateTimeWithTimeZone,
    pub error_messages: Option<Vec<String>>,
    // TODO: this maybe removed as not necessary
    pub worker_local_id: Option<i32>,
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
        belongs_to = "super::node_managers::Entity",
        from = "Column::ManagerId",
        to = "super::node_managers::Column::Id",
        on_update = "Cascade",
        on_delete = "Cascade"
    )]
    Managers,
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

impl ActiveModelBehavior for ActiveModel {}
