use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Create group_agent table
        manager
            .create_table(
                Table::create()
                    .table(GroupAgent::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(GroupAgent::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(GroupAgent::GroupId).big_integer().not_null())
                    .col(ColumnDef::new(GroupAgent::AgentId).big_integer().not_null())
                    .col(ColumnDef::new(GroupAgent::Role).integer().not_null())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-group_agent-group_id")
                            .from(GroupAgent::Table, GroupAgent::GroupId)
                            .to(Groups::Table, Groups::Id)
                            .on_delete(ForeignKeyAction::Restrict)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-group_agent-agent_id")
                            .from(GroupAgent::Table, GroupAgent::AgentId)
                            .to(Agents::Table, Agents::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Unique constraint on (group_id, agent_id)
        manager
            .create_index(
                Index::create()
                    .name("idx_group_agent-group_id-agent_id")
                    .table(GroupAgent::Table)
                    .col(GroupAgent::GroupId)
                    .col(GroupAgent::AgentId)
                    .unique()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_group_agent-group_id")
                    .table(GroupAgent::Table)
                    .col(GroupAgent::GroupId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_group_agent-agent_id")
                    .table(GroupAgent::Table)
                    .col(GroupAgent::AgentId)
                    .to_owned(),
            )
            .await?;

        // Create task_suite_agent table
        manager
            .create_table(
                Table::create()
                    .table(TaskSuiteAgent::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(TaskSuiteAgent::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(TaskSuiteAgent::TaskSuiteId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(TaskSuiteAgent::AgentId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(TaskSuiteAgent::SelectionType)
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(TaskSuiteAgent::MatchedTags).array(ColumnType::Text))
                    .col(
                        ColumnDef::new(TaskSuiteAgent::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(ColumnDef::new(TaskSuiteAgent::CreatorId).big_integer())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-task_suite_agent-task_suite_id")
                            .from(TaskSuiteAgent::Table, TaskSuiteAgent::TaskSuiteId)
                            .to(TaskSuites::Table, TaskSuites::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-task_suite_agent-agent_id")
                            .from(TaskSuiteAgent::Table, TaskSuiteAgent::AgentId)
                            .to(Agents::Table, Agents::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-task_suite_agent-creator_id")
                            .from(TaskSuiteAgent::Table, TaskSuiteAgent::CreatorId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::SetNull)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Unique constraint on (task_suite_id, agent_id)
        manager
            .create_index(
                Index::create()
                    .name("idx_task_suite_agent-task_suite_id-agent_id")
                    .table(TaskSuiteAgent::Table)
                    .col(TaskSuiteAgent::TaskSuiteId)
                    .col(TaskSuiteAgent::AgentId)
                    .unique()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_task_suite_agent-task_suite_id")
                    .table(TaskSuiteAgent::Table)
                    .col(TaskSuiteAgent::TaskSuiteId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_task_suite_agent-agent_id")
                    .table(TaskSuiteAgent::Table)
                    .col(TaskSuiteAgent::AgentId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_task_suite_agent-selection_type")
                    .table(TaskSuiteAgent::Table)
                    .col(TaskSuiteAgent::SelectionType)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                sea_query::Index::drop()
                    .name("idx_task_suite_agent-selection_type")
                    .to_owned(),
            )
            .await?;
        manager
            .drop_index(
                sea_query::Index::drop()
                    .name("idx_task_suite_agent-agent_id")
                    .to_owned(),
            )
            .await?;
        manager
            .drop_index(
                sea_query::Index::drop()
                    .name("idx_task_suite_agent-task_suite_id")
                    .to_owned(),
            )
            .await?;
        manager
            .drop_index(
                sea_query::Index::drop()
                    .name("idx_task_suite_agent-task_suite_id-agent_id")
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(Table::drop().table(TaskSuiteAgent::Table).to_owned())
            .await?;

        manager
            .drop_index(
                sea_query::Index::drop()
                    .name("idx_group_agent-agent_id")
                    .to_owned(),
            )
            .await?;
        manager
            .drop_index(
                sea_query::Index::drop()
                    .name("idx_group_agent-group_id")
                    .to_owned(),
            )
            .await?;
        manager
            .drop_index(
                sea_query::Index::drop()
                    .name("idx_group_agent-group_id-agent_id")
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(Table::drop().table(GroupAgent::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum GroupAgent {
    Table,
    Id,
    GroupId,
    AgentId,
    Role,
}

#[derive(DeriveIden)]
enum TaskSuiteAgent {
    Table,
    Id,
    TaskSuiteId,
    AgentId,
    SelectionType,
    MatchedTags,
    CreatedAt,
    CreatorId,
}

#[derive(DeriveIden)]
enum Groups {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum Agents {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum TaskSuites {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum Users {
    Table,
    Id,
}
