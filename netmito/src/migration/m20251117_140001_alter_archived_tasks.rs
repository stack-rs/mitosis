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

        // Add exec_options column
        manager
            .alter_table(
                Table::alter()
                    .table(ArchivedTasks::Table)
                    .add_column_if_not_exists(ColumnDef::new(ArchivedTasks::ExecOptions).json())
                    .to_owned(),
            )
            .await?;

        // Add runner_id column (UUID, nullable)
        manager
            .alter_table(
                Table::alter()
                    .table(ArchivedTasks::Table)
                    .add_column_if_not_exists(ColumnDef::new(ArchivedTasks::RunnerId).uuid())
                    .to_owned(),
            )
            .await?;

        // Migrate timeout into spec JSON and extract watch into exec_options
        let db = manager.get_connection();
        db.execute_unprepared(
            r#"
            UPDATE archived_tasks SET
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
            UPDATE archived_tasks SET spec = spec - 'watch'
            "#,
        )
        .await?;

        // Migrate assigned_worker + reporter_uuid into runner_id:
        // Priority: reporter_uuid (already UUID) > worker UUID lookup > all-zeros > NULL
        db.execute_unprepared(
            r#"
            UPDATE archived_tasks SET
                runner_id = CASE
                    WHEN reporter_uuid IS NOT NULL THEN reporter_uuid
                    WHEN assigned_worker IS NOT NULL THEN
                        COALESCE(
                            (SELECT w.uuid FROM workers w WHERE w.id = archived_tasks.assigned_worker),
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
                    .table(ArchivedTasks::Table)
                    .drop_column(ArchivedTasks::Timeout)
                    .to_owned(),
            )
            .await?;

        // Drop assigned_worker column
        manager
            .alter_table(
                Table::alter()
                    .table(ArchivedTasks::Table)
                    .drop_column(ArchivedTasks::AssignedWorker)
                    .to_owned(),
            )
            .await?;

        // Drop reporter_uuid index (created by m20251031_000001)
        manager
            .drop_index(
                Index::drop()
                    .name("idx_archived_tasks-reporter_uuid")
                    .to_owned(),
            )
            .await?;

        // Drop reporter_uuid column
        manager
            .alter_table(
                Table::alter()
                    .table(ArchivedTasks::Table)
                    .drop_column(ArchivedTasks::ReporterUuid)
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

        // Create partial index on runner_id
        manager
            .create_index(
                sea_query::Index::create()
                    .if_not_exists()
                    .name("idx_archived_tasks-runner_id")
                    .table(ArchivedTasks::Table)
                    .col(ArchivedTasks::RunnerId)
                    .and_where(Expr::col(ArchivedTasks::RunnerId).is_not_null())
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
                    .name("idx_archived_tasks-runner_id")
                    .table(ArchivedTasks::Table)
                    .to_owned(),
            )
            .await?;

        // Drop task_suite_id index
        manager
            .drop_index(
                Index::drop()
                    .name("idx_archived_tasks-task_suite_id")
                    .table(ArchivedTasks::Table)
                    .to_owned(),
            )
            .await?;

        // Add reporter_uuid column back
        manager
            .alter_table(
                Table::alter()
                    .table(ArchivedTasks::Table)
                    .add_column(ColumnDef::new(ArchivedTasks::ReporterUuid).uuid())
                    .to_owned(),
            )
            .await?;

        // Recreate reporter_uuid index
        manager
            .create_index(
                sea_query::Index::create()
                    .if_not_exists()
                    .name("idx_archived_tasks-reporter_uuid")
                    .table(ArchivedTasks::Table)
                    .col(ArchivedTasks::ReporterUuid)
                    .and_where(Expr::col(ArchivedTasks::ReporterUuid).is_not_null())
                    .to_owned(),
            )
            .await?;

        // Add assigned_worker column back
        manager
            .alter_table(
                Table::alter()
                    .table(ArchivedTasks::Table)
                    .add_column(ColumnDef::new(ArchivedTasks::AssignedWorker).big_integer())
                    .to_owned(),
            )
            .await?;

        // Add timeout column back
        manager
            .alter_table(
                Table::alter()
                    .table(ArchivedTasks::Table)
                    .add_column(
                        ColumnDef::new(ArchivedTasks::Timeout)
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
            UPDATE archived_tasks SET
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
                    .table(ArchivedTasks::Table)
                    .drop_column(ArchivedTasks::RunnerId)
                    .to_owned(),
            )
            .await?;

        // Drop exec_options column
        manager
            .alter_table(
                Table::alter()
                    .table(ArchivedTasks::Table)
                    .drop_column(ArchivedTasks::ExecOptions)
                    .to_owned(),
            )
            .await?;

        // Drop task_suite_id column
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
    ExecOptions,
    RunnerId,
    Timeout,
    AssignedWorker,
    ReporterUuid,
}
