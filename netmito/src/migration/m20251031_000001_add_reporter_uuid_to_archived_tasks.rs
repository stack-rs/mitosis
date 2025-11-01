use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(ArchivedTasks::Table)
                    .add_column_if_not_exists(
                        ColumnDef::new(ArchivedTasks::ReporterUuid).uuid().null(),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                sea_query::Index::create()
                    .if_not_exists()
                    .name("idx_archived_tasks-reporter_uuid")
                    .table(ArchivedTasks::Table)
                    .col(ArchivedTasks::ReporterUuid)
                    .and_where(Expr::col(ArchivedTasks::ReporterUuid).is_not_null())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                sea_query::Index::drop()
                    .name("idx_archived_tasks-reporter_uuid")
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(ArchivedTasks::Table)
                    .drop_column(ArchivedTasks::ReporterUuid)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum ArchivedTasks {
    Table,
    ReporterUuid,
}
