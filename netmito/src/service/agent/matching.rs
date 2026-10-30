//! Which agents can run which suites.
//!
//! The set of agents that can run a suite is
//!
//! ```text
//! (tag-matched ∪ UserIncluded) − UserExcluded    only agents that the group of the suite can write to
//! ```
//!
//! `task_suite_agent` keeps only the overrides that a user sets by hand. The tag-matched part is
//! not stored: each query calculates it in SQL. `agents.tags @> task_suites.tags` tells if an agent
//! has all the tags of the suite, and both `tags` columns have GIN indexes.
//!
//! All the eligibility checks of the agent service use the predicates here, thus the rule is in
//! one location.

use sea_orm::sea_query::{
    extension::postgres::PgExpr, Alias, Expr, Order, Query, SelectStatement, SimpleExpr,
};
use sea_orm::{ConnectionTrait, FromQueryResult};
use uuid::Uuid;

use crate::entity::role::GroupAgentRole;
use crate::entity::state::{AgentState, TaskState, TaskSuiteState};
use crate::entity::task_suite_agent::SuiteAgentOverrideType;
use crate::entity::{
    active_tasks as ActiveTasks, agents as Agent, group_agent as GroupAgent,
    task_suite_agent as TaskSuiteAgent, task_suites as TaskSuites,
};

fn suite_col(col: TaskSuites::Column) -> Expr {
    Expr::col((TaskSuites::Entity, col))
}

fn agent_col(col: Agent::Column) -> Expr {
    Expr::col((Agent::Entity, col))
}

/// An override row of type `kind` exists for the pair `(suite, agent)`.
fn has_override(kind: SuiteAgentOverrideType) -> SimpleExpr {
    Expr::exists(
        Query::select()
            .expr(Expr::val(1))
            .from(TaskSuiteAgent::Entity)
            .and_where(
                Expr::col((TaskSuiteAgent::Entity, TaskSuiteAgent::Column::TaskSuiteId))
                    .equals((TaskSuites::Entity, TaskSuites::Column::Id)),
            )
            .and_where(
                Expr::col((TaskSuiteAgent::Entity, TaskSuiteAgent::Column::AgentId))
                    .equals((Agent::Entity, Agent::Column::Id)),
            )
            .and_where(
                Expr::col((TaskSuiteAgent::Entity, TaskSuiteAgent::Column::OverrideType)).eq(kind),
            )
            .to_owned(),
    )
}

/// The group of the suite has at least the Write role on the agent.
fn group_grants_access() -> SimpleExpr {
    Expr::exists(
        Query::select()
            .expr(Expr::val(1))
            .from(GroupAgent::Entity)
            .and_where(
                Expr::col((GroupAgent::Entity, GroupAgent::Column::GroupId))
                    .equals((TaskSuites::Entity, TaskSuites::Column::GroupId)),
            )
            .and_where(
                Expr::col((GroupAgent::Entity, GroupAgent::Column::AgentId))
                    .equals((Agent::Entity, Agent::Column::Id)),
            )
            .and_where(
                Expr::col((GroupAgent::Entity, GroupAgent::Column::Role))
                    .gte(GroupAgentRole::Write),
            )
            .to_owned(),
    )
}

/// The full eligibility predicate for a pair `(suite, agent)`. The query that uses it must have
/// both `task_suites` and `agents`.
pub fn eligibility_predicate() -> SimpleExpr {
    let tag_matched = agent_col(Agent::Column::Tags).contains(suite_col(TaskSuites::Column::Tags));
    let included = has_override(SuiteAgentOverrideType::UserIncluded);
    let excluded = has_override(SuiteAgentOverrideType::UserExcluded);

    group_grants_access()
        .and(tag_matched.or(included))
        .and(excluded.not())
}

/// A suite has work for an agent if it is `Open` or `Closed` and has a `Ready` task.
///
/// `incomplete_tasks > 0` is not sufficient: it also counts the tasks that other agents run. A
/// suite whose remaining tasks all run would then get agents that find nothing to claim.
pub fn suite_has_work() -> SimpleExpr {
    let has_claimable_task = Expr::exists(
        Query::select()
            .expr(Expr::val(1))
            .from(ActiveTasks::Entity)
            .and_where(
                Expr::col((ActiveTasks::Entity, ActiveTasks::Column::TaskSuiteId))
                    .equals((TaskSuites::Entity, TaskSuites::Column::Id)),
            )
            .and_where(
                Expr::col((ActiveTasks::Entity, ActiveTasks::Column::State)).eq(TaskState::Ready),
            )
            .to_owned(),
    );

    suite_col(TaskSuites::Column::State)
        .is_in([TaskSuiteState::Open, TaskSuiteState::Closed])
        .and(has_claimable_task)
}

