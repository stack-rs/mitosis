use argon2::{
    password_hash::{rand_core::OsRng, PasswordHasher, SaltString},
    Argon2,
};
use sea_orm::{prelude::*, Set, TransactionTrait};
use sea_orm_migration::prelude::*;

use crate::{
    entity::{
        groups as Group, role::UserGroupRole, state::UserState, user_group as UserGroup,
        users as User,
    },
    error::Error,
};

pub async fn create_user<C>(
    db: &C,
    username: String,
    md5_password: [u8; 16],
    admin: bool,
) -> crate::error::Result<User::Model>
where
    C: TransactionTrait,
{
    let user = db
        .transaction::<_, User::Model, Error>(|txn| {
            Box::pin(async move {
                let user = User::Entity::find()
                    .filter(User::Column::Username.eq(&username))
                    .one(txn)
                    .await?;
                let group = Group::Entity::find()
                    .filter(Group::Column::GroupName.eq(&username))
                    .one(txn)
                    .await?;
                match (user, group) {
                    (None, None) => {
                        let now = TimeDateTimeWithTimeZone::now_utc();
                        let salt = SaltString::generate(&mut OsRng);
                        let argon2 = Argon2::default();
                        let password_hash = argon2.hash_password(&md5_password, &salt)?.to_string();
                        let user = User::ActiveModel {
                            username: Set(username.clone()),
                            encrypted_password: Set(password_hash),
                            created_at: Set(now),
                            updated_at: Set(now),
                            admin: Set(admin),
                            ..Default::default()
                        };
                        let user = User::Entity::insert(user)
                            .on_conflict(
                                sea_query::OnConflict::column(User::Column::Username)
                                    .do_nothing()
                                    .to_owned(),
                            )
                            .exec_with_returning(txn)
                            .await?;
                        let group = Group::ActiveModel {
                            group_name: Set(username),
                            creator_id: Set(user.id),
                            created_at: Set(now),
                            updated_at: Set(now),
                            ..Default::default()
                        };
                        let group = Group::Entity::insert(group)
                            .on_conflict(
                                sea_query::OnConflict::column(Group::Column::GroupName)
                                    .do_nothing()
                                    .to_owned(),
                            )
                            .exec_with_returning(txn)
                            .await?;
                        let user_group = UserGroup::ActiveModel {
                            user_id: Set(user.id),
                            group_id: Set(group.id),
                            role: Set(UserGroupRole::Admin),
                            ..Default::default()
                        };
                        UserGroup::Entity::insert(user_group)
                            .on_conflict(
                                sea_query::OnConflict::columns([
                                    UserGroup::Column::UserId,
                                    UserGroup::Column::GroupId,
                                ])
                                .do_nothing()
                                .to_owned(),
                            )
                            .exec(txn)
                            .await?;
                        Ok(user)
                    }
                    _ => Err(DbErr::RecordNotInserted.into()),
                }
            })
        })
        .await?;
    Ok(user)
}

pub async fn change_user_state<C>(
    db: &C,
    username: String,
    state: UserState,
) -> crate::error::Result<UserState>
where
    C: TransactionTrait,
{
    Ok(db
        .transaction::<_, UserState, Error>(|txn| {
            Box::pin(async move {
                let user = User::Entity::find()
                    .filter(User::Column::Username.eq(&username))
                    .one(txn)
                    .await?;
                if let Some(user) = user {
                    // change state to the new state
                    let user = User::ActiveModel {
                        id: Set(user.id),
                        state: Set(state),
                        ..Default::default()
                    };
                    let u = user.update(txn).await?;
                    Ok(u.state)
                } else {
                    Err(DbErr::RecordNotUpdated.into())
                }
            })
        })
        .await?)
}
