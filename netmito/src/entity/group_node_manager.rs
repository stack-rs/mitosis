//! `SeaORM` Entity for group_node_manager table

use sea_orm::entity::prelude::*;

use super::role::GroupNodeManagerRole;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "group_node_manager")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub group_id: i64,
    pub manager_id: i64,
    pub role: GroupNodeManagerRole,
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
        belongs_to = "super::node_managers::Entity",
        from = "Column::ManagerId",
        to = "super::node_managers::Column::Id",
        on_update = "Cascade",
        on_delete = "Restrict"
    )]
    Managers,
}

impl Related<super::groups::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Groups.def()
    }
}

impl Related<super::node_managers::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Managers.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
