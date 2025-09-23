use std::collections::{HashMap, HashSet};

use sea_orm::{prelude::*, FromQueryResult, QuerySelect, Set, TransactionTrait};
use sea_orm_migration::prelude::*;

use crate::{
    config::InfraPool,
    entity::{
        group_worker as GroupWorker, groups as Group, role::UserGroupRole, state::GroupState,
        user_group as UserGroup, users as User,
    },
    error::{ApiError, AuthError, Error},
    schema::GroupQueryInfo,
};

use super::worker::PartialUserGroupRole;

pub async fn user_create_group<C>(
    db: &C,
    user_id: i64,
    group_name: String,
) -> crate::error::Result<Group::Model>
where
    C: TransactionTrait,
{
    if !super::name_validator(&group_name) {
        return Err(ApiError::InvalidRequest("Invalid group name".to_string()).into());
    }
    let group = db
        .transaction::<_, Group::Model, Error>(|txn| {
            Box::pin(async move {
                let user = User::Entity::find()
                    .filter(User::Column::Username.eq(&group_name))
                    .one(txn)
                    .await?;
                let group = Group::Entity::find()
                    .filter(Group::Column::GroupName.eq(&group_name))
                    .one(txn)
                    .await?;
                match (user, group) {
                    (None, None) => {
                        let now = TimeDateTimeWithTimeZone::now_utc();
                        let creator = User::Entity::find_by_id(user_id)
                            .one(txn)
                            .await?
                            .ok_or::<Error>(ApiError::InternalServerError.into())?;
                        if creator.group_count >= creator.group_quota {
                            return Err(ApiError::QuotaExceeded.into());
                        } else {
                            let creator = User::ActiveModel {
                                id: Set(creator.id),
                                group_count: Set(creator.group_count + 1),
                                updated_at: Set(now),
                                ..Default::default()
                            };
                            creator.update(txn).await?;
                        }
                        let group = Group::ActiveModel {
                            group_name: Set(group_name),
                            creator_id: Set(user_id),
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
                            user_id: Set(user_id),
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
                        Ok(group)
                    }
                    _ => Err(ApiError::AlreadyExists(group_name).into()),
                }
            })
        })
        .await?;
    Ok(group)
}

#[derive(FromQueryResult)]
struct UserInGroup {
    username: String,
    role: UserGroupRole,
}

pub async fn user_get_group_by_name(
    user_id: i64,
    group_name: String,
    pool: &InfraPool,
) -> crate::error::Result<GroupQueryInfo> {
    let group = Group::Entity::find()
        .filter(Group::Column::GroupName.eq(&group_name))
        .one(&pool.db)
        .await?
        .ok_or(ApiError::NotFound(format!("Group {group_name}")))?;
    let user_group = UserGroup::Entity::find()
        .filter(UserGroup::Column::UserId.eq(user_id))
        .filter(UserGroup::Column::GroupId.eq(group.id))
        .one(&pool.db)
        .await?
        .ok_or(AuthError::PermissionDenied)?;
    let creator_username: Option<String> = User::Entity::find()
        .filter(User::Column::Id.eq(group.creator_id))
        .select_only()
        .column(User::Column::Username)
        .into_tuple()
        .one(&pool.db)
        .await?;
    let creator_username = creator_username.unwrap_or_default();
    let users_in_group = if user_group.role == UserGroupRole::Admin {
        let builder = pool.db.get_database_backend();
        let stmt = Query::select()
            .column((UserGroup::Entity, UserGroup::Column::Role))
            .column((User::Entity, User::Column::Username))
            .from(UserGroup::Entity)
            .join(
                sea_orm::JoinType::Join,
                User::Entity,
                Expr::col((UserGroup::Entity, UserGroup::Column::UserId))
                    .eq(Expr::col((User::Entity, User::Column::Id))),
            )
            .and_where(Expr::col((UserGroup::Entity, UserGroup::Column::GroupId)).eq(group.id))
            .to_owned();
        let user_group_role = UserInGroup::find_by_statement(builder.build(&stmt))
            .all(&pool.db)
            .await?;
        let users_in_group = user_group_role
            .into_iter()
            .map(|r| (r.username, r.role))
            .collect();
        Some(users_in_group)
    } else {
        None
    };
    let worker_count: Option<i64> = GroupWorker::Entity::find()
        .filter(GroupWorker::Column::GroupId.eq(group.id))
        .select_only()
        .column_as(GroupWorker::Column::WorkerId.count(), "count")
        .into_tuple()
        .one(&pool.db)
        .await?;
    let worker_count = worker_count.unwrap_or_default();
    Ok(GroupQueryInfo {
        group_name: group.group_name,
        creator_username,
        created_at: group.created_at,
        updated_at: group.updated_at,
        state: group.state,
        task_count: group.task_count,
        storage_quota: group.storage_quota,
        storage_used: group.storage_used,
        worker_count,
        users_in_group,
    })
}

pub async fn change_group_state<C>(
    db: &C,
    group_name: String,
    state: GroupState,
) -> crate::error::Result<GroupState>
where
    C: TransactionTrait,
{
    Ok(db
        .transaction::<_, GroupState, Error>(|txn| {
            Box::pin(async move {
                let group = Group::Entity::find()
                    .filter(Group::Column::GroupName.eq(&group_name))
                    .one(txn)
                    .await?;
                if let Some(group) = group {
                    // change state to the new state
                    let group = Group::ActiveModel {
                        id: Set(group.id),
                        state: Set(state),
                        updated_at: Set(TimeDateTimeWithTimeZone::now_utc()),
                        ..Default::default()
                    };
                    let g = group.update(txn).await?;
                    Ok(g.state)
                } else {
                    Err(DbErr::RecordNotUpdated.into())
                }
            })
        })
        .await?)
}

pub async fn update_user_group_role(
    user_id: i64,
    group_name: String,
    mut relations: HashMap<String, UserGroupRole>,
    pool: &InfraPool,
) -> crate::error::Result<()> {
    if relations.is_empty() {
        return Err(ApiError::InvalidRequest("Empty group relations".to_string()).into());
    }
    let group = Group::Entity::find()
        .filter(Group::Column::GroupName.eq(&group_name))
        .one(&pool.db)
        .await?
        .ok_or(ApiError::NotFound(format!("Group {group_name}")))?;
    let user_group = UserGroup::Entity::find()
        .filter(UserGroup::Column::UserId.eq(user_id))
        .filter(UserGroup::Column::GroupId.eq(group.id))
        .one(&pool.db)
        .await?
        .ok_or(AuthError::PermissionDenied)?;
    if user_group.role != UserGroupRole::Admin {
        return Err(AuthError::PermissionDenied.into());
    }
    let user_names = relations.keys().cloned().collect::<Vec<_>>();
    let user_count = user_names.len();
    let users: Vec<(i64, String)> = User::Entity::find()
        .filter(Expr::col(User::Column::Username).eq(PgFunc::any(user_names)))
        .select_only()
        .column(User::Column::Id)
        .column(User::Column::Username)
        .into_tuple()
        .all(&pool.db)
        .await?;
    if users.len() != user_count {
        return Err(ApiError::InvalidRequest("Some users do not exist".to_string()).into());
    }
    let user_group_relations = users.into_iter().filter_map(|(user_id, username)| {
        relations
            .remove(&username)
            .map(|role| UserGroup::ActiveModel {
                user_id: Set(user_id),
                group_id: Set(group.id),
                role: Set(role),
                ..Default::default()
            })
    });
    UserGroup::Entity::insert_many(user_group_relations)
        .on_conflict(
            OnConflict::columns([UserGroup::Column::UserId, UserGroup::Column::GroupId])
                .update_column(UserGroup::Column::Role)
                .to_owned(),
        )
        .exec(&pool.db)
        .await?;
    Ok(())
}

pub async fn remove_user_group_role(
    user_id: i64,
    group_name: String,
    usernames: HashSet<String>,
    pool: &InfraPool,
) -> crate::error::Result<()> {
    if usernames.is_empty() {
        return Err(ApiError::InvalidRequest("Empty group relations".to_string()).into());
    }
    let group = Group::Entity::find()
        .filter(Group::Column::GroupName.eq(&group_name))
        .one(&pool.db)
        .await?
        .ok_or(ApiError::NotFound(format!("Group {group_name}")))?;
    let user_group = UserGroup::Entity::find()
        .filter(UserGroup::Column::UserId.eq(user_id))
        .filter(UserGroup::Column::GroupId.eq(group.id))
        .one(&pool.db)
        .await?
        .ok_or(AuthError::PermissionDenied)?;
    if user_group.role != UserGroupRole::Admin {
        return Err(AuthError::PermissionDenied.into());
    }
    let user_names = usernames.into_iter().collect::<Vec<_>>();
    let user_count = user_names.len();
    let users: Vec<i64> = User::Entity::find()
        .filter(Expr::col(User::Column::Username).eq(PgFunc::any(user_names)))
        .select_only()
        .column(User::Column::Id)
        .column(User::Column::Username)
        .into_tuple()
        .all(&pool.db)
        .await?;
    if users.len() != user_count {
        return Err(ApiError::InvalidRequest("Some users do not exist".to_string()).into());
    }
    UserGroup::Entity::delete_many()
        .filter(Expr::col(UserGroup::Column::UserId).eq(PgFunc::any(users)))
        .filter(Expr::col(UserGroup::Column::GroupId).eq(group.id))
        .exec(&pool.db)
        .await?;
    Ok(())
}

pub async fn query_user_groups(
    user_id: i64,
    pool: &InfraPool,
) -> crate::error::Result<HashMap<String, UserGroupRole>> {
    let builder = pool.db.get_database_backend();
    let stmt = Query::select()
        .columns([
            (UserGroup::Entity, UserGroup::Column::GroupId),
            (UserGroup::Entity, UserGroup::Column::Role),
        ])
        .column((Group::Entity, Group::Column::GroupName))
        .from(UserGroup::Entity)
        .join(
            sea_orm::JoinType::Join,
            Group::Entity,
            Expr::col((UserGroup::Entity, UserGroup::Column::GroupId))
                .eq(Expr::col((Group::Entity, Group::Column::Id))),
        )
        .and_where(Expr::col((UserGroup::Entity, UserGroup::Column::UserId)).eq(user_id))
        .to_owned();
    let user_group_role = PartialUserGroupRole::find_by_statement(builder.build(&stmt))
        .all(&pool.db)
        .await?;
    let group_relations = user_group_role
        .into_iter()
        .map(|r| (r.group_name, r.role))
        .collect();
    Ok(group_relations)
}

enum StorageQuotaOp {
    Increase(i64),
    Decrease(i64),
    Set(i64),
}

fn parse_storage_quota(quota: &str) -> crate::error::Result<StorageQuotaOp> {
    fn parse_bytesize(s: &str) -> crate::error::Result<i64> {
        let u = parse_size::parse_size(s)?;
        Ok(u as i64)
    }
    match quota {
        s if s.starts_with('+') => {
            let v = parse_bytesize(&s[1..])?;
            Ok(StorageQuotaOp::Increase(v))
        }
        s if s.starts_with('-') => {
            let v = parse_bytesize(&s[1..])?;
            Ok(StorageQuotaOp::Decrease(v))
        }
        s if s.starts_with('=') => {
            let v = parse_bytesize(&s[1..])?;
            Ok(StorageQuotaOp::Set(v))
        }
        s => {
            let v = parse_bytesize(s)?;
            Ok(StorageQuotaOp::Set(v))
        }
    }
}

pub async fn change_group_storage_quota(
    pool: &InfraPool,
    group_name: String,
    storage_quota: String,
) -> crate::error::Result<i64> {
    let quota_op = parse_storage_quota(&storage_quota)?;
    let updated_quota = pool
        .db
        .transaction::<_, i64, Error>(|txn| {
            Box::pin(async move {
                let group = Group::Entity::find()
                    .filter(Group::Column::GroupName.eq(&group_name))
                    .one(txn)
                    .await?;
                if let Some(group) = group {
                    // change state to the new state
                    let new_quota = match quota_op {
                        StorageQuotaOp::Increase(v) => group.storage_quota.saturating_add(v),
                        StorageQuotaOp::Decrease(v) => group.storage_quota.saturating_sub(v),
                        StorageQuotaOp::Set(v) => v,
                    };
                    let group = Group::ActiveModel {
                        id: Set(group.id),
                        storage_quota: Set(new_quota),
                        updated_at: Set(TimeDateTimeWithTimeZone::now_utc()),
                        ..Default::default()
                    };
                    let g = group.update(txn).await?;
                    Ok(g.storage_quota)
                } else {
                    Err(DbErr::RecordNotUpdated.into())
                }
            })
        })
        .await?;
    Ok(updated_quota)
}
