use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(NodeManagers::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(NodeManagers::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(NodeManagers::Uuid)
                            .uuid()
                            .not_null()
                            .unique_key(),
                    )
                    .col(
                        ColumnDef::new(NodeManagers::CreatorId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(NodeManagers::Tags)
                            .array(ColumnType::Text)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(NodeManagers::Labels)
                            .array(ColumnType::Text)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(NodeManagers::State)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(NodeManagers::LastHeartbeat)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(ColumnDef::new(NodeManagers::AssignedTaskSuiteId).big_integer())
                    .col(ColumnDef::new(NodeManagers::LeaseExpiresAt).timestamp_with_time_zone())
                    .col(
                        ColumnDef::new(NodeManagers::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(NodeManagers::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-node_managers-creator_id")
                            .from(NodeManagers::Table, NodeManagers::CreatorId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Restrict)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-node_managers-assigned_task_suite_id")
                            .from(NodeManagers::Table, NodeManagers::AssignedTaskSuiteId)
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
                    .name("idx_node_managers-creator_id")
                    .table(NodeManagers::Table)
                    .col(NodeManagers::CreatorId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_node_managers-state")
                    .table(NodeManagers::Table)
                    .col(NodeManagers::State)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_node_managers-heartbeat")
                    .table(NodeManagers::Table)
                    .col(NodeManagers::LastHeartbeat)
                    .to_owned(),
            )
            .await?;

        // GIN indexes for tags and labels
        manager
            .create_index(
                sea_query::Index::create()
                    .if_not_exists()
                    .name("idx_node_managers-tags_gin")
                    .table(NodeManagers::Table)
                    .col(NodeManagers::Tags)
                    .full_text()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                sea_query::Index::create()
                    .if_not_exists()
                    .name("idx_node_managers-labels_gin")
                    .table(NodeManagers::Table)
                    .col(NodeManagers::Labels)
                    .full_text()
                    .to_owned(),
            )
            .await?;

        // Partial index for assigned managers
        manager
            .create_index(
                sea_query::Index::create()
                    .if_not_exists()
                    .name("idx_node_managers-assigned_task_suite")
                    .table(NodeManagers::Table)
                    .col(NodeManagers::AssignedTaskSuiteId)
                    .and_where(Expr::col(NodeManagers::AssignedTaskSuiteId).is_not_null())
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                sea_query::Index::drop()
                    .name("idx_node_managers-assigned_task_suite")
                    .to_owned(),
            )
            .await?;
        manager
            .drop_index(
                sea_query::Index::drop()
                    .name("idx_node_managers-labels_gin")
                    .to_owned(),
            )
            .await?;
        manager
            .drop_index(
                sea_query::Index::drop()
                    .name("idx_node_managers-tags_gin")
                    .to_owned(),
            )
            .await?;
        manager
            .drop_index(
                sea_query::Index::drop()
                    .name("idx_node_managers-heartbeat")
                    .to_owned(),
            )
            .await?;
        manager
            .drop_index(
                sea_query::Index::drop()
                    .name("idx_node_managers-state")
                    .to_owned(),
            )
            .await?;
        manager
            .drop_index(
                sea_query::Index::drop()
                    .name("idx_node_managers-creator_id")
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(Table::drop().table(NodeManagers::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum NodeManagers {
    Table,
    Id,
    Uuid,
    CreatorId,
    Tags,
    Labels,
    State,
    LastHeartbeat,
    AssignedTaskSuiteId,
    LeaseExpiresAt,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum Users {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum TaskSuites {
    Table,
    Id,
}
