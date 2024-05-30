use sea_orm::{prelude::*, Set, TransactionTrait};
use sea_orm_migration::prelude::*;

use crate::{
    entity::{
        groups as Group, role::UserGroupRole, state::GroupState, user_group as UserGroup,
        users as User,
    },
    error::{ApiError, Error},
};

pub async fn create_group<C>(
    db: &C,
    user_id: i64,
    group_name: String,
) -> crate::error::Result<Group::Model>
where
    C: TransactionTrait,
{
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
                    _ => Err(DbErr::RecordNotInserted.into()),
                }
            })
        })
        .await?;
    Ok(group)
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
