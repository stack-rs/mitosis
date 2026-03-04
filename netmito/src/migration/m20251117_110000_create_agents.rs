use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Agents::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Agents::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Agents::Uuid).uuid().not_null().unique_key())
                    .col(ColumnDef::new(Agents::CreatorId).big_integer().not_null())
                    .col(ColumnDef::new(Agents::MachineId).big_integer())
                    .col(
                        ColumnDef::new(Agents::Tags)
                            .array(ColumnType::Text)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Agents::Labels)
                            .array(ColumnType::Text)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Agents::State)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(Agents::LastHeartbeat)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(ColumnDef::new(Agents::AssignedTaskSuiteId).big_integer())
                    .col(
                        ColumnDef::new(Agents::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(Agents::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-agents-creator_id")
                            .from(Agents::Table, Agents::CreatorId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Restrict)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-agents-machine_id")
                            .from(Agents::Table, Agents::MachineId)
                            .to(Machines::Table, Machines::Id)
                            .on_delete(ForeignKeyAction::SetNull)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-agents-assigned_task_suite_id")
                            .from(Agents::Table, Agents::AssignedTaskSuiteId)
                            .to(TaskSuites::Table, TaskSuites::Id)
                            .on_delete(ForeignKeyAction::SetNull)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Create indexes
        manager
            .create_index(
                Index::create()
                    .name("idx_agents-creator_id")
                    .table(Agents::Table)
                    .col(Agents::CreatorId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_agents-state")
                    .table(Agents::Table)
                    .col(Agents::State)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_agents-heartbeat")
                    .table(Agents::Table)
                    .col(Agents::LastHeartbeat)
                    .to_owned(),
            )
            .await?;

        // GIN indexes for tags and labels
        manager
            .create_index(
                sea_query::Index::create()
                    .if_not_exists()
                    .name("idx_agents-tags_gin")
                    .table(Agents::Table)
                    .col(Agents::Tags)
                    .full_text()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                sea_query::Index::create()
                    .if_not_exists()
                    .name("idx_agents-labels_gin")
                    .table(Agents::Table)
                    .col(Agents::Labels)
                    .full_text()
                    .to_owned(),
            )
            .await?;

        // Partial index for assigned managers
        manager
            .create_index(
                sea_query::Index::create()
                    .if_not_exists()
                    .name("idx_agents-assigned_task_suite")
                    .table(Agents::Table)
                    .col(Agents::AssignedTaskSuiteId)
                    .and_where(Expr::col(Agents::AssignedTaskSuiteId).is_not_null())
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                sea_query::Index::drop()
                    .name("idx_agents-assigned_task_suite")
                    .to_owned(),
            )
            .await?;
        manager
            .drop_index(
                sea_query::Index::drop()
                    .name("idx_agents-labels_gin")
                    .to_owned(),
            )
            .await?;
        manager
            .drop_index(
                sea_query::Index::drop()
                    .name("idx_agents-tags_gin")
                    .to_owned(),
            )
            .await?;
        manager
            .drop_index(
                sea_query::Index::drop()
                    .name("idx_agents-heartbeat")
                    .to_owned(),
            )
            .await?;
        manager
            .drop_index(sea_query::Index::drop().name("idx_agents-state").to_owned())
            .await?;
        manager
            .drop_index(
                sea_query::Index::drop()
                    .name("idx_agents-creator_id")
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(Table::drop().table(Agents::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum Agents {
    Table,
    Id,
    Uuid,
    CreatorId,
    MachineId,
    Tags,
    Labels,
    State,
    LastHeartbeat,
    AssignedTaskSuiteId,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum Users {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum Machines {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum TaskSuites {
    Table,
    Id,
}
