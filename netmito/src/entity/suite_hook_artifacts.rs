//! `SeaORM` Entity for suite_hook_artifacts table
//!
//! Metadata + size for a hook execution's uploaded logs. The bytes live in S3
//! under `hooks/{suite_hook_execution_id}/{content_type}`; this row drives
//! quota accounting and cascade-deletes with its hook execution (and thus with
//! the run). See docs/plans/2026-06-15-run-interaction-design.md (A.3.5).

use sea_orm::entity::prelude::*;

use super::content::ArtifactContentType;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "suite_hook_artifacts")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub suite_hook_execution_id: i64,
    pub content_type: ArtifactContentType,
    /// Size in bytes; refunded to the group's `storage_used` on delete.
    pub size: i64,
    pub created_at: TimeDateTimeWithTimeZone,
    pub updated_at: TimeDateTimeWithTimeZone,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::suite_hook_executions::Entity",
        from = "Column::SuiteHookExecutionId",
        to = "super::suite_hook_executions::Column::Id",
        on_update = "Cascade",
        on_delete = "Cascade"
    )]
    SuiteHookExecutions,
}

impl Related<super::suite_hook_executions::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::SuiteHookExecutions.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
