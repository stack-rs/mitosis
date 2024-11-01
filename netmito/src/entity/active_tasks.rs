//! `SeaORM` Entity. Generated by sea-orm-codegen 0.12.15

use sea_orm::entity::prelude::*;

use super::state::TaskState;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "active_tasks")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub creator_id: i64,
    pub group_id: i64,
    pub task_id: i64,
    #[sea_orm(unique)]
    pub uuid: Uuid,
    pub tags: Vec<String>,
    pub labels: Vec<String>,
    pub created_at: TimeDateTimeWithTimeZone,
    pub updated_at: TimeDateTimeWithTimeZone,
    pub state: TaskState,
    pub assigned_worker: Option<i64>,
    pub timeout: i64,
    pub priority: i32,
    pub spec: Json,
    pub result: Option<Json>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::groups::Entity",
        from = "Column::GroupId",
        to = "super::groups::Column::Id",
        on_update = "Cascade",
        on_delete = "Restrict"
    )]
    Groups,
    #[sea_orm(
        belongs_to = "super::users::Entity",
        from = "Column::CreatorId",
        to = "super::users::Column::Id",
        on_update = "Cascade",
        on_delete = "Restrict"
    )]
    Users,
}

impl Related<super::groups::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Groups.def()
    }
}

impl Related<super::users::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Users.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}

impl From<Model> for super::archived_tasks::Model {
    fn from(task: Model) -> super::archived_tasks::Model {
        Self {
            id: task.id,
            creator_id: task.creator_id,
            group_id: task.group_id,
            task_id: task.task_id,
            uuid: task.uuid,
            tags: task.tags,
            labels: task.labels,
            created_at: task.created_at,
            updated_at: task.updated_at,
            state: task.state,
            assigned_worker: task.assigned_worker,
            timeout: task.timeout,
            priority: task.priority,
            spec: task.spec,
            result: task.result,
        }
    }
}