use sea_orm_migration::prelude::*;

use crate::{error::Error, service::user::create_user};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let admin_user_info = crate::config::INIT_ADMIN_USER.get().ok_or(DbErr::Custom(
            "default admin user config not found".to_string(),
        ))?;
        let db = manager.get_connection();
        let md5_password = md5::compute(admin_user_info.password.as_bytes()).0;
        let res = create_user(db, admin_user_info.username.clone(), md5_password, true).await;
        match res {
            Ok(_) => {
                tracing::info!(target: "netmito::migration", "Default admin user created");
                Ok(())
            }
            Err(e) => match e {
                Error::DbError(DbErr::RecordNotInserted) => {
                    tracing::info!(target: "netmito::migration", "Default admin user already exists, skip creation");
                    Ok(())
                }
                _ => Err(DbErr::Custom(format!(
                    "Error creating default admin user: {}",
                    e
                ))),
            },
        }
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // We are not going to support this migration
        Ok(())
    }
}
