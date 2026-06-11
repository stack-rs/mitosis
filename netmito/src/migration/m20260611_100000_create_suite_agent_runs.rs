use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // ────────────────────────────────────────────────────────────────
        // 1. Create suite_agent_runs: one row per attempt of an agent
        //    running a task suite.
        //    See docs/plans/2026-06-10-suite-agent-run-design.md.
        // ────────────────────────────────────────────────────────────────
        manager
            .create_table(
                Table::create()
                    .table(SuiteAgentRuns::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(SuiteAgentRuns::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(SuiteAgentRuns::TaskSuiteId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(SuiteAgentRuns::RunId).integer().not_null())
                    // Nullable: SetNull when the agent row is removed
                    .col(ColumnDef::new(SuiteAgentRuns::AgentId).big_integer())
                    // Denormalized registration identity; survives agent deletion
                    .col(ColumnDef::new(SuiteAgentRuns::AgentUuid).uuid().not_null())
                    // machine_code is mandatory at registration, so every agent
                    // has a real machines row to copy from at accept time
                    .col(
                        ColumnDef::new(SuiteAgentRuns::MachineId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SuiteAgentRuns::State)
                            .integer()
                            .not_null()
                            .default(0), // Provision
                    )
                    .col(
                        ColumnDef::new(SuiteAgentRuns::TasksCompleted)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(SuiteAgentRuns::TasksFailed)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(ColumnDef::new(SuiteAgentRuns::FailureReason).json_binary())
                    .col(
                        ColumnDef::new(SuiteAgentRuns::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(ColumnDef::new(SuiteAgentRuns::StartedAt).timestamp_with_time_zone())
                    .col(ColumnDef::new(SuiteAgentRuns::FinishedAt).timestamp_with_time_zone())
                    .col(
                        ColumnDef::new(SuiteAgentRuns::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-suite_agent_runs-task_suite_id")
                            .from(SuiteAgentRuns::Table, SuiteAgentRuns::TaskSuiteId)
                            .to(TaskSuites::Table, TaskSuites::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-suite_agent_runs-agent_id")
                            .from(SuiteAgentRuns::Table, SuiteAgentRuns::AgentId)
                            .to(Agents::Table, Agents::Id)
                            .on_delete(ForeignKeyAction::SetNull)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-suite_agent_runs-machine_id")
                            .from(SuiteAgentRuns::Table, SuiteAgentRuns::MachineId)
                            .to(Machines::Table, Machines::Id)
                            .on_delete(ForeignKeyAction::Restrict)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // User-facing key: run_id is ascending per suite (across agents).
        // The unique index is the safety net for max(run_id)+1 allocation.
        manager
            .create_index(
                Index::create()
                    .unique()
                    .name("idx_suite_agent_runs-suite_run")
                    .table(SuiteAgentRuns::Table)
                    .col(SuiteAgentRuns::TaskSuiteId)
                    .col(SuiteAgentRuns::RunId)
                    .to_owned(),
            )
            .await?;

        // Serves state-filtered queries and TTL sweeps over terminal runs
        manager
            .create_index(
                Index::create()
                    .name("idx_suite_agent_runs-state_finished")
                    .table(SuiteAgentRuns::Table)
                    .col(SuiteAgentRuns::State)
                    .col(SuiteAgentRuns::FinishedAt)
                    .to_owned(),
            )
            .await?;

        // "All runs by this registration" lookups, alive or not
        manager
            .create_index(
                Index::create()
                    .name("idx_suite_agent_runs-agent_uuid")
                    .table(SuiteAgentRuns::Table)
                    .col(SuiteAgentRuns::AgentUuid)
                    .to_owned(),
            )
            .await?;

        // ────────────────────────────────────────────────────────────────
        // 2. Repoint suite_hook_executions: (task_suite_id, agent_id) →
        //    suite_agent_run_id.
        //    Existing rows cannot be attributed to a run (the runs table
        //    did not exist when they were written) and no production code
        //    writes this table yet, so they are dropped.
        // ────────────────────────────────────────────────────────────────
        manager
            .get_connection()
            .execute_unprepared("DELETE FROM suite_hook_executions")
            .await?;

        // Drop old indexes
        for idx in [
            "idx_suite_hook_executions-active",
            "idx_suite_hook_executions-agent_id",
            "idx_suite_hook_executions-task_suite_id",
            "idx_suite_hook_executions-suite_agent",
        ] {
            manager
                .drop_index(
                    Index::drop()
                        .name(idx)
                        .table(SuiteHookExecutions::Table)
                        .to_owned(),
                )
                .await?;
        }

        // Drop old FKs and columns, add the run FK column
        manager
            .alter_table(
                Table::alter()
                    .table(SuiteHookExecutions::Table)
                    .drop_foreign_key(Alias::new("fk-suite_hook_executions-task_suite_id"))
                    .drop_foreign_key(Alias::new("fk-suite_hook_executions-agent_id"))
                    .drop_column(SuiteHookExecutions::TaskSuiteId)
                    .drop_column(SuiteHookExecutions::AgentId)
                    .add_column(
                        ColumnDef::new(SuiteHookExecutions::SuiteAgentRunId)
                            .big_integer()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(SuiteHookExecutions::Table)
                    .add_foreign_key(
                        TableForeignKey::new()
                            .name("fk-suite_hook_executions-suite_agent_run_id")
                            .from_tbl(SuiteHookExecutions::Table)
                            .from_col(SuiteHookExecutions::SuiteAgentRunId)
                            .to_tbl(SuiteAgentRuns::Table)
                            .to_col(SuiteAgentRuns::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_suite_hook_executions-run_id")
                    .table(SuiteHookExecutions::Table)
                    .col(SuiteHookExecutions::SuiteAgentRunId)
                    .to_owned(),
            )
            .await?;

        // Partial index for active (non-terminal) hook executions
        manager
            .create_index(
                Index::create()
                    .name("idx_suite_hook_executions-active")
                    .table(SuiteHookExecutions::Table)
                    .col(SuiteHookExecutions::SuiteAgentRunId)
                    .col(SuiteHookExecutions::HookType)
                    .and_where(Expr::col(SuiteHookExecutions::State).eq(0)) // Running
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Revert suite_hook_executions to (task_suite_id, agent_id).
        // Rows written against runs cannot be mapped back, so they are
        // dropped (mirrors the data reset in `up`).
        manager
            .get_connection()
            .execute_unprepared("DELETE FROM suite_hook_executions")
            .await?;

        for idx in [
            "idx_suite_hook_executions-active",
            "idx_suite_hook_executions-run_id",
        ] {
            manager
                .drop_index(
                    Index::drop()
                        .name(idx)
                        .table(SuiteHookExecutions::Table)
                        .to_owned(),
                )
                .await?;
        }

        manager
            .alter_table(
                Table::alter()
                    .table(SuiteHookExecutions::Table)
                    .drop_foreign_key(Alias::new("fk-suite_hook_executions-suite_agent_run_id"))
                    .drop_column(SuiteHookExecutions::SuiteAgentRunId)
                    .add_column(
                        ColumnDef::new(SuiteHookExecutions::TaskSuiteId)
                            .big_integer()
                            .not_null(),
                    )
                    .add_column(
                        ColumnDef::new(SuiteHookExecutions::AgentId)
                            .big_integer()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(SuiteHookExecutions::Table)
                    .add_foreign_key(
                        TableForeignKey::new()
                            .name("fk-suite_hook_executions-task_suite_id")
                            .from_tbl(SuiteHookExecutions::Table)
                            .from_col(SuiteHookExecutions::TaskSuiteId)
                            .to_tbl(TaskSuites::Table)
                            .to_col(TaskSuites::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .add_foreign_key(
                        TableForeignKey::new()
                            .name("fk-suite_hook_executions-agent_id")
                            .from_tbl(SuiteHookExecutions::Table)
                            .from_col(SuiteHookExecutions::AgentId)
                            .to_tbl(Agents::Table)
                            .to_col(Agents::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_suite_hook_executions-suite_agent")
                    .table(SuiteHookExecutions::Table)
                    .col(SuiteHookExecutions::TaskSuiteId)
                    .col(SuiteHookExecutions::AgentId)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_suite_hook_executions-task_suite_id")
                    .table(SuiteHookExecutions::Table)
                    .col(SuiteHookExecutions::TaskSuiteId)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_suite_hook_executions-agent_id")
                    .table(SuiteHookExecutions::Table)
                    .col(SuiteHookExecutions::AgentId)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_suite_hook_executions-active")
                    .table(SuiteHookExecutions::Table)
                    .col(SuiteHookExecutions::TaskSuiteId)
                    .col(SuiteHookExecutions::AgentId)
                    .col(SuiteHookExecutions::HookType)
                    .and_where(Expr::col(SuiteHookExecutions::State).eq(0)) // Running
                    .to_owned(),
            )
            .await?;

        // Drop suite_agent_runs (indexes go with the table)
        manager
            .drop_table(
                Table::drop()
                    .table(SuiteAgentRuns::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum SuiteAgentRuns {
    Table,
    Id,
    TaskSuiteId,
    RunId,
    AgentId,
    AgentUuid,
    MachineId,
    State,
    TasksCompleted,
    TasksFailed,
    FailureReason,
    CreatedAt,
    StartedAt,
    FinishedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum SuiteHookExecutions {
    Table,
    TaskSuiteId,
    AgentId,
    SuiteAgentRunId,
    HookType,
    State,
}

#[derive(DeriveIden)]
enum TaskSuites {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum Agents {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum Machines {
    Table,
    Id,
}
