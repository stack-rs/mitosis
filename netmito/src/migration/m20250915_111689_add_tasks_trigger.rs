use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(ActiveTasks::Table)
                    .add_column_if_not_exists(
                        ColumnDef::new(ActiveTasks::UpstreamTaskUuid).uuid().null(),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(ActiveTasks::Table)
                    .add_column_if_not_exists(
                        ColumnDef::new(ActiveTasks::DownstreamTaskUuid)
                            .uuid()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(ArchivedTasks::Table)
                    .add_column_if_not_exists(
                        ColumnDef::new(ArchivedTasks::UpstreamTaskUuid)
                            .uuid()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(ArchivedTasks::Table)
                    .add_column_if_not_exists(
                        ColumnDef::new(ArchivedTasks::DownstreamTaskUuid)
                            .uuid()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(ArchivedTasks::Table)
                    .drop_column(ArchivedTasks::DownstreamTaskUuid)
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(ArchivedTasks::Table)
                    .drop_column(ArchivedTasks::UpstreamTaskUuid)
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(ActiveTasks::Table)
                    .drop_column(ActiveTasks::DownstreamTaskUuid)
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(ActiveTasks::Table)
                    .drop_column(ActiveTasks::UpstreamTaskUuid)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

#[derive(Iden)]
enum ActiveTasks {
    Table,
    UpstreamTaskUuid,
    DownstreamTaskUuid,
}

#[derive(Iden)]
enum ArchivedTasks {
    Table,
    UpstreamTaskUuid,
    DownstreamTaskUuid,
}
