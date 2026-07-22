use sea_orm::{prelude::*, ConnectionTrait, Set};
use uuid::Uuid;

use crate::entity::role::UserGroupRole;
use crate::entity::{
    agents as Agent, group_agent as GroupAgent, task_suite_agent as TaskSuiteAgent,
    task_suites as TaskSuites, user_group as UserGroup,
};
use crate::error::Error;
use crate::schema::{SuiteAgentSelectionAction, SuiteAgentSelectionError};

/// Outcome of applying one entry in a batch selection: either a per-item failure to
/// report back to the caller (`Item`) or a fatal infrastructure error that must abort
/// the whole batch (`Fatal`). The `From` impls let the resolve/apply primitives use `?`
/// on DB errors and have them treated as fatal automatically.
pub(crate) enum SelectionItemError {
    Item(SuiteAgentSelectionError),
    Fatal(Error),
}

impl From<Error> for SelectionItemError {
    fn from(e: Error) -> Self {
        SelectionItemError::Fatal(e)
    }
}

impl From<DbErr> for SelectionItemError {
    fn from(e: DbErr) -> Self {
        SelectionItemError::Fatal(Error::from(e))
    }
}

/// Resolve a suite for a suite agent selection, authorizing the caller's Write role on the
/// suite's owner group.
pub(crate) async fn authorize_suite<C>(
    txn: &C,
    user_id: i64,
    suite_uuid: Uuid,
) -> std::result::Result<TaskSuites::Model, SelectionItemError>
where
    C: ConnectionTrait,
{
    let suite = TaskSuites::Entity::find()
        .filter(TaskSuites::Column::Uuid.eq(suite_uuid))
        .one(txn)
        .await?
        .ok_or(SelectionItemError::Item(
            SuiteAgentSelectionError::SuiteNotFound,
        ))?;

    let authorized = UserGroup::Entity::find()
        .filter(UserGroup::Column::UserId.eq(user_id))
        .filter(UserGroup::Column::GroupId.eq(suite.group_id))
        .one(txn)
        .await?
        .map(|ug| ug.role >= UserGroupRole::Write)
        .unwrap_or(false);
    if !authorized {
        return Err(SelectionItemError::Item(
            SuiteAgentSelectionError::NoWriteAccessOnSuite,
        ));
    }

    Ok(suite)
}

/// Resolve an agent by uuid for a selection batch. Existence only; the group→agent
/// access check happens in [`apply_suite_agent_selection`].
pub(crate) async fn resolve_agent<C>(
    txn: &C,
    agent_uuid: Uuid,
) -> std::result::Result<Agent::Model, SelectionItemError>
where
    C: ConnectionTrait,
{
    Agent::Entity::find()
        .filter(Agent::Column::Uuid.eq(agent_uuid))
        .one(txn)
        .await?
        .ok_or(SelectionItemError::Item(
            SuiteAgentSelectionError::AgentNotFound,
        ))
}

/// Apply one selection override for an already-resolved (suite, agent) pair: enforce
/// the suite group's write access to the agent, then upsert (`Include`/`Exclude`) or
/// clear (`Match`) the override.
pub(crate) async fn apply_suite_agent_selection<C>(
    txn: &C,
    user_id: i64,
    suite: &TaskSuites::Model,
    agent: &Agent::Model,
    action: SuiteAgentSelectionAction,
    now: TimeDateTimeWithTimeZone,
) -> std::result::Result<(), SelectionItemError>
where
    C: ConnectionTrait,
{
    // The suite's group must have write access to the agent (enforced for all actions,
    // including Match, unlike the legacy single-agent reset which skipped this check).
    let has_access = GroupAgent::Entity::find()
        .filter(GroupAgent::Column::GroupId.eq(suite.group_id))
        .filter(GroupAgent::Column::AgentId.eq(agent.id))
        .one(txn)
        .await?
        .map(|ga| ga.role.has_write_access())
        .unwrap_or(false);
    if !has_access {
        return Err(SelectionItemError::Item(
            SuiteAgentSelectionError::NoWriteAccessOnAgent,
        ));
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
