use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Create group_node_manager table
        manager
            .create_table(
                Table::create()
                    .table(GroupNodeManager::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(GroupNodeManager::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(GroupNodeManager::GroupId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(GroupNodeManager::ManagerId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(GroupNodeManager::Role).integer().not_null())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-group_node_manager-group_id")
                            .from(GroupNodeManager::Table, GroupNodeManager::GroupId)
                            .to(Groups::Table, Groups::Id)
                            .on_delete(ForeignKeyAction::Restrict)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-group_node_manager-manager_id")
                            .from(GroupNodeManager::Table, GroupNodeManager::ManagerId)
                            .to(NodeManagers::Table, NodeManagers::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Unique constraint on (group_id, manager_id)
        manager
            .create_index(
                Index::create()
                    .name("idx_group_node_manager-group_id-manager_id")
                    .table(GroupNodeManager::Table)
                    .col(GroupNodeManager::GroupId)
                    .col(GroupNodeManager::ManagerId)
                    .unique()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_group_node_manager-group_id")
                    .table(GroupNodeManager::Table)
                    .col(GroupNodeManager::GroupId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_group_node_manager-manager_id")
                    .table(GroupNodeManager::Table)
                    .col(GroupNodeManager::ManagerId)
                    .to_owned(),
            )
            .await?;

        // Create task_suite_node_manager table
        manager
            .create_table(
                Table::create()
                    .table(TaskSuiteNodeManager::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(TaskSuiteNodeManager::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(TaskSuiteNodeManager::TaskSuiteId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(TaskSuiteNodeManager::ManagerId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(TaskSuiteNodeManager::SelectionType)
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(TaskSuiteNodeManager::MatchedTags).array(ColumnType::Text))
                    .col(
                        ColumnDef::new(TaskSuiteNodeManager::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(ColumnDef::new(TaskSuiteNodeManager::CreatorId).big_integer())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-task_suite_node_manager-task_suite_id")
                            .from(
                                TaskSuiteNodeManager::Table,
                                TaskSuiteNodeManager::TaskSuiteId,
                            )
                            .to(TaskSuites::Table, TaskSuites::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-task_suite_node_manager-manager_id")
                            .from(TaskSuiteNodeManager::Table, TaskSuiteNodeManager::ManagerId)
                            .to(NodeManagers::Table, NodeManagers::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-task_suite_node_manager-creator_id")
                            .from(TaskSuiteNodeManager::Table, TaskSuiteNodeManager::CreatorId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::SetNull)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Unique constraint on (task_suite_id, manager_id)
        manager
            .create_index(
                Index::create()
                    .name("idx_task_suite_node_manager-task_suite_id-manager_id")
                    .table(TaskSuiteNodeManager::Table)
                    .col(TaskSuiteNodeManager::TaskSuiteId)
                    .col(TaskSuiteNodeManager::ManagerId)
                    .unique()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_task_suite_node_manager-task_suite_id")
                    .table(TaskSuiteNodeManager::Table)
                    .col(TaskSuiteNodeManager::TaskSuiteId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_task_suite_node_manager-manager_id")
                    .table(TaskSuiteNodeManager::Table)
                    .col(TaskSuiteNodeManager::ManagerId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_task_suite_node_manager-selection_type")
                    .table(TaskSuiteNodeManager::Table)
                    .col(TaskSuiteNodeManager::SelectionType)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                sea_query::Index::drop()
                    .name("idx_task_suite_node_manager-selection_type")
                    .to_owned(),
            )
            .await?;
        manager
            .drop_index(
                sea_query::Index::drop()
                    .name("idx_task_suite_node_manager-manager_id")
                    .to_owned(),
            )
            .await?;
        manager
            .drop_index(
                sea_query::Index::drop()
                    .name("idx_task_suite_node_manager-task_suite_id")
                    .to_owned(),
            )
            .await?;
        manager
            .drop_index(
                sea_query::Index::drop()
                    .name("idx_task_suite_node_manager-task_suite_id-manager_id")
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(Table::drop().table(TaskSuiteNodeManager::Table).to_owned())
            .await?;

        manager
            .drop_index(
                sea_query::Index::drop()
                    .name("idx_group_node_manager-manager_id")
                    .to_owned(),
            )
            .await?;
        manager
            .drop_index(
                sea_query::Index::drop()
                    .name("idx_group_node_manager-group_id")
                    .to_owned(),
            )
            .await?;
        manager
            .drop_index(
                sea_query::Index::drop()
                    .name("idx_group_node_manager-group_id-manager_id")
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(Table::drop().table(GroupNodeManager::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum GroupNodeManager {
    Table,
    Id,
    GroupId,
    ManagerId,
    Role,
}

#[derive(DeriveIden)]
enum TaskSuiteNodeManager {
    Table,
    Id,
    TaskSuiteId,
    ManagerId,
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
enum NodeManagers {
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
