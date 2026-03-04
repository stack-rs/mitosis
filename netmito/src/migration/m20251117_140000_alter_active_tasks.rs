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

        // Add exec_options column
        manager
            .alter_table(
                Table::alter()
                    .table(ActiveTasks::Table)
                    .add_column_if_not_exists(ColumnDef::new(ActiveTasks::ExecOptions).json())
                    .to_owned(),
            )
            .await?;

        // Add runner_id column (UUID, nullable)
        manager
            .alter_table(
                Table::alter()
                    .table(ActiveTasks::Table)
                    .add_column_if_not_exists(ColumnDef::new(ActiveTasks::RunnerId).uuid())
                    .to_owned(),
            )
            .await?;

        // Migrate timeout into spec JSON and extract watch into exec_options
        let db = manager.get_connection();
        db.execute_unprepared(
            r#"
            UPDATE active_tasks SET
                spec = spec || jsonb_build_object('timeout', timeout * 1000000000),
                exec_options = CASE
                    WHEN spec->'watch' IS NOT NULL THEN jsonb_build_object('watch', spec->'watch')
                    ELSE NULL
                END
            "#,
        )
        .await?;

        // Remove watch from spec after migration
        db.execute_unprepared(
            r#"
            UPDATE active_tasks SET spec = spec - 'watch'
            "#,
        )
        .await?;

        // Migrate assigned_worker (i64 worker ID) to runner_id (UUID)
        // Look up worker UUID from workers table; use all-zero UUID if not found
        db.execute_unprepared(
            r#"
            UPDATE active_tasks SET
                runner_id = CASE
                    WHEN assigned_worker IS NOT NULL THEN
                        COALESCE(
                            (SELECT w.uuid FROM workers w WHERE w.id = active_tasks.assigned_worker),
                            '00000000-0000-0000-0000-000000000000'::uuid
                        )
                    ELSE NULL
                END
            "#,
        )
        .await?;

        // Drop timeout column
        manager
            .alter_table(
                Table::alter()
                    .table(ActiveTasks::Table)
                    .drop_column(ActiveTasks::Timeout)
                    .to_owned(),
            )
            .await?;

        // Drop assigned_worker column
        manager
            .alter_table(
                Table::alter()
                    .table(ActiveTasks::Table)
                    .drop_column(ActiveTasks::AssignedWorker)
                    .to_owned(),
            )
            .await?;

        // Add foreign key for task_suite_id
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

        // Create partial index on runner_id (only non-NULL values)
        manager
            .create_index(
                sea_query::Index::create()
                    .if_not_exists()
                    .name("idx_active_tasks-runner_id")
                    .table(ActiveTasks::Table)
                    .col(ActiveTasks::RunnerId)
                    .and_where(Expr::col(ActiveTasks::RunnerId).is_not_null())
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Drop runner_id index
        manager
            .drop_index(
                Index::drop()
                    .name("idx_active_tasks-runner_id")
                    .table(ActiveTasks::Table)
                    .to_owned(),
            )
            .await?;

        // Drop task_suite_id index
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

        // Add assigned_worker column back
        manager
            .alter_table(
                Table::alter()
                    .table(ActiveTasks::Table)
                    .add_column(ColumnDef::new(ActiveTasks::AssignedWorker).big_integer())
                    .to_owned(),
            )
            .await?;

        // Add timeout column back
        manager
            .alter_table(
                Table::alter()
                    .table(ActiveTasks::Table)
                    .add_column(
                        ColumnDef::new(ActiveTasks::Timeout)
                            .big_integer()
                            .not_null()
                            .default(300),
                    )
                    .to_owned(),
            )
            .await?;

        // Migrate timeout back from spec and restore watch
        let db = manager.get_connection();
        db.execute_unprepared(
            r#"
            UPDATE active_tasks SET
                timeout = COALESCE((spec->>'timeout')::bigint / 1000000000, 300),
                spec = (spec - 'timeout') || CASE
                    WHEN exec_options->'watch' IS NOT NULL THEN jsonb_build_object('watch', exec_options->'watch')
                    ELSE '{}'::jsonb
                END
            "#,
        )
        .await?;

        // Drop runner_id column
        manager
            .alter_table(
                Table::alter()
                    .table(ActiveTasks::Table)
                    .drop_column(ActiveTasks::RunnerId)
                    .to_owned(),
            )
            .await?;

        // Drop exec_options column
        manager
            .alter_table(
                Table::alter()
                    .table(ActiveTasks::Table)
                    .drop_column(ActiveTasks::ExecOptions)
                    .to_owned(),
            )
            .await?;

        // Drop task_suite_id column
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
    ExecOptions,
    RunnerId,
    Timeout,
    AssignedWorker,
}

#[derive(DeriveIden)]
enum TaskSuites {
    Table,
    Id,
}
