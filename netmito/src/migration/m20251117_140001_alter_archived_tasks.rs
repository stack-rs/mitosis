use sea_orm_migration::prelude::*;
use uuid::Uuid;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Convert spec/result to jsonb and add new columns
        manager
            .alter_table(
                Table::alter()
                    .table(ArchivedTasks::Table)
                    .modify_column(ColumnDef::new(ArchivedTasks::Spec).json_binary())
                    .modify_column(ColumnDef::new(ArchivedTasks::Result).json_binary())
                    .add_column_if_not_exists(
                        ColumnDef::new(ArchivedTasks::TaskSuiteId).big_integer(),
                    )
                    .add_column_if_not_exists(
                        ColumnDef::new(ArchivedTasks::ExecOptions).json_binary(),
                    )
                    .add_column_if_not_exists(ColumnDef::new(ArchivedTasks::RunnerUuid).uuid())
                    .to_owned(),
            )
            .await?;

        // Migrate timeout (seconds) into spec and extract watch into exec_options
        manager
            .get_connection()
            .execute_unprepared(
                r#"UPDATE archived_tasks SET
                    spec = (spec || jsonb_build_object('timeout', timeout)) - 'watch',
                    exec_options = CASE
                        WHEN spec->'watch' IS NOT NULL THEN jsonb_build_object('watch', spec->'watch')
                        ELSE NULL
                    END"#,
            )
            .await?;

        // Migrate reporter_uuid / assigned_worker (i64 row id) to runner_uuid (UUID)
        let subquery = Query::select()
            .column((Workers::Table, Workers::WorkerId))
            .from(Workers::Table)
            .and_where(
                Expr::col((Workers::Table, Workers::Id))
                    .equals((ArchivedTasks::Table, ArchivedTasks::AssignedWorker)),
            )
            .to_owned();

        let stmt = Query::update()
            .table(ArchivedTasks::Table)
            .value(
                ArchivedTasks::RunnerUuid,
                CaseStatement::new()
                    .case(
                        Expr::col(ArchivedTasks::ReporterUuid).is_not_null(),
                        Expr::col(ArchivedTasks::ReporterUuid),
                    )
                    .case(
                        Expr::col(ArchivedTasks::AssignedWorker).is_not_null(),
                        Func::coalesce([
                            SimpleExpr::SubQuery(
                                None,
                                Box::new(subquery.into_sub_query_statement()),
                            ),
                            Expr::val(Uuid::nil()).into(),
                        ]),
                    )
                    .finally(SimpleExpr::Keyword(Keyword::Null)),
            )
            .to_owned();

        manager
            .get_connection()
            .execute(manager.get_database_backend().build(&stmt))
            .await?;

        // Drop reporter_uuid index, then all migrated columns
        manager
            .drop_index(
                Index::drop()
                    .name("idx_archived_tasks-reporter_uuid")
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(ArchivedTasks::Table)
                    .drop_column(ArchivedTasks::Timeout)
                    .drop_column(ArchivedTasks::AssignedWorker)
                    .drop_column(ArchivedTasks::ReporterUuid)
                    .to_owned(),
            )
            .await?;

        // Create partial indices on task_suite_id and runner_uuid
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

        manager
            .create_index(
                sea_query::Index::create()
                    .if_not_exists()
                    .name("idx_archived_tasks-runner_uuid")
                    .table(ArchivedTasks::Table)
                    .col(ArchivedTasks::RunnerUuid)
                    .and_where(Expr::col(ArchivedTasks::RunnerUuid).is_not_null())
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Drop indices
        manager
            .drop_index(
                Index::drop()
                    .name("idx_archived_tasks-runner_uuid")
                    .table(ArchivedTasks::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .name("idx_archived_tasks-task_suite_id")
                    .table(ArchivedTasks::Table)
                    .to_owned(),
            )
            .await?;

        // Add removed columns back
        manager
            .alter_table(
                Table::alter()
                    .table(ArchivedTasks::Table)
                    .add_column(ColumnDef::new(ArchivedTasks::ReporterUuid).uuid())
                    .add_column(ColumnDef::new(ArchivedTasks::AssignedWorker).big_integer())
                    .add_column(
                        ColumnDef::new(ArchivedTasks::Timeout)
                            .big_integer()
                            .not_null()
                            .default(300),
                    )
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

        // Restore timeout (seconds) from spec and watch from exec_options
        manager
            .get_connection()
            .execute_unprepared(
                r#"UPDATE archived_tasks SET
                    timeout = COALESCE((spec->>'timeout')::bigint, 300),
                    spec = (spec - 'timeout') || CASE
                        WHEN exec_options->'watch' IS NOT NULL THEN jsonb_build_object('watch', exec_options->'watch')
                        ELSE '{}'::jsonb
                    END"#,
            )
            .await?;

        // Drop added columns and revert spec/result to json
        manager
            .alter_table(
                Table::alter()
                    .table(ArchivedTasks::Table)
                    .drop_column(ArchivedTasks::RunnerUuid)
                    .drop_column(ArchivedTasks::ExecOptions)
                    .drop_column(ArchivedTasks::TaskSuiteId)
                    .modify_column(ColumnDef::new(ArchivedTasks::Spec).json())
                    .modify_column(ColumnDef::new(ArchivedTasks::Result).json())
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
    RunnerUuid,
    Timeout,
    AssignedWorker,
    ReporterUuid,
    Spec,
    Result,
}

#[derive(DeriveIden)]
enum Workers {
    Table,
    Id,
    WorkerId,
}
