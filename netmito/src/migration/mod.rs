pub use sea_orm_migration::prelude::*;

mod m20240416_132735_create_table;
mod m20240424_055034_create_admin_user;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20240416_132735_create_table::Migration),
            Box::new(m20240424_055034_create_admin_user::Migration),
        ]
    }
}
