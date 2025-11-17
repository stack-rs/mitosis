use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(TaskExecutionFailures::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(TaskExecutionFailures::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(TaskExecutionFailures::TaskId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(TaskExecutionFailures::TaskUuid)
                            .uuid()
                            .not_null(),
                    )
                    .col(ColumnDef::new(TaskExecutionFailures::TaskSuiteId).big_integer())
                    .col(
                        ColumnDef::new(TaskExecutionFailures::ManagerId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(TaskExecutionFailures::FailureCount)
                            .integer()
                            .not_null()
                            .default(1),
                    )
                    .col(
                        ColumnDef::new(TaskExecutionFailures::LastFailureAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(TaskExecutionFailures::ErrorMessages)
                            .array(ColumnType::Text),
                    )
                    .col(ColumnDef::new(TaskExecutionFailures::WorkerLocalId).integer())
                    .col(
                        ColumnDef::new(TaskExecutionFailures::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(TaskExecutionFailures::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-task_execution_failures-task_suite_id")
                            .from(
                                TaskExecutionFailures::Table,
                                TaskExecutionFailures::TaskSuiteId,
                            )
                            .to(TaskSuites::Table, TaskSuites::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-task_execution_failures-manager_id")
                            .from(
                                TaskExecutionFailures::Table,
                                TaskExecutionFailures::ManagerId,
                            )
                            .to(NodeManagers::Table, NodeManagers::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Unique constraint on (task_uuid, manager_id)
        manager
            .create_index(
                Index::create()
                    .name("idx_task_execution_failures-task_uuid-manager_id")
                    .table(TaskExecutionFailures::Table)
                    .col(TaskExecutionFailures::TaskUuid)
                    .col(TaskExecutionFailures::ManagerId)
                    .unique()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_task_execution_failures-task_uuid")
                    .table(TaskExecutionFailures::Table)
                    .col(TaskExecutionFailures::TaskUuid)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_task_execution_failures-manager_id")
                    .table(TaskExecutionFailures::Table)
                    .col(TaskExecutionFailures::ManagerId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_task_execution_failures-task_suite_id")
                    .table(TaskExecutionFailures::Table)
                    .col(TaskExecutionFailures::TaskSuiteId)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                sea_query::Index::drop()
                    .name("idx_task_execution_failures-task_suite_id")
                    .to_owned(),
            )
            .await?;
        manager
            .drop_index(
                sea_query::Index::drop()
                    .name("idx_task_execution_failures-manager_id")
                    .to_owned(),
            )
            .await?;
        manager
            .drop_index(
                sea_query::Index::drop()
                    .name("idx_task_execution_failures-task_uuid")
                    .to_owned(),
            )
            .await?;
        manager
            .drop_index(
                sea_query::Index::drop()
                    .name("idx_task_execution_failures-task_uuid-manager_id")
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(Table::drop().table(TaskExecutionFailures::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum TaskExecutionFailures {
    Table,
    Id,
    TaskId,
    TaskUuid,
    TaskSuiteId,
    ManagerId,
    FailureCount,
    LastFailureAt,
    ErrorMessages,
    WorkerLocalId,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum TaskSuites {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum NodeManagers {
    Table,
    Id,
}
