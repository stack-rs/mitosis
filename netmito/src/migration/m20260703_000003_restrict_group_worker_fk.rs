//! ALTER migration tightening the `group_worker` → `workers` foreign key.
//!
//! Re-points `fk-group_worker-worker_id` from `on_delete=Cascade` to
//! `on_delete=Restrict`, matching the other group→resource role tables
//! (`user_group`, `group_agent`, `task_suite_agent`), which all restrict on
//! both sides.
//!
//! Every worker-deletion path already deletes the `group_worker` rows in the
//! same transaction before deleting the worker, so the cascade never fired
//! from application code. This only changes manual deletes over a direct DB
//! connection, which must now clear the referencing rows first.
//!
//! Postgres cannot alter a constraint's action in place, so both directions
//! drop the FK and recreate it; only `on_delete` differs.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(GroupWorker::Table)
                    .drop_foreign_key(Alias::new("fk-group_worker-worker_id"))
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(GroupWorker::Table)
                    .add_foreign_key(
                        TableForeignKey::new()
                            .name("fk-group_worker-worker_id")
                            .from_tbl(GroupWorker::Table)
                            .from_col(GroupWorker::WorkerId)
                            .to_tbl(Workers::Table)
                            .to_col(Workers::Id)
                            .on_delete(ForeignKeyAction::Restrict)
                            .on_update(ForeignKeyAction::Cascade),
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
                    .table(GroupWorker::Table)
                    .drop_foreign_key(Alias::new("fk-group_worker-worker_id"))
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(GroupWorker::Table)
                    .add_foreign_key(
                        TableForeignKey::new()
                            .name("fk-group_worker-worker_id")
                            .from_tbl(GroupWorker::Table)
                            .from_col(GroupWorker::WorkerId)
                            .to_tbl(Workers::Table)
                            .to_col(Workers::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum GroupWorker {
    Table,
    WorkerId,
}

#[derive(DeriveIden)]
enum Workers {
    Table,
    Id,
}
