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
                    .table(ActiveTasks::Table)
                    .modify_column(ColumnDef::new(ActiveTasks::Spec).json_binary())
                    .modify_column(ColumnDef::new(ActiveTasks::Result).json_binary())
                    .add_column_if_not_exists(
                        ColumnDef::new(ActiveTasks::TaskSuiteId).big_integer(),
                    )
                    .add_column_if_not_exists(
                        ColumnDef::new(ActiveTasks::ExecOptions).json_binary(),
                    )
                    .add_column_if_not_exists(ColumnDef::new(ActiveTasks::RunnerUuid).uuid())
                    .to_owned(),
            )
            .await?;

        // Migrate timeout (seconds) into spec and extract watch into exec_options
        manager
            .get_connection()
            .execute_unprepared(
                r#"UPDATE active_tasks SET
                    spec = (spec || jsonb_build_object('timeout', timeout)) - 'watch',
                    exec_options = CASE
                        WHEN spec->'watch' IS NOT NULL THEN jsonb_build_object('watch', spec->'watch')
                        ELSE NULL
                    END"#,
            )
            .await?;

        // Migrate assigned_worker (i64 row id) to runner_uuid (UUID)
        let subquery = Query::select()
            .column((Workers::Table, Workers::WorkerId))
            .from(Workers::Table)
            .and_where(
                Expr::col((Workers::Table, Workers::Id))
                    .equals((ActiveTasks::Table, ActiveTasks::AssignedWorker)),
            )
            .to_owned();

        let stmt = Query::update()
            .table(ActiveTasks::Table)
            .value(
                ActiveTasks::RunnerUuid,
                CaseStatement::new()
                    .case(
                        Expr::col(ActiveTasks::AssignedWorker).is_not_null(),
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

        // Drop migrated columns
        manager
            .alter_table(
                Table::alter()
                    .table(ActiveTasks::Table)
                    .drop_column(ActiveTasks::Timeout)
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

        // Create partial indices on task_suite_id and runner_uuid
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

        manager
            .create_index(
                sea_query::Index::create()
                    .if_not_exists()
                    .name("idx_active_tasks-runner_uuid")
                    .table(ActiveTasks::Table)
                    .col(ActiveTasks::RunnerUuid)
                    .and_where(Expr::col(ActiveTasks::RunnerUuid).is_not_null())
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
                    .name("idx_active_tasks-runner_uuid")
                    .table(ActiveTasks::Table)
                    .to_owned(),
            )
            .await?;

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

        // Add removed columns back
        manager
            .alter_table(
                Table::alter()
                    .table(ActiveTasks::Table)
                    .add_column(ColumnDef::new(ActiveTasks::AssignedWorker).big_integer())
                    .add_column(
                        ColumnDef::new(ActiveTasks::Timeout)
                            .big_integer()
                            .not_null()
                            .default(300),
                    )
                    .to_owned(),
            )
            .await?;

        // Restore timeout (seconds) from spec and watch from exec_options
        manager
            .get_connection()
            .execute_unprepared(
                r#"UPDATE active_tasks SET
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
                    .table(ActiveTasks::Table)
                    .drop_column(ActiveTasks::RunnerUuid)
                    .drop_column(ActiveTasks::ExecOptions)
                    .drop_column(ActiveTasks::TaskSuiteId)
                    .modify_column(ColumnDef::new(ActiveTasks::Spec).json())
                    .modify_column(ColumnDef::new(ActiveTasks::Result).json())
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
    RunnerUuid,
    Timeout,
    AssignedWorker,
    Spec,
    Result,
}

#[derive(DeriveIden)]
enum TaskSuites {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum Workers {
    Table,
    Id,
    WorkerId,
}
