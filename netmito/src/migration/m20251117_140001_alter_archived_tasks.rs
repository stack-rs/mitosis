use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Add task_suite_id column
        manager
            .alter_table(
                Table::alter()
                    .table(ArchivedTasks::Table)
                    .add_column_if_not_exists(
                        ColumnDef::new(ArchivedTasks::TaskSuiteId).big_integer(),
                    )
                    .to_owned(),
            )
            .await?;

        // Create index on task_suite_id (partial index for archived tasks)
        manager
            .create_index(
                sea_query::Index::create()
                    .if_not_exists()
                    .name("idx_archived_tasks-task_suite_id")
                    .table(ArchivedTasks::Table)
                    .col(ArchivedTasks::TaskSuiteId)
                    .and_where(Expr::col(ArchivedTasks::TaskSuiteId).is_not_null())
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Drop index first
        manager
            .drop_index(
                Index::drop()
                    .name("idx_archived_tasks-task_suite_id")
                    .table(ArchivedTasks::Table)
                    .to_owned(),
            )
            .await?;

        // Drop column
        manager
            .alter_table(
                Table::alter()
                    .table(ArchivedTasks::Table)
                    .drop_column(ArchivedTasks::TaskSuiteId)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum ArchivedTasks {
    Table,
    TaskSuiteId,
}