/// `SELECT … FROM task_suites, agents WHERE <eligible>`, for the callers to add their columns and
/// filters.
fn eligible_pairs() -> SelectStatement {
    Query::select()
        .from(TaskSuites::Entity)
        .from(Agent::Entity)
        .and_where(eligibility_predicate())
        .to_owned()
}

/// Whether this agent can run this suite. It does not check if the suite has work.
pub async fn is_agent_eligible<C: ConnectionTrait>(
    db: &C,
    suite_id: i64,
    agent_id: i64,
) -> crate::error::Result<bool> {
    #[derive(FromQueryResult)]
    struct Exists {
        #[allow(dead_code)]
        eligible: i32,
    }

    let mut stmt = eligible_pairs();
    stmt.expr_as(Expr::val(1), Alias::new("eligible"))
        .and_where(suite_col(TaskSuites::Column::Id).eq(suite_id))
        .and_where(agent_col(Agent::Column::Id).eq(agent_id))
        .limit(1);

    let builder = db.get_database_backend();
    Ok(Exists::find_by_statement(builder.build(&stmt))
        .one(db)
        .await?
        .is_some())
}

/// Whether this agent has a suite with work now.
pub async fn agent_has_available_suite<C: ConnectionTrait>(
    db: &C,
    agent_id: i64,
    blocked: &[i64],
) -> crate::error::Result<bool> {
    Ok(best_available_suite_id(db, agent_id, blocked)
        .await?
        .is_some())
}

/// The suite that this agent should run next: the highest priority first, then the oldest. `None`
/// if no suite is available.
///
/// The query excludes the suites in `blocked` ([`SuiteQueues::blocked_for`]: the suites reserved
/// for their jobs). A filter after the query would be wrong: a blocked suite at the top would hide
/// the next suite.
///
/// [`SuiteQueues::blocked_for`]: crate::service::agent::queue::SuiteQueues::blocked_for
pub async fn best_available_suite_id<C: ConnectionTrait>(
    db: &C,
    agent_id: i64,
    blocked: &[i64],
) -> crate::error::Result<Option<i64>> {
    #[derive(FromQueryResult)]
    struct SuiteId {
        suite_id: i64,
    }

    let mut stmt = eligible_pairs();
    stmt.expr_as(suite_col(TaskSuites::Column::Id), Alias::new("suite_id"))
        .and_where(agent_col(Agent::Column::Id).eq(agent_id))
        .and_where(suite_has_work());
    if !blocked.is_empty() {
        stmt.and_where(suite_col(TaskSuites::Column::Id).is_not_in(blocked.to_vec()));
    }
    stmt.order_by_expr(suite_col(TaskSuites::Column::Priority).into(), Order::Desc)
        .order_by_expr(suite_col(TaskSuites::Column::CreatedAt).into(), Order::Asc)
        .limit(1);

    let builder = db.get_database_backend();
    Ok(SuiteId::find_by_statement(builder.build(&stmt))
        .one(db)
        .await?
        .map(|r| r.suite_id))
}

#[derive(FromQueryResult)]
struct AgentUuid {
    uuid: Uuid,
}

/// The uuids of all the agents that can run a suite.
pub async fn eligible_agent_uuids<C: ConnectionTrait>(
    db: &C,
    suite_id: i64,
) -> crate::error::Result<Vec<Uuid>> {
    let mut stmt = eligible_pairs();
    stmt.expr_as(agent_col(Agent::Column::Uuid), Alias::new("uuid"))
        .and_where(suite_col(TaskSuites::Column::Id).eq(suite_id));

    let builder = db.get_database_backend();
    Ok(AgentUuid::find_by_statement(builder.build(&stmt))
        .all(db)
        .await?
        .into_iter()
        .map(|r| r.uuid)
        .collect())
}

/// The uuids of the `Idle` agents that can run a suite: the agents that can start it now.
pub async fn idle_eligible_agent_uuids<C: ConnectionTrait>(
    db: &C,
    suite_id: i64,
) -> crate::error::Result<Vec<Uuid>> {
    let mut stmt = eligible_pairs();
    stmt.expr_as(agent_col(Agent::Column::Uuid), Alias::new("uuid"))
        .and_where(suite_col(TaskSuites::Column::Id).eq(suite_id))
        .and_where(agent_col(Agent::Column::State).eq(AgentState::Idle));

    let builder = db.get_database_backend();
    Ok(AgentUuid::find_by_statement(builder.build(&stmt))
        .all(db)
        .await?
        .into_iter()
        .map(|r| r.uuid)
        .collect())
}
