use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Machines::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Machines::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Machines::MachineCode)
                            .text()
                            .not_null()
                            .unique_key(),
                    )
                    .col(ColumnDef::new(Machines::Metadata).json_binary())
                    .col(
                        ColumnDef::new(Machines::FirstSeenAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(Machines::LastSeenAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await?;

        // Index on machine_code for fast lookups
        manager
            .create_index(
                Index::create()
                    .name("idx_machines-machine_code")
                    .table(Machines::Table)
                    .col(Machines::MachineCode)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .name("idx_machines-machine_code")
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(Table::drop().table(Machines::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum Machines {
    Table,
    Id,
    MachineCode,
    Metadata,
    FirstSeenAt,
    LastSeenAt,
}
