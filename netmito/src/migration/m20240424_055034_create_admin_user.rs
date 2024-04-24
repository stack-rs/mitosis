use argon2::{
    password_hash::{rand_core::OsRng, PasswordHasher, SaltString},
    Argon2,
};
use sea_orm::prelude::*;
use sea_orm::ActiveValue::Set;
use sea_orm_migration::prelude::*;

use crate::entity::users as User;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let admin_user_info = crate::config::InitAdminUser.get().ok_or(DbErr::Custom(
            "default admin user config not found".to_string(),
        ))?;
        let db = manager.get_connection();
        // Find if there is a user has the same username
        let user = User::Entity::find()
            .filter(User::Column::Username.eq(&admin_user_info.username))
            .one(db)
            .await?;
        if user.is_none() {
            tracing::info!("Creating default admin user");
            let now = TimeDateTimeWithTimeZone::now_utc();
            let hashed_passwd = md5::compute(admin_user_info.password.as_bytes());
            let salt = SaltString::generate(&mut OsRng);
            let argon2 = Argon2::default();
            let password_hash = argon2
                .hash_password(&hashed_passwd.0, &salt)
                .map_err(|e| DbErr::Custom(format!("Hash error: {}", e)))?
                .to_string();
            let admin_user = User::ActiveModel {
                username: Set(admin_user_info.username.clone()),
                encrypted_password: Set(password_hash),
                created_at: Set(now),
                updated_at: Set(now),
                admin: Set(true),
                ..Default::default()
            };
            let _ = admin_user.insert(db).await?;
        } else {
            tracing::info!("Default admin user already exists, skip creation");
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // delete the admin user
        let admin_user_info = crate::config::InitAdminUser.get().ok_or(DbErr::Custom(
            "default admin user config not found".to_string(),
        ))?;
        let db = manager.get_connection();
        let _ = User::Entity::delete_many()
            .filter(User::Column::Username.eq(&admin_user_info.username))
            .exec(db)
            .await?;
        Ok(())
    }
}
