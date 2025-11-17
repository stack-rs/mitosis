use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(TaskSuites::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(TaskSuites::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(TaskSuites::Uuid)
                            .uuid()
                            .not_null()
                            .unique_key(),
                    )
                    .col(ColumnDef::new(TaskSuites::Name).text())
                    .col(ColumnDef::new(TaskSuites::Description).text())
                    .col(ColumnDef::new(TaskSuites::GroupId).big_integer().not_null())
                    .col(
                        ColumnDef::new(TaskSuites::CreatorId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(TaskSuites::Tags)
                            .array(ColumnType::Text)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(TaskSuites::Labels)
                            .array(ColumnType::Text)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(TaskSuites::Priority)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(TaskSuites::WorkerSchedule)
                            .json_binary()
                            .not_null(),
                    )
                    .col(ColumnDef::new(TaskSuites::EnvPreparation).json_binary())
                    .col(ColumnDef::new(TaskSuites::EnvCleanup).json_binary())
                    .col(
                        ColumnDef::new(TaskSuites::State)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(ColumnDef::new(TaskSuites::LastTaskSubmittedAt).timestamp_with_time_zone())
                    .col(
                        ColumnDef::new(TaskSuites::TotalTasks)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(TaskSuites::PendingTasks)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(TaskSuites::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(TaskSuites::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(ColumnDef::new(TaskSuites::CompletedAt).timestamp_with_time_zone())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-task_suites-group")
                            .from(TaskSuites::Table, TaskSuites::GroupId)
                            .to(Groups::Table, Groups::Id)
                            .on_delete(ForeignKeyAction::Restrict)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-task_suites-creator")
                            .from(TaskSuites::Table, TaskSuites::CreatorId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Restrict)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Create indexes
        manager
            .create_index(
                Index::create()
                    .name("idx_task_suites-group_id")
                    .table(TaskSuites::Table)
                    .col(TaskSuites::GroupId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_task_suites-creator_id")
                    .table(TaskSuites::Table)
                    .col(TaskSuites::CreatorId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_task_suites-state")
                    .table(TaskSuites::Table)
                    .col(TaskSuites::State)
                    .to_owned(),
            )
            .await?;

        // GIN indexes for array columns
        manager
            .create_index(
                sea_query::Index::create()
                    .if_not_exists()
                    .name("idx_tasks_suites-tags_gin")
                    .table(TaskSuites::Table)
                    .col(TaskSuites::Tags)
                    .full_text()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                sea_query::Index::create()
                    .if_not_exists()
                    .name("idx_tasks_suites-labels_gin")
                    .table(TaskSuites::Table)
                    .col(TaskSuites::Labels)
                    .full_text()
                    .to_owned(),
            )
            .await?;
        // Partial index for auto-close timeout queries
        manager
            .create_index(
                sea_query::Index::create()
                    .if_not_exists()
                    .name("idx_task_suites-auto_close")
                    .table(TaskSuites::Table)
                    .col(TaskSuites::LastTaskSubmittedAt)
                    .and_where(Expr::col(TaskSuites::State).eq(0))
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                sea_query::Index::drop()
                    .name("idx_task_suites-auto_close")
                    .to_owned(),
            )
            .await?;
        manager
            .drop_index(
                sea_query::Index::drop()
                    .name("idx_tasks_suites-labels_gin")
                    .to_owned(),
            )
            .await?;
        manager
            .drop_index(
                sea_query::Index::drop()
                    .name("idx_tasks_suites-tags_gin")
                    .to_owned(),
            )
            .await?;
        manager
            .drop_index(
                sea_query::Index::drop()
                    .name("idx_task_suites-state")
                    .to_owned(),
            )
            .await?;
        manager
            .drop_index(
                sea_query::Index::drop()
                    .name("idx_task_suites-creator_id")
                    .to_owned(),
            )
            .await?;
        manager
            .drop_index(
                sea_query::Index::drop()
                    .name("idx_task_suites-group_id")
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(Table::drop().table(TaskSuites::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum TaskSuites {
    Table,
    Id,
    Uuid,
    Name,
    Description,
    GroupId,
    CreatorId,
    Tags,
    Labels,
    Priority,
    WorkerSchedule,
    EnvPreparation,
    EnvCleanup,
    State,
    LastTaskSubmittedAt,
    TotalTasks,
    PendingTasks,
    CreatedAt,
    UpdatedAt,
    CompletedAt,
}

#[derive(DeriveIden)]
enum Groups {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum Users {
    Table,
    Id,
}
