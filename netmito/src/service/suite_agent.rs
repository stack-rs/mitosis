use sea_orm::{prelude::*, ConnectionTrait, QuerySelect, Set};
use uuid::Uuid;

use crate::entity::role::UserGroupRole;
use crate::entity::{
    agents as Agent, group_agent as GroupAgent, task_suite_agent as TaskSuiteAgent,
    task_suites as TaskSuites, user_group as UserGroup,
};
use crate::error::{Error, ErrorMsg};
use crate::schema::SuiteAgentOverrideAction;

/// Outcome of applying one entry in a batch override: either a per-item failure to
/// report back to the caller (`Item`) or a fatal infrastructure error that must abort
/// the whole batch (`Fatal`). The `From` impls let the resolve/apply primitives use `?`
/// on DB errors and have them treated as fatal automatically.
pub(crate) enum ResolveError {
    Item(ErrorMsg),
    Fatal(Error),
}

impl From<Error> for ResolveError {
    fn from(e: Error) -> Self {
        ResolveError::Fatal(e)
    }
}

impl From<DbErr> for ResolveError {
    fn from(e: DbErr) -> Self {
        ResolveError::Fatal(Error::from(e))
    }
}

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
    TaskSuites::Entity::find()
        .filter(TaskSuites::Column::Uuid.eq(suite_uuid))
        .filter(UserGroup::Column::UserId.eq(user_id))
        .filter(UserGroup::Column::Role.gte(min_user_role))
        .join(
            sea_orm::JoinType::InnerJoin,
            TaskSuites::Entity::belongs_to(UserGroup::Entity)
                .from(TaskSuites::Column::GroupId)
                .to(UserGroup::Column::GroupId)
                .into(),
        )
        .one(txn)
        .await?
        .ok_or_else(|| {
            ResolveError::Item(ErrorMsg {
                msg: format!(
                    "Suite {suite_uuid} does not exist or the user does not have permission to manage it"
                ),
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

    match (existing, action.selection_type()) {
        (None, None) => {
            // Nothing to do
        }
        (None, Some(selection)) => {
            // Add a new row
            let am = TaskSuiteAgent::ActiveModel {
                task_suite_id: Set(suite.id),
                agent_id: Set(agent.id),
                selection_type: Set(selection),
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
            am.selection_type = Set(selection);
            am.creator_id = Set(Some(user_id));
            am.updated_at = Set(now);
            am.update(txn).await?;
        }
    }

    Ok(())
}
