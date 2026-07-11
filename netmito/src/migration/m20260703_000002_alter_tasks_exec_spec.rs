//! Exec-spec refactor of the task tables (`active_tasks`, `archived_tasks`).
//!

use sea_orm_migration::prelude::*;
use uuid::Uuid;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // ── active_tasks ──
        manager
            .alter_table(
                Table::alter()
                    .table(ActiveTasks::Table)
                    .modify_column(ColumnDef::new(ActiveTasks::Spec).json_binary())
                    .modify_column(ColumnDef::new(ActiveTasks::Result).json_binary())
                    .add_column_if_not_exists(
                        ColumnDef::new(ActiveTasks::ExecOptions).json_binary(),
                    )
                    .add_column_if_not_exists(ColumnDef::new(ActiveTasks::RunnerUuid).uuid())
                    .to_owned(),
            )
            .await?;
        // Fold timeout into spec, lift watch out of spec into exec_options.
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
        // Map assigned_worker (row id) → runner_uuid (worker uuid); nil if the
        // worker row is already gone.
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
        manager
            .alter_table(
                Table::alter()
                    .table(ActiveTasks::Table)
                    .drop_column(ActiveTasks::Timeout)
                    .drop_column(ActiveTasks::AssignedWorker)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_active_tasks-runner_uuid")
                    .table(ActiveTasks::Table)
                    .col(ActiveTasks::RunnerUuid)
                    .and_where(Expr::col(ActiveTasks::RunnerUuid).is_not_null())
                    .to_owned(),
            )
            .await?;

        // ── archived_tasks ──
        manager
            .alter_table(
                Table::alter()
                    .table(ArchivedTasks::Table)
                    .modify_column(ColumnDef::new(ArchivedTasks::Spec).json_binary())
                    .modify_column(ColumnDef::new(ArchivedTasks::Result).json_binary())
                    .add_column_if_not_exists(
                        ColumnDef::new(ArchivedTasks::ExecOptions).json_binary(),
                    )
                    .add_column_if_not_exists(ColumnDef::new(ArchivedTasks::RunnerUuid).uuid())
                    .to_owned(),
            )
            .await?;
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
        // Archived rows already record the finishing worker in reporter_uuid; prefer
        // it, else fall back to the assigned_worker→workers lookup.
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
        // reporter_uuid's partial index must go before the column is dropped.
        manager
            .drop_index(
                Index::drop()
                    .name("idx_archived_tasks-reporter_uuid")
                    .table(ArchivedTasks::Table)
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
        manager
            .create_index(
                Index::create()
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
        // ── active_tasks ──
        manager
            .drop_index(
                Index::drop()
                    .name("idx_active_tasks-runner_uuid")
                    .table(ActiveTasks::Table)
                    .to_owned(),
            )
            .await?;
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
        // Restore timeout from spec and watch from exec_options; assigned_worker is
        // not reconstructed (runner_uuid → row id is not reversible).
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
        manager
            .alter_table(
                Table::alter()
                    .table(ActiveTasks::Table)
                    .drop_column(ActiveTasks::RunnerUuid)
                    .drop_column(ActiveTasks::ExecOptions)
                    .modify_column(ColumnDef::new(ActiveTasks::Spec).json())
                    .modify_column(ColumnDef::new(ActiveTasks::Result).json())
                    .to_owned(),
            )
            .await?;

        // ── archived_tasks ──
        manager
            .drop_index(
                Index::drop()
                    .name("idx_archived_tasks-runner_uuid")
                    .table(ArchivedTasks::Table)
                    .to_owned(),
            )
            .await?;
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
        // Recover reporter_uuid from runner_uuid (archived runner == the reporter).
        manager
            .get_connection()
            .execute_unprepared("UPDATE archived_tasks SET reporter_uuid = runner_uuid")
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_archived_tasks-reporter_uuid")
                    .table(ArchivedTasks::Table)
                    .col(ArchivedTasks::ReporterUuid)
                    .and_where(Expr::col(ArchivedTasks::ReporterUuid).is_not_null())
                    .to_owned(),
            )
            .await?;
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
        manager
            .alter_table(
                Table::alter()
                    .table(ArchivedTasks::Table)
                    .drop_column(ArchivedTasks::RunnerUuid)
                    .drop_column(ArchivedTasks::ExecOptions)
                    .modify_column(ColumnDef::new(ArchivedTasks::Spec).json())
                    .modify_column(ColumnDef::new(ArchivedTasks::Result).json())
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum ActiveTasks {
    Table,
    Spec,
    Result,
    ExecOptions,
    RunnerUuid,
    Timeout,
    AssignedWorker,
}

#[derive(DeriveIden)]
enum ArchivedTasks {
    Table,
    Spec,
    Result,
    ExecOptions,
    RunnerUuid,
    Timeout,
    AssignedWorker,
    ReporterUuid,
}

#[derive(DeriveIden)]
enum Workers {
    Table,
    Id,
    WorkerId,
}
