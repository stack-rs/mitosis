//! ALTER migration linking existing tasks to task suites.
//!
//! Adds a new task_suite_id col for active_tasks and archived_tasks
//! and set the task_suite_id of existing tasks to null.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for tbl in [Tasks::ActiveTasks, Tasks::ArchivedTasks] {
            manager
                .alter_table(
                    Table::alter()
                        .table(tbl)
                        .add_column_if_not_exists(ColumnDef::new(Tasks::TaskSuiteId).big_integer())
                        .to_owned(),
                )
                .await?;
            manager
                .alter_table(
                    Table::alter()
                        .table(tbl)
                        .add_foreign_key(
                            TableForeignKey::new()
                                .name(match tbl {
                                    Tasks::ActiveTasks => "fk-active_tasks-task_suite_id",
                                    _ => "fk-archived_tasks-task_suite_id",
                                })
                                .from_tbl(tbl)
                                .from_col(Tasks::TaskSuiteId)
                                .to_tbl(TaskSuites::Table)
                                .to_col(TaskSuites::Id)
                                .on_delete(ForeignKeyAction::SetNull)
                                .on_update(ForeignKeyAction::Cascade),
                        )
                        .to_owned(),
                )
                .await?;
            manager
                .create_index(
                    Index::create()
                        .if_not_exists()
                        .name(match tbl {
                            Tasks::ActiveTasks => "idx_active_tasks-task_suite_id",
                            _ => "idx_archived_tasks-task_suite_id",
                        })
                        .table(tbl)
                        .col(Tasks::TaskSuiteId)
                        .and_where(Expr::col(Tasks::TaskSuiteId).is_not_null())
                        .to_owned(),
                )
                .await?;
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for (tbl, fk, idx) in [
            (
                Tasks::ActiveTasks,
                "fk-active_tasks-task_suite_id",
                "idx_active_tasks-task_suite_id",
            ),
            (
                Tasks::ArchivedTasks,
                "fk-archived_tasks-task_suite_id",
                "idx_archived_tasks-task_suite_id",
            ),
        ] {
            manager
                .drop_index(Index::drop().name(idx).table(tbl).to_owned())
                .await?;
            manager
                .alter_table(
                    Table::alter()
                        .table(tbl)
                        .drop_foreign_key(Alias::new(fk))
                        .to_owned(),
                )
                .await?;
            manager
                .alter_table(
                    Table::alter()
                        .table(tbl)
                        .drop_column(Tasks::TaskSuiteId)
                        .to_owned(),
                )
                .await?;
        }

        Ok(())
    }
}

/// Existing task tables, referenced only to add `task_suite_id`. Variant names
/// map to the `active_tasks` / `archived_tasks` table names.
#[derive(DeriveIden, Clone, Copy)]
#[allow(clippy::enum_variant_names)]
enum Tasks {
    ActiveTasks,
    ArchivedTasks,
    TaskSuiteId,
}

#[derive(DeriveIden)]
enum TaskSuites {
    Table,
    Id,
}
