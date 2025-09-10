use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Critical indices for active_tasks table
        manager
            .create_index(
                sea_query::Index::create()
                    .if_not_exists()
                    .name("idx_active_tasks-state")
                    .table(ActiveTasks::Table)
                    .col(ActiveTasks::State)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                sea_query::Index::create()
                    .if_not_exists()
                    .name("idx_active_tasks-group_id")
                    .table(ActiveTasks::Table)
                    .col(ActiveTasks::GroupId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                sea_query::Index::create()
                    .if_not_exists()
                    .name("idx_active_tasks-priority")
                    .table(ActiveTasks::Table)
                    .col(ActiveTasks::Priority)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                sea_query::Index::create()
                    .if_not_exists()
                    .name("idx_active_tasks-creator_id-group_id")
                    .table(ActiveTasks::Table)
                    .col(ActiveTasks::CreatorId)
                    .col(ActiveTasks::GroupId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                sea_query::Index::create()
                    .if_not_exists()
                    .name("idx_archived_tasks-state")
                    .table(ArchivedTasks::Table)
                    .col(ArchivedTasks::State)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                sea_query::Index::create()
                    .if_not_exists()
                    .name("idx_archived_tasks-group_id")
                    .table(ArchivedTasks::Table)
                    .col(ArchivedTasks::GroupId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                sea_query::Index::create()
                    .if_not_exists()
                    .name("idx_archived_tasks-priority")
                    .table(ArchivedTasks::Table)
                    .col(ArchivedTasks::Priority)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                sea_query::Index::create()
                    .if_not_exists()
                    .name("idx_archived_tasks-creator_id-group_id")
                    .table(ArchivedTasks::Table)
                    .col(ArchivedTasks::CreatorId)
                    .col(ArchivedTasks::GroupId)
                    .to_owned(),
            )
            .await?;

        // Index for group-worker relationships
        manager
            .create_index(
                sea_query::Index::create()
                    .if_not_exists()
                    .name("idx_group_worker_group_role")
                    .table(GroupWorker::Table)
                    .col(GroupWorker::GroupId)
                    .col(GroupWorker::Role)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                sea_query::Index::create()
                    .if_not_exists()
                    .name("idx_group_worker_worker_id")
                    .table(GroupWorker::Table)
                    .col(GroupWorker::WorkerId)
                    .to_owned(),
            )
            .await?;

        // Index for workers
        manager
            .create_index(
                sea_query::Index::create()
                    .if_not_exists()
                    .name("idx_workers_creator_id")
                    .table(Workers::Table)
                    .col(Workers::CreatorId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                sea_query::Index::create()
                    .if_not_exists()
                    .name("idx_workers_state_heartbeat")
                    .table(Workers::Table)
                    .col(Workers::State)
                    .col(Workers::LastHeartbeat)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                sea_query::Index::create()
                    .if_not_exists()
                    .name("idx_groups_creator_id")
                    .table(Groups::Table)
                    .col(Groups::CreatorId)
                    .to_owned(),
            )
            .await?;

        // Index for artifacts
        manager
            .create_index(
                sea_query::Index::create()
                    .if_not_exists()
                    .name("idx_artifacts_task_id")
                    .table(Artifacts::Table)
                    .col(Artifacts::TaskId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                sea_query::Index::create()
                    .if_not_exists()
                    .name("idx_attachments_group_id")
                    .table(Attachments::Table)
                    .col(Attachments::GroupId)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Drop all indices in reverse order
        let indices = [
            "idx_active_tasks-state",
            "idx_active_tasks-group_id",
            "idx_active_tasks-priority",
            "idx_active_tasks-creator_id-group_id",
            "idx_archived_tasks-state",
            "idx_archived_tasks-group_id",
            "idx_archived_tasks-priority",
            "idx_archived_tasks-creator_id-group_id",
            "idx_group_worker_group_role",
            "idx_group_worker_worker_id",
            "idx_workers_creator_id",
            "idx_workers_state_heartbeat",
            "idx_groups_creator_id",
            "idx_artifacts_task_id",
            "idx_attachments_group_id",
        ];

        for index_name in indices.iter() {
            manager
                .drop_index(sea_query::Index::drop().name(*index_name).to_owned())
                .await
                .ok(); // Ignore errors for indices that might not exist
        }

        Ok(())
    }
}

#[derive(Iden)]
enum ActiveTasks {
    Table,
    State,
    GroupId,
    Priority,
    CreatorId,
}

#[derive(Iden)]
enum ArchivedTasks {
    Table,
    State,
    GroupId,
    Priority,
    CreatorId,
}

#[derive(Iden)]
enum GroupWorker {
    Table,
    GroupId,
    WorkerId,
    Role,
}

#[derive(Iden)]
enum Workers {
    Table,
    CreatorId,
    State,
    LastHeartbeat,
}

#[derive(Iden)]
enum Groups {
    Table,
    CreatorId,
}

#[derive(Iden)]
enum Artifacts {
    Table,
    TaskId,
}

#[derive(Iden)]
enum Attachments {
    Table,
    GroupId,
}
