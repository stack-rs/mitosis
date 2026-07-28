//! Which agents may run which suites.
//!
//! The effective set for a suite is
//!
//! ```text
//! (tag-matched ∪ UserIncluded) − UserExcluded    restricted to agents the
//!                                                suite's group may write to
//! ```
//!
//! `task_suite_agent` persists **only** the user's manual overrides
//! (`[[suite-agent-table-manual-only]]`); the tag-matched half is derived, never
//! stored. Dev derives it in a scheduler actor holding an in-memory index. We
//! derive it per query instead, in SQL: Postgres array containment
//! (`agents.tags @> task_suites.tags`) answers "does this agent carry every tag
//! the suite asks for?" directly, and both `tags` columns already have GIN
//! indexes. Same semantics, no second source of truth to keep coherent.
//!
//! Every eligibility check in the agent layer goes through the predicates here,
//! so the rule lives in exactly one place — including for the day the in-memory
//! scheduler does arrive.

use sea_orm::sea_query::{
    extension::postgres::PgExpr, Alias, Expr, Order, Query, SelectStatement, SimpleExpr,
};
use sea_orm::{ConnectionTrait, FromQueryResult};
use uuid::Uuid;

use crate::entity::role::GroupAgentRole;
use crate::entity::state::{AgentState, TaskSuiteState};
use crate::entity::task_suite_agent::SuiteAgentOverrideType;
use crate::entity::{
    agents as Agent, group_agent as GroupAgent, task_suite_agent as TaskSuiteAgent,
    task_suites as TaskSuites,
};

fn suite_col(col: TaskSuites::Column) -> Expr {
    Expr::col((TaskSuites::Entity, col))
}

fn agent_col(col: Agent::Column) -> Expr {
    Expr::col((Agent::Entity, col))
}

/// A manual override row exists for this `(suite, agent)` pair with `kind`.
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

/// The suite's owning group grants this agent at least Write.
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

/// The full eligibility predicate for a `(suite, agent)` pair, in terms of the
/// `task_suites` and `agents` columns of the surrounding query. Both tables must
/// be in scope wherever this is used.
pub fn eligibility_predicate() -> SimpleExpr {
    let tag_matched = agent_col(Agent::Column::Tags).contains(suite_col(TaskSuites::Column::Tags));
    let included = has_override(SuiteAgentOverrideType::UserIncluded);
    let excluded = has_override(SuiteAgentOverrideType::UserExcluded);

    group_grants_access()
        .and(tag_matched.or(included))
        .and(excluded.not())
}

/// A suite is worth handing to an agent when it can still take work: not
/// cancelled, and with at least one task left to finish. `Complete` suites are
/// excluded — they have nothing to run until a new task reopens them.
pub fn suite_has_work() -> SimpleExpr {
    suite_col(TaskSuites::Column::State)
        .is_in([TaskSuiteState::Open, TaskSuiteState::Closed])
        .and(suite_col(TaskSuites::Column::IncompleteTasks).gt(0))
}

/// Base `SELECT … FROM task_suites, agents WHERE <eligible>` that callers extend
/// with their own projection and filters.
fn eligible_pairs() -> SelectStatement {
    Query::select()
        .from(TaskSuites::Entity)
        .from(Agent::Entity)
        .and_where(eligibility_predicate())
        .to_owned()
}

/// Is this agent eligible to run this suite (ignoring whether the suite
/// currently has work)?
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

/// Does this agent have any suite with pending work right now?
pub async fn agent_has_available_suite<C: ConnectionTrait>(
    db: &C,
    agent_id: i64,
) -> crate::error::Result<bool> {
    Ok(best_available_suite_id(db, agent_id).await?.is_some())
}

/// The suite this agent should pick up next: highest priority first, then
/// oldest. `None` when nothing is available.
pub async fn best_available_suite_id<C: ConnectionTrait>(
    db: &C,
    agent_id: i64,
) -> crate::error::Result<Option<i64>> {
    #[derive(FromQueryResult)]
    struct SuiteId {
        suite_id: i64,
    }

    let mut stmt = eligible_pairs();
    stmt.expr_as(suite_col(TaskSuites::Column::Id), Alias::new("suite_id"))
        .and_where(agent_col(Agent::Column::Id).eq(agent_id))
        .and_where(suite_has_work())
        .order_by_expr(suite_col(TaskSuites::Column::Priority).into(), Order::Desc)
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

/// The uuids of every agent eligible for a suite — what a suite's detail view
/// reports and who gets considered when the suite gains work.
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

/// The uuids of eligible agents that are **idle**, i.e. the ones that could
/// start this suite right now. Used to target `SuiteAvailable` pushes.
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
