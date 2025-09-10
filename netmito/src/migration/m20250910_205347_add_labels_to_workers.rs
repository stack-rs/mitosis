use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Workers::Table)
                    .add_column_if_not_exists(
                        ColumnDef::new(Workers::Labels)
                            .array(ColumnType::Text)
                            .not_null()
                            .default(Expr::val("{}")),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Workers::Table)
                    .drop_column(Workers::Labels)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum Workers {
    Table,
    Labels,
}
