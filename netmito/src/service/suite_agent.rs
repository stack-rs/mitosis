use sea_orm::sea_query::Query;
use sea_orm::{prelude::*, ConnectionTrait, FromQueryResult, Set};
use uuid::Uuid;

use crate::entity::role::UserGroupRole;
use crate::entity::{
    agents as Agent, group_agent as GroupAgent, task_suite_agent as TaskSuiteAgent,
    task_suites as TaskSuites, user_group as UserGroup,
};
use crate::error::{ErrorMsg, ResolveError};
use crate::schema::SuiteAgentOverrideAction;

/// Resolve a suite by uuid and authorize the caller's role is at least `min_user_role`
/// on its owning group.
///
/// Returns [`ResolveError::Item`] if the suite does not exist or the caller does not have
/// right
pub(crate) async fn authorize_suite<C>(
    txn: &C,
    user_id: i64,
    suite_uuid: Uuid,
    min_user_role: UserGroupRole,
) -> std::result::Result<TaskSuites::Model, ResolveError>
where
    C: ConnectionTrait,
{
    let builder = txn.get_database_backend();
    let stmt = Query::select()
        .columns([
            (TaskSuites::Entity, TaskSuites::Column::Id),
            (TaskSuites::Entity, TaskSuites::Column::Uuid),
            (TaskSuites::Entity, TaskSuites::Column::Name),
            (TaskSuites::Entity, TaskSuites::Column::Description),
            (TaskSuites::Entity, TaskSuites::Column::GroupId),
            (TaskSuites::Entity, TaskSuites::Column::CreatorId),
            (TaskSuites::Entity, TaskSuites::Column::Tags),
            (TaskSuites::Entity, TaskSuites::Column::Labels),
            (TaskSuites::Entity, TaskSuites::Column::Priority),
            (TaskSuites::Entity, TaskSuites::Column::WorkerSchedule),
            (TaskSuites::Entity, TaskSuites::Column::ExecHooks),
            (TaskSuites::Entity, TaskSuites::Column::State),
            (TaskSuites::Entity, TaskSuites::Column::LastTaskSubmittedAt),
            (TaskSuites::Entity, TaskSuites::Column::TotalTasks),
            (TaskSuites::Entity, TaskSuites::Column::IncompleteTasks),
            (TaskSuites::Entity, TaskSuites::Column::CreatedAt),
            (TaskSuites::Entity, TaskSuites::Column::UpdatedAt),
            (TaskSuites::Entity, TaskSuites::Column::CompletedAt),
        ])
        .from(TaskSuites::Entity)
        .join(
            sea_orm::JoinType::Join,
            UserGroup::Entity,
            Expr::col((UserGroup::Entity, UserGroup::Column::GroupId))
                .eq(Expr::col((TaskSuites::Entity, TaskSuites::Column::GroupId))),
        )
        .and_where(Expr::col((TaskSuites::Entity, TaskSuites::Column::Uuid)).eq(suite_uuid))
        .and_where(Expr::col((UserGroup::Entity, UserGroup::Column::UserId)).eq(user_id))
        .and_where(Expr::col((UserGroup::Entity, UserGroup::Column::Role)).gte(min_user_role))
        .to_owned();
    TaskSuites::Model::find_by_statement(builder.build(&stmt))
        .one(txn)
        .await?
        .ok_or_else(|| {
            ResolveError::Item(ErrorMsg {
                msg: format!("User doesn't have permission or suite with uuid {suite_uuid}"),
            })
        })
}

/// Resolve an agent by uuid for an override batch. Existence only; the group→agent
/// access check happens in [`apply_suite_agent_override`].
pub(crate) async fn resolve_agent<C>(
    txn: &C,
    agent_uuid: Uuid,
) -> std::result::Result<Agent::Model, ResolveError>
where
    C: ConnectionTrait,
{
    Agent::Entity::find()
        .filter(Agent::Column::Uuid.eq(agent_uuid))
        .one(txn)
        .await?
        .ok_or_else(|| {
            ResolveError::Item(ErrorMsg {
                msg: format!("Agent {} not found", agent_uuid),
            })
        })
}

/// Apply one override for an already-resolved (suite, agent) pair: enforce
/// the suite group's write access to the agent, then upsert (`Include`/`Exclude`) or
/// clear (`Clear`) the override.
pub(crate) async fn apply_suite_agent_override<C>(
    txn: &C,
    user_id: i64,
    suite: &TaskSuites::Model,
    agent: &Agent::Model,
    action: SuiteAgentOverrideAction,
    now: TimeDateTimeWithTimeZone,
) -> std::result::Result<(), ResolveError>
where
    C: ConnectionTrait,
{
    // The suite's group must have write access to the agent.
    let has_access = GroupAgent::Entity::find()
        .filter(GroupAgent::Column::GroupId.eq(suite.group_id))
        .filter(GroupAgent::Column::AgentId.eq(agent.id))
        .one(txn)
        .await?
        .map(|ga| ga.role.has_write_access())
        .unwrap_or(false);
    if !has_access {
        return Err(ResolveError::Item(ErrorMsg {
            msg: "Suite group has no write access to agent".to_string(),
        }));
    }

    let existing = TaskSuiteAgent::Entity::find()
        .filter(TaskSuiteAgent::Column::TaskSuiteId.eq(suite.id))
        .filter(TaskSuiteAgent::Column::AgentId.eq(agent.id))
        .one(txn)
        .await?;

    match (existing, action.override_type()) {
        (None, None) => {
            // Nothing to do
        }
        (None, Some(selection)) => {
            // Add a new row
            let am = TaskSuiteAgent::ActiveModel {
                task_suite_id: Set(suite.id),
                agent_id: Set(agent.id),
                override_type: Set(selection),
                creator_id: Set(Some(user_id)),
                created_at: Set(now),
                updated_at: Set(now),
                ..Default::default()
            };
            am.insert(txn).await?;
        }
        (Some(row), None) => {
            // Remove the existing row
            row.delete(txn).await?;
        }
        (Some(row), Some(selection)) => {
            // Update the existing row
            let mut am: TaskSuiteAgent::ActiveModel = row.into();
            am.override_type = Set(selection);
            am.creator_id = Set(Some(user_id));
            am.updated_at = Set(now);
            am.update(txn).await?;
        }
    }

    Ok(())
}
