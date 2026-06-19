use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // One row per (hook execution, content type). Hook logs live in S3 under
        // `hooks/{suite_hook_execution_id}/{content_type}`; this row is the
        // metadata + size that drives quota refunds. Cascade-deletes with its
        // hook execution (and thus with the run). See
        // docs/plans/2026-06-15-run-interaction-design.md (A.3.5).
        manager
            .create_table(
                Table::create()
                    .table(SuiteHookArtifacts::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(SuiteHookArtifacts::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(SuiteHookArtifacts::SuiteHookExecutionId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SuiteHookArtifacts::ContentType)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SuiteHookArtifacts::Size)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SuiteHookArtifacts::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(SuiteHookArtifacts::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-suite_hook_artifacts-suite_hook_execution_id")
                            .from(
                                SuiteHookArtifacts::Table,
                                SuiteHookArtifacts::SuiteHookExecutionId,
                            )
                            .to(SuiteHookExecutions::Table, SuiteHookExecutions::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // At most one artifact per content type per hook execution; also the
        // upsert key for re-uploads.
        manager
            .create_index(
                Index::create()
                    .unique()
                    .name("idx_suite_hook_artifacts-exec_content_type")
                    .table(SuiteHookArtifacts::Table)
                    .col(SuiteHookArtifacts::SuiteHookExecutionId)
                    .col(SuiteHookArtifacts::ContentType)
                    .to_owned(),
            )
            .await?;

        // Enforce "one hook execution per (run, hook_type)": a run has exactly
        // one provision/cleanup/background hook. This is the upsert key for a
        // re-reported `Result` (the agent-facing endpoint overwrites on
        // conflict). Safe to add now — the table is empty (no production code
        // writes it yet; the suite_agent_runs migration cleared it).
        manager
            .create_index(
                Index::create()
                    .unique()
                    .name("idx_suite_hook_executions-run_hook_type")
                    .table(SuiteHookExecutions::Table)
                    .col(SuiteHookExecutions::SuiteAgentRunId)
                    .col(SuiteHookExecutions::HookType)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .name("idx_suite_hook_executions-run_hook_type")
                    .table(SuiteHookExecutions::Table)
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(
                Table::drop()
                    .table(SuiteHookArtifacts::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum SuiteHookArtifacts {
    Table,
    Id,
    SuiteHookExecutionId,
    ContentType,
    Size,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum SuiteHookExecutions {
    Table,
    Id,
    SuiteAgentRunId,
    HookType,
}
