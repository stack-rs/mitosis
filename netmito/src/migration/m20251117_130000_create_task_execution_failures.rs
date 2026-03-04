use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(SuiteHookExecutions::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(SuiteHookExecutions::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(SuiteHookExecutions::TaskSuiteId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SuiteHookExecutions::AgentId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SuiteHookExecutions::HookType)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SuiteHookExecutions::Spec)
                            .json_binary()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SuiteHookExecutions::State)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(ColumnDef::new(SuiteHookExecutions::Result).json_binary())
                    .col(ColumnDef::new(SuiteHookExecutions::StartedAt).timestamp_with_time_zone())
                    .col(
                        ColumnDef::new(SuiteHookExecutions::CompletedAt)
                            .timestamp_with_time_zone(),
                    )
                    .col(
                        ColumnDef::new(SuiteHookExecutions::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(SuiteHookExecutions::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-suite_hook_executions-task_suite_id")
                            .from(
                                SuiteHookExecutions::Table,
                                SuiteHookExecutions::TaskSuiteId,
                            )
                            .to(TaskSuites::Table, TaskSuites::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-suite_hook_executions-agent_id")
                            .from(SuiteHookExecutions::Table, SuiteHookExecutions::AgentId)
                            .to(Agents::Table, Agents::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Index on (task_suite_id, agent_id) for lookup
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

        // Partial index for active (non-terminal) hook executions
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

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .name("idx_suite_hook_executions-active")
                    .to_owned(),
            )
            .await?;
        manager
            .drop_index(
                Index::drop()
                    .name("idx_suite_hook_executions-agent_id")
                    .to_owned(),
            )
            .await?;
        manager
            .drop_index(
                Index::drop()
                    .name("idx_suite_hook_executions-task_suite_id")
                    .to_owned(),
            )
            .await?;
        manager
            .drop_index(
                Index::drop()
                    .name("idx_suite_hook_executions-suite_agent")
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(
                Table::drop()
                    .table(SuiteHookExecutions::Table)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum SuiteHookExecutions {
    Table,
    Id,
    TaskSuiteId,
    AgentId,
    HookType,
    Spec,
    State,
    Result,
    StartedAt,
    CompletedAt,
    CreatedAt,
    UpdatedAt,
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
