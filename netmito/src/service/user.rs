use argon2::{
    password_hash::{rand_core::OsRng, PasswordHasher, SaltString},
    Argon2,
};
use sea_orm::{prelude::*, Set, TransactionTrait};
use sea_orm_migration::prelude::*;
use std::str::FromStr;

use crate::{
    config::InfraPool,
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

fn parse_quota(quota: i32, quota_op: &str) -> crate::error::Result<i32> {
    match quota_op {
        s if s.starts_with('+') => {
            let v = i32::from_str(&s[1..])?;
            Ok(quota + v)
        }
        s if s.starts_with('-') => {
            let v = i32::from_str(&s[1..])?;
            Ok(quota - v)
        }
        s if s.starts_with('=') => {
            let v = i32::from_str(&s[1..])?;
            Ok(v)
        }
        s => {
            let v = i32::from_str(s)?;
            Ok(v)
        }
    }
}

pub async fn change_user_group_quota(
    pool: &InfraPool,
    user_name: String,
    group_quota: String,
) -> crate::error::Result<i32> {
    let updated_quota = pool
        .db
        .transaction::<_, i32, Error>(|txn| {
            Box::pin(async move {
                let user = User::Entity::find()
                    .filter(User::Column::Username.eq(&user_name))
                    .one(txn)
                    .await?;
                if let Some(user) = user {
                    let new_quota = parse_quota(user.group_quota, &group_quota)?.max(0);
                    let user = User::ActiveModel {
                        id: Set(user.id),
                        group_quota: Set(new_quota),
                        updated_at: Set(TimeDateTimeWithTimeZone::now_utc()),
                        ..Default::default()
                    };
                    let u = user.update(txn).await?;
                    Ok(u.group_quota)
                } else {
                    Err(DbErr::RecordNotUpdated.into())
                }
            })
        })
        .await?;
    Ok(updated_quota)
}
