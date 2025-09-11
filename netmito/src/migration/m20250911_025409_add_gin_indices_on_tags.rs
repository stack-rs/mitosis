use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[derive(Iden)]
enum ActiveTasks {
    Table,
    Tags,
}

#[derive(Iden)]
enum Workers {
    Table,
    Tags,
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Create GIN index for active_tasks.tags to improve array contains/contained queries
        manager
            .create_index(
                sea_query::Index::create()
                    .if_not_exists()
                    .name("idx_active_tasks-tags_gin")
                    .table(ActiveTasks::Table)
                    .col(ActiveTasks::Tags)
                    .full_text()
                    .to_owned(),
            )
            .await?;

        // Create GIN index for workers.tags to improve array contains/contained queries
        manager
            .create_index(
                sea_query::Index::create()
                    .if_not_exists()
                    .name("idx_workers-tags_gin")
                    .table(Workers::Table)
                    .col(Workers::Tags)
                    .full_text()
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Drop all indices in reverse order
        let indices = ["idx_workers-tags_gin", "idx_active_tasks-tags_gin"];

        for index_name in indices.iter() {
            manager
                .drop_index(sea_query::Index::drop().name(*index_name).to_owned())
                .await
                .ok(); // Ignore errors for indices that might not exist
        }

        Ok(())
    }
}
