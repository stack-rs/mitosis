pub use sea_orm_migration::prelude::*;

mod m20240416_132735_create_table;
mod m20240424_055034_create_admin_user;
mod m20250910_192001_add_performance_indices;
mod m20250910_205347_add_labels_to_workers;
mod m20250911_025409_add_gin_indices_on_tags;
mod m20250915_111689_add_tasks_trigger;
mod m20251031_000001_add_reporter_uuid_to_archived_tasks;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20240416_132735_create_table::Migration),
            Box::new(m20240424_055034_create_admin_user::Migration),
            Box::new(m20250910_192001_add_performance_indices::Migration),
            Box::new(m20250910_205347_add_labels_to_workers::Migration),
            Box::new(m20250911_025409_add_gin_indices_on_tags::Migration),
            Box::new(m20250915_111689_add_tasks_trigger::Migration),
            Box::new(m20251031_000001_add_reporter_uuid_to_archived_tasks::Migration),
        ]
    }
}
