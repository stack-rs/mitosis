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
                    .table(ActiveTasks::Table)
                    .add_column_if_not_exists(
                        ColumnDef::new(ActiveTasks::TaskSuiteId).big_integer(),
                    )
                    .to_owned(),
            )
            .await?;

        // Add foreign key
        manager
            .alter_table(
                Table::alter()
                    .table(ActiveTasks::Table)
                    .add_foreign_key(
                        TableForeignKey::new()
                            .name("fk-active_tasks-task_suite_id")
                            .from_tbl(ActiveTasks::Table)
                            .from_col(ActiveTasks::TaskSuiteId)
                            .to_tbl(TaskSuites::Table)
                            .to_col(TaskSuites::Id)
                            .on_delete(ForeignKeyAction::SetNull)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Create partial index on task_suite_id (only non-NULL values)
        manager
            .create_index(
                sea_query::Index::create()
                    .if_not_exists()
                    .name("idx_active_tasks-task_suite_id")
                    .table(ActiveTasks::Table)
                    .col(ActiveTasks::TaskSuiteId)
                    .and_where(Expr::col(ActiveTasks::TaskSuiteId).is_not_null())
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
                    .name("idx_active_tasks-task_suite_id")
                    .table(ActiveTasks::Table)
                    .to_owned(),
            )
            .await?;

        // Drop foreign key
        manager
            .alter_table(
                Table::alter()
                    .table(ActiveTasks::Table)
                    .drop_foreign_key(Alias::new("fk-active_tasks-task_suite_id"))
                    .to_owned(),
            )
            .await?;

        // Drop column
        manager
            .alter_table(
                Table::alter()
                    .table(ActiveTasks::Table)
                    .drop_column(ActiveTasks::TaskSuiteId)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum ActiveTasks {
    Table,
    TaskSuiteId,
}

#[derive(DeriveIden)]
enum TaskSuites {
    Table,
    Id,
}
