//! Agent lifecycle: fleet management for users, and the execution loop for
//! agents.
//!
//! ## The loop
//!
//! ```text
//! register ──▶ heartbeat ──▶ accept suite ──▶ start ──▶ …tasks… ──▶ cleanup ──▶ complete
//!                  ▲                                                                │
//!                  └─────────────────────── Idle ◀───────────────────────────────────┘
//! ```
//!
//! `accept` is the only transition that claims anything, and it both chooses
//! the suite and takes it: one transaction picks a candidate, locks it,
//! re-checks eligibility, moves the agent `Idle → Provisioning`, and opens a
//! `suite_agent_jobs` row whose opaque id the agent echoes on every later call.
//! `start`/`cleanup`/`complete` advance that row and the agent state in step.
//!
//! ## How work is offered
//!
//! A suite that gains work offers it to the jobs already running it before
//! anyone else ([`notify_suite_available`]), fullest first and only as many of
//! them as it takes to cover the queue: they are told directly and the suite is
//! briefly reserved for them. Idle eligible agents are nudged for whatever those
//! jobs cannot take, and for a suite no job is on at all. The nudge names no
//! suite — which one is best for an agent is decided in [`agent_accept_suite`],
//! under the suite's lock, against the state at the moment it asks. [`queue`]
//! holds the in-memory side of this.
//!
//! ## No automatic preemption (yet)
//!
//! An agent that has accepted a suite runs it to completion; nothing takes it
//! away for a higher-priority suite on its own. Priority only orders the
//! *choice* made in [`matching::best_available_suite_id`], which is made once,
//! when the agent goes looking for work.
//!
//! [`user_stop_agent_job`] is the manual stand-in: stopping a job sends the
//! agent straight back through that choice, so it lands on whatever outranks
//! everything now. Because the agent re-picks *after* winding down rather than
//! being handed a target, the answer is as fresh as it can be.
//!
//! The pieces automatic preemption needs are in place and unused: the
//! `PreemptSuite` notification, the agent's handling of it, and the fact that
//! `accept` records the job so the coordinator can address a specific in-flight
//! run. Turning it on means emitting that notification here — no schema or
//! protocol change.

pub mod heartbeat;
pub mod hook;
pub mod job;
pub mod matching;
pub mod queue;
pub mod task;

use std::collections::{HashMap, HashSet};

use sea_orm::sea_query::extension::postgres::PgExpr;
use sea_orm::sea_query::{Alias, PgFunc, Query};
use sea_orm::{prelude::*, FromQueryResult, QueryOrder, QuerySelect, Set, TransactionTrait};
use uuid::Uuid;

use crate::config::InfraPool;
use crate::entity::{
    active_tasks as ActiveTasks, agents as Agent, group_agent as GroupAgent, groups as Group,
    machines as Machines,
    role::{GroupAgentRole, UserGroupRole},
    state::{AgentState, SuiteJobState, TaskState, TaskSuiteState},
    suite_agent_jobs as SuiteAgentJobs, task_suites as TaskSuites, user_group as UserGroup,
    users as User,
};
use crate::error::{ApiError, AuthError, Error, Result};
use crate::schema::{
    AcceptSuiteReq, AcceptSuiteResp, AgentHeartbeatReq, AgentHeartbeatResp, AgentInfo,
    AgentNotification, AgentShutdownOp, AgentsQueryReq, AgentsQueryResp, CompleteJobReq,
    CompleteJobResp, CountQuery, ExecHooks, RegisterAgentReq, RegisterAgentResp, StopAgentJobResp,
    StopJobOp, SuiteJobOutcome, TaskSuiteSpec, WorkerSchedulePlan,
};
use crate::service::auth::token::generate_worker_token;
use crate::ws::AgentWsRouter;

use heartbeat::AgentHeartbeatOp;

// ─────────────────────────────────────────────────────────────────────────────
// Fleet management (user-authed)
// ─────────────────────────────────────────────────────────────────────────────

/// Register an agent, or re-adopt the one already bound to this machine.
///
/// `machines.machine_code` and `machines.agent_id` are both unique, so a machine
/// has exactly one agent row for its whole life. Rather than fail a restarting
/// agent on the unique index, registration is an upsert: an existing machine
/// hands back its agent, refreshed with the new tags/labels/groups/metadata and
/// a newly minted token. The caller needs Write or Admin in every group listed,
/// and Admin in `admin_group`.
///
/// Access is **rewritten**, not merged: the request is the whole truth about
/// which groups may reach the agent, so a re-registration that drops a group
/// drops its access too.
pub async fn user_register_agent(
    user_id: i64,
    pool: &InfraPool,
    req: RegisterAgentReq,
) -> Result<RegisterAgentResp> {
    let now = TimeDateTimeWithTimeZone::now_utc();
    let tags: Vec<String> = req.tags.into_iter().collect();
    let labels: Vec<String> = req.labels.into_iter().collect();
    let groups: Vec<String> = req.groups.into_iter().collect();
    let admin_group = req.admin_group.map(|g| g.trim().to_string());
    let machine_code = req.machine_code.trim().to_string();
    if machine_code.is_empty() {
        return Err(Error::ApiError(ApiError::InvalidRequest(
            "machine_code must not be empty".to_string(),
        )));
    }
    let metadata = req
        .metadata
        .as_ref()
        .map(serde_json::to_value)
        .transpose()?;

    let (agent_uuid, agent_id, reused, retired) = pool
        .db
        .transaction::<_, (Uuid, i64, bool, Option<RetiredWork>), Error>(|txn| {
            Box::pin(async move {
                // Exactly one group holds Admin over the agent, and that role is
                // the only way it is ever shut down. Unless the caller names a
                // group, it is their personal one
                let admin_group_name = match admin_group {
                    Some(name) => name,
                    _ => {
                        User::Entity::find_by_id(user_id)
                            .one(txn)
                            .await?
                            .ok_or(Error::ApiError(ApiError::NotFound("User".to_string())))?
                            .username
                    }
                };

                let admin_group = Group::Entity::find()
                    .filter(Group::Column::GroupName.eq(admin_group_name.clone()))
                    .one(txn)
                    .await?
                    .ok_or(Error::ApiError(ApiError::NotFound(format!(
                        "Group {admin_group_name}"
                    ))))?;
                // The registering user must be the admin of the admin_group
                let admin_membership = UserGroup::Entity::find()
                    .filter(UserGroup::Column::UserId.eq(user_id))
                    .filter(UserGroup::Column::GroupId.eq(admin_group.id))
                    .one(txn)
                    .await?
                    .ok_or(Error::AuthError(AuthError::PermissionDenied))?;
                if admin_membership.role != UserGroupRole::Admin {
                    return Err(Error::AuthError(AuthError::PermissionDenied));
                }

                // Resolve every requested group up front
                //
                // The registering user must have at least Write permission in each group.
                let mut group_ids = Vec::with_capacity(groups.len());
                for group_name in &groups {
                    let group = Group::Entity::find()
                        .filter(Group::Column::GroupName.eq(group_name.clone()))
                        .one(txn)
                        .await?
                        .ok_or(Error::ApiError(ApiError::NotFound(format!(
                            "Group {group_name}"
                        ))))?;
                    let user_group = UserGroup::Entity::find()
                        .filter(UserGroup::Column::UserId.eq(user_id))
                        .filter(UserGroup::Column::GroupId.eq(group.id))
                        .one(txn)
                        .await?
                        .ok_or(Error::AuthError(AuthError::PermissionDenied))?;
                    if !(user_group.role >= UserGroupRole::Write) {
                        return Err(Error::AuthError(AuthError::PermissionDenied));
                    }
                    group_ids.push(group.id);
                }

                let existing = Machines::Entity::find()
                    .filter(Machines::Column::MachineCode.eq(machine_code.clone()))
                    .one(txn)
                    .await?;

                let mut retired = None;
                let (agent, reused) = match existing {
                    Some(machine) => {
                        let agent = Agent::Entity::find_by_id(machine.agent_id)
                            .one(txn)
                            .await?
                            .ok_or_else(|| {
                                // The FK guarantees this cannot happen.
                                Error::Custom(format!(
                                    "Machine {machine_code} points at missing agent {}",
                                    machine.agent_id
                                ))
                            })?;
                        // A machine registering again means the process that
                        // held this agent is gone, whatever it still looks like
                        // it is doing. Closing it out here, in the transaction
                        // that re-adopts it, is what keeps its jobs and the
                        // tasks it was holding from waiting on a heartbeat that
                        // will never lapse. Deliberately after the permission
                        // checks above: a machine code must not be enough to
                        // tear an agent down.
                        let agent_state = agent.state;
                        if agent_state != AgentState::Offline {
                            retired = Some(
                                retire_agent_writes(
                                    txn,
                                    agent.id,
                                    agent.uuid,
                                    RetireCause::Reregistered,
                                    now,
                                )
                                .await?,
                            );
                        }
                        let mut agent: Agent::ActiveModel = agent.into();
                        agent.tags = Set(tags);
                        agent.labels = Set(labels);
                        // A restarting agent comes back Idle, with nothing
                        // assigned: the retirement above has already closed out
                        // whatever the previous process was running.
                        agent.state = Set(AgentState::Idle);
                        agent.assigned_task_suite_id = Set(None);
                        agent.last_heartbeat = Set(now);
                        agent.updated_at = Set(now);
                        let agent = agent.update(txn).await?;
                        tracing::info!(
                            agent_uuid = %agent.uuid,
                            previous_state = %agent_state,
                            "Re-adopting the agent already registered for this machine"
                        );

                        let mut machine: Machines::ActiveModel = machine.into();
                        machine.metadata = Set(metadata);
                        machine.last_seen_at = Set(now);
                        machine.update(txn).await?;

                        (agent, true)
                    }
                    None => {
                        let agent = Agent::ActiveModel {
                            uuid: Set(Uuid::new_v4()),
                            creator_id: Set(user_id),
                            tags: Set(tags),
                            labels: Set(labels),
                            state: Set(AgentState::Idle),
                            last_heartbeat: Set(now),
                            assigned_task_suite_id: Set(None),
                            created_at: Set(now),
                            updated_at: Set(now),
                            ..Default::default()
                        }
                        .insert(txn)
                        .await?;

                        Machines::ActiveModel {
                            agent_id: Set(agent.id),
                            machine_code: Set(machine_code),
                            metadata: Set(metadata),
                            first_seen_at: Set(now),
                            last_seen_at: Set(now),
                            ..Default::default()
                        }
                        .insert(txn)
                        .await?;

                        (agent, false)
                    }
                };

                // Grant group access to the agent. A re-registration rewrites all group access
                GroupAgent::Entity::delete_many()
                    .filter(GroupAgent::Column::AgentId.eq(agent.id))
                    .exec(txn)
                    .await?;

                // Admin first, so an admin group that also appears in `groups`
                // keeps Admin instead of being written as Write.
                let mut granted = HashSet::with_capacity(group_ids.len() + 1);
                let grants = std::iter::once((admin_group.id, GroupAgentRole::Admin))
                    .chain(group_ids.into_iter().map(|id| (id, GroupAgentRole::Write)));
                for (group_id, role) in grants {
                    if !granted.insert(group_id) {
                        continue;
                    }
                    GroupAgent::ActiveModel {
                        group_id: Set(group_id),
                        agent_id: Set(agent.id),
                        role: Set(role),
                        ..Default::default()
                    }
                    .insert(txn)
                    .await?;
                }

                Ok((agent.uuid, agent.id, reused, retired))
            })
        })
        .await?;

    // Before the heartbeat below: the follow-through stops tracking the agent
    // that just went, and the fresh one has to be tracked after that, not
    // before.
    if let Some(retired) = retired {
        retire_agent_post_commit(
            pool,
            agent_id,
            agent_uuid,
            RetireCause::Reregistered,
            retired,
        )
        .await;
    }

    // An agent is a long-lived daemon: without an explicit lifetime its token
    // never expires.
    let token = generate_worker_token(agent_uuid.to_string(), 0, req.lifetime)?;

    // Start (or restart) liveness tracking for this agent.
    let _ = pool
        .agent_heartbeat_queue_tx
        .send(AgentHeartbeatOp::Heartbeat(agent_id));

    let notification_counter = AgentWsRouter::counter(&pool.ws_router_tx, agent_uuid)
        .await
        .unwrap_or_default();

    Ok(RegisterAgentResp {
        agent_uuid,
        token,
        notification_counter,
        reused,
    })
}

/// Query agents visible to the user. Requires at least Read in the group.
pub async fn user_query_agents(
    user_id: i64,
    pool: &InfraPool,
    mut query: AgentsQueryReq,
) -> Result<AgentsQueryResp> {
    if query.group_name.is_none() {
        let user = User::Entity::find_by_id(user_id)
            .one(&pool.db)
            .await?
            .ok_or(Error::ApiError(ApiError::NotFound("User".to_string())))?;
        query.group_name = Some(user.username);
    }
    let group_name = query.group_name.clone().unwrap();

    let group = Group::Entity::find()
        .filter(Group::Column::GroupName.eq(&group_name))
        .one(&pool.db)
        .await?
        .ok_or(Error::ApiError(ApiError::NotFound(format!(
            "Group {group_name}"
        ))))?;
    let authorized = UserGroup::Entity::find()
        .filter(UserGroup::Column::UserId.eq(user_id))
        .filter(UserGroup::Column::GroupId.eq(group.id))
        .filter(UserGroup::Column::Role.gte(UserGroupRole::Read))
        .one(&pool.db)
        .await?
        .is_some();
    if !authorized {
        return Err(Error::ApiError(ApiError::NotFound(format!(
            "User doesn't have permission or group with name {group_name}"
        ))));
    }

    let mut stmt = Query::select();
    if query.count {
        stmt.expr(Expr::col((Agent::Entity, Agent::Column::Uuid)).count());
    } else {
        stmt.columns([
            (Agent::Entity, Agent::Column::Uuid),
            (Agent::Entity, Agent::Column::Tags),
            (Agent::Entity, Agent::Column::Labels),
            (Agent::Entity, Agent::Column::State),
            (Agent::Entity, Agent::Column::LastHeartbeat),
            (Agent::Entity, Agent::Column::CreatedAt),
            (Agent::Entity, Agent::Column::UpdatedAt),
        ])
        .expr_as(
            Expr::col((User::Entity, User::Column::Username)),
            Alias::new("creator_username"),
        )
        .expr_as(
            Expr::col((TaskSuites::Entity, TaskSuites::Column::Uuid)),
            Alias::new("assigned_suite_uuid"),
        )
        .column((Machines::Entity, Machines::Column::MachineCode))
        .column((Machines::Entity, Machines::Column::Metadata));
    }

    stmt.from(Agent::Entity)
        .join(
            sea_orm::JoinType::Join,
            GroupAgent::Entity,
            Expr::col((GroupAgent::Entity, GroupAgent::Column::AgentId))
                .eq(Expr::col((Agent::Entity, Agent::Column::Id))),
        )
        .join(
            sea_orm::JoinType::Join,
            User::Entity,
            Expr::col((User::Entity, User::Column::Id))
                .eq(Expr::col((Agent::Entity, Agent::Column::CreatorId))),
        )
        .join(
            sea_orm::JoinType::LeftJoin,
            Machines::Entity,
            Expr::col((Machines::Entity, Machines::Column::AgentId))
                .eq(Expr::col((Agent::Entity, Agent::Column::Id))),
        )
        .join(
            sea_orm::JoinType::LeftJoin,
            TaskSuites::Entity,
            Expr::col((TaskSuites::Entity, TaskSuites::Column::Id)).eq(Expr::col((
                Agent::Entity,
                Agent::Column::AssignedTaskSuiteId,
            ))),
        )
        .and_where(Expr::col((GroupAgent::Entity, GroupAgent::Column::GroupId)).eq(group.id));

    if let Some(ref tags) = query.tags {
        let tags: Vec<String> = tags.iter().cloned().collect();
        stmt.and_where(Expr::col((Agent::Entity, Agent::Column::Tags)).contains(tags));
    }
    if let Some(ref labels) = query.labels {
        let labels: Vec<String> = labels.iter().cloned().collect();
        stmt.and_where(Expr::col((Agent::Entity, Agent::Column::Labels)).contains(labels));
    }
    if let Some(ref states) = query.states {
        let states: Vec<AgentState> = states.iter().copied().collect();
        stmt.and_where(Expr::col((Agent::Entity, Agent::Column::State)).eq(PgFunc::any(states)));
    }
    if let Some(ref creator_username) = query.creator_username {
        stmt.and_where(
            Expr::col((User::Entity, User::Column::Username)).eq(creator_username.clone()),
        );
    }
    if let Some(limit) = query.limit {
        stmt.limit(limit);
    }
    if let Some(offset) = query.offset {
        stmt.offset(offset);
    }

    let builder = pool.db.get_database_backend();
    if query.count {
        let count = CountQuery::find_by_statement(builder.build(&stmt))
            .one(&pool.db)
            .await?
            .map(|c| c.count as u64)
            .unwrap_or(0);
        Ok(AgentsQueryResp {
            count,
            agents: vec![],
            group_name,
        })
    } else {
        let agents = AgentInfo::find_by_statement(builder.build(&stmt))
            .all(&pool.db)
            .await?;
        Ok(AgentsQueryResp {
            count: agents.len() as u64,
            agents,
            group_name,
        })
    }
}

/// Shut an agent down.
///
/// - `Graceful`: ask the agent to stop. An idle agent is parked `Offline` now; a
///   busy one winds its job down — the tasks it is running finish and commit,
///   cleanup runs, whatever it had claimed but not started is reclaimed — and it
///   goes `Offline` when its heartbeat stops. It does *not* drain the rest of
///   the suite first; other agents pick that up.
/// - `Force`: park it `Offline` immediately, kill its in-flight jobs, and
///   reclaim its uncommitted tasks so other agents re-run them.
pub async fn user_shutdown_agent_by_uuid(
    user_id: i64,
    agent_uuid: Uuid,
    op: AgentShutdownOp,
    pool: &InfraPool,
) -> Result<()> {
    let now = TimeDateTimeWithTimeZone::now_utc();

    // Fetch and authorize in one lookup
    let agent = Agent::Entity::find()
        .join_rev(sea_orm::JoinType::Join, GroupAgent::Relation::Agents.def())
        .join(sea_orm::JoinType::Join, GroupAgent::Relation::Groups.def())
        .join(sea_orm::JoinType::Join, Group::Relation::UserGroup.def())
        .filter(Agent::Column::Uuid.eq(agent_uuid))
        .filter(GroupAgent::Column::Role.eq(GroupAgentRole::Admin))
        .filter(UserGroup::Column::UserId.eq(user_id))
        .filter(UserGroup::Column::Role.eq(UserGroupRole::Admin))
        .one(&pool.db)
        .await?
        .ok_or(Error::ApiError(ApiError::NotFound(format!(
            "User doesn't have permission or agent with uuid {agent_uuid}"
        ))))?;

    match op {
        AgentShutdownOp::Force => {
            retire_agent(pool, agent.id, RetireCause::ForceShutdown).await?;
        }
        AgentShutdownOp::Graceful => {
            if agent.assigned_task_suite_id.is_none() {
                heartbeat::mark_offline(pool, agent.id, now).await?;
                let _ = pool
                    .agent_heartbeat_queue_tx
                    .send(AgentHeartbeatOp::Remove(agent.id));
            } else {
                // Busy: let it finish. Its own shutdown handling stops it from taking a
                // next suite; liveness tracking stays on so a stall still times out.
                tracing::info!(
                    agent_uuid = %agent.uuid,
                    "Graceful shutdown requested while busy — the agent will stop after its current job"
                );
            }
        }
    }

    AgentWsRouter::notify(
        &pool.ws_router_tx,
        agent.uuid,
        AgentNotification::Shutdown {
            graceful: matches!(op, AgentShutdownOp::Graceful),
        },
    );

    Ok(())
}

/// Why an agent is being retired, which is also which terminal its jobs get.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetireCause {
    /// Its heartbeat lapsed: the process is gone, or unreachable, which for our
    /// purposes is the same thing.
    HeartbeatTimeout,
    /// A user forced it off.
    ForceShutdown,
    /// A new process registered against the same machine, so whatever was
    /// running under the old one is over whether it knows it or not.
    Reregistered,
}

impl RetireCause {
    fn into_job_terminal(self) -> SuiteJobState {
        match self {
            // The agent was told to stop; the jobs died with it, not on their own.
            Self::ForceShutdown => SuiteJobState::Killed,
            Self::HeartbeatTimeout | Self::Reregistered => SuiteJobState::Lost,
        }
    }
}

/// Everything that has to happen when an agent stops being usable, in one call.
///
/// Retiring is not a state change but a teardown, and the parts only make sense
/// together: the agent is parked `Offline` with no assignment, its in-flight
/// jobs get a coordinator-written terminal, the tasks it holds uncommitted go
/// back to `Ready`, the suites it was running forget it, its liveness tracking
/// stops, and the work it dropped is offered to whoever can take it now.
///
/// Idempotent by construction: every write is filtered on the state it is
/// leaving, so a second call finds nothing to do. An agent already `Offline` is
/// skipped outright.
pub async fn retire_agent(pool: &InfraPool, agent_id: i64, cause: RetireCause) -> Result<()> {
    let now = TimeDateTimeWithTimeZone::now_utc();

    let Some(agent) = Agent::Entity::find_by_id(agent_id).one(&pool.db).await? else {
        tracing::debug!(agent_id, "Agent is gone; nothing to retire");
        return Ok(());
    };
    if agent.state == AgentState::Offline {
        return Ok(());
    }
    let agent_uuid = agent.uuid;

    let outcome = pool
        .db
        .transaction::<_, RetiredWork, Error>(|txn| {
            Box::pin(
                async move { retire_agent_writes(txn, agent_id, agent_uuid, cause, now).await },
            )
        })
        .await?;

    retire_agent_post_commit(pool, agent_id, agent_uuid, cause, outcome).await;
    Ok(())
}

/// What a retirement freed: the suites whose jobs it ended, and the tasks it
/// handed back as `(task id, suite id, priority)`.
type RetiredWork = (Vec<i64>, Vec<(i64, Option<i64>, i32)>);

/// The database half of [`retire_agent`], for callers that need it inside a
/// transaction of their own — registration writes the returning agent in the
/// same transaction that closes out the departed one.
async fn retire_agent_writes<C: ConnectionTrait>(
    txn: &C,
    agent_id: i64,
    agent_uuid: Uuid,
    cause: RetireCause,
    now: TimeDateTimeWithTimeZone,
) -> Result<RetiredWork> {
    let suite_ids =
        job::terminate_agent_jobs(txn, agent_id, cause.into_job_terminal(), now).await?;

    Agent::Entity::update_many()
        .col_expr(Agent::Column::State, Expr::value(AgentState::Offline))
        .col_expr(Agent::Column::AssignedTaskSuiteId, Expr::value(None::<i64>))
        .col_expr(Agent::Column::UpdatedAt, Expr::value(now))
        .filter(Agent::Column::Id.eq(agent_id))
        .exec(txn)
        .await?;

    // Unscoped: the agent is gone from every suite at once, not just the one job
    // we happened to be looking at.
    let reclaimed = job::reclaim_agent_tasks(txn, agent_uuid, None, now).await?;

    Ok((suite_ids, reclaimed))
}

/// Everything outside the database that a retirement owes: drop the agent from
/// the suites it was running, stop tracking its liveness, and offer the work it
/// dropped to whoever can take it now.
///
/// Runs after the writes have committed, so nothing here can be undone by a
/// rollback.
async fn retire_agent_post_commit(
    pool: &InfraPool,
    agent_id: i64,
    agent_uuid: Uuid,
    cause: RetireCause,
    (suite_ids, reclaimed): RetiredWork,
) {
    // Grouped before the jobs are closed, because closing one checks what it was
    // counted holding against what it just gave back.
    let reclaimed_count = reclaimed.len();
    let mut by_suite: HashMap<i64, Vec<(i64, i32)>> = HashMap::new();
    for (task_id, suite_id, priority) in reclaimed {
        let Some(suite_id) = suite_id else { continue };
        by_suite
            .entry(suite_id)
            .or_default()
            .push((task_id, priority));
    }

    for suite_id in &suite_ids {
        let reclaimed = by_suite.get(suite_id).map_or(0, Vec::len);
        pool.suite_queues.close(*suite_id, agent_id, reclaimed);
    }
    let _ = pool
        .agent_heartbeat_queue_tx
        .send(AgentHeartbeatOp::Remove(agent_id));

    tracing::info!(
        agent_id,
        %agent_uuid,
        ?cause,
        jobs = suite_ids.len(),
        reclaimed = reclaimed_count,
        "Retired an agent"
    );

    // Offer what it dropped, per suite — including the suites whose jobs ended
    // holding nothing, since a job leaving may itself be what frees the suite up
    // for somebody else.
    let mut suites: HashSet<i64> = suite_ids.into_iter().collect();
    suites.extend(by_suite.keys().copied());
    for suite_id in suites {
        notify_suite_tasks_ready(
            pool,
            suite_id,
            by_suite.remove(&suite_id).unwrap_or_default(),
        )
        .await;
    }
}

/// Stop the job `agent_uuid` is running now, leaving the agent up.
///
/// Authorized like a shutdown: only an Admin of the agent's admin group. The
/// suite-scoped route ([`crate::service::suite::user_stop_suite_job`]) reaches
/// the same core through the suite's owner instead.
pub async fn user_stop_agent_job(
    user_id: i64,
    agent_uuid: Uuid,
    op: StopJobOp,
    pool: &InfraPool,
) -> Result<StopAgentJobResp> {
    let agent = Agent::Entity::find()
        .join_rev(sea_orm::JoinType::Join, GroupAgent::Relation::Agents.def())
        .join(sea_orm::JoinType::Join, GroupAgent::Relation::Groups.def())
        .join(sea_orm::JoinType::Join, Group::Relation::UserGroup.def())
        .filter(Agent::Column::Uuid.eq(agent_uuid))
        .filter(GroupAgent::Column::Role.eq(GroupAgentRole::Admin))
        .filter(UserGroup::Column::UserId.eq(user_id))
        .filter(UserGroup::Column::Role.eq(UserGroupRole::Admin))
        .one(&pool.db)
        .await?
        .ok_or(Error::ApiError(ApiError::NotFound(format!(
            "User doesn't have permission or agent with uuid {agent_uuid}"
        ))))?;

    // At most one by construction — `accept` refuses a busy agent — so the
    // ordering only makes the answer deterministic if that ever slips.
    let job = SuiteAgentJobs::Entity::find()
        .filter(SuiteAgentJobs::Column::AgentId.eq(agent.id))
        .filter(SuiteAgentJobs::Column::State.is_in(job::IN_FLIGHT))
        .order_by_desc(SuiteAgentJobs::Column::Id)
        .one(&pool.db)
        .await?;
    let Some(job) = job else {
        return Ok(StopAgentJobResp {
            stopped: false,
            suite_uuid: None,
            job_id: None,
        });
    };

    let suite_uuid = TaskSuites::Entity::find_by_id(job.task_suite_id)
        .one(&pool.db)
        .await?
        .map(|s| s.uuid)
        .ok_or_else(|| Error::Custom(format!("Job {} points at a missing suite", job.id)))?;

    stop_agent_job(pool, &agent, &job, suite_uuid, op).await
}

/// Stop one in-flight job, whoever asked. Both stop routes end here.
///
/// The agent is left `Idle` rather than `Offline`: the point of a stop is that
/// it goes back and picks a suite again, which is how a user preempts one onto
/// work that outranks what it started.
///
/// `Force` writes the terminal here so no later agent report is accepted;
/// `Graceful` writes nothing and lets the agent walk its own job to `Completed`,
/// which is what lets its running tasks commit.
pub(crate) async fn stop_agent_job(
    pool: &InfraPool,
    agent: &Agent::Model,
    job: &SuiteAgentJobs::Model,
    suite_uuid: Uuid,
    op: StopJobOp,
) -> Result<StopAgentJobResp> {
    let now = TimeDateTimeWithTimeZone::now_utc();
    let (agent_id, agent_uuid) = (agent.id, agent.uuid);
    let (job_handle, job_id) = (job.id, job.job_id);

    if op == StopJobOp::Force {
        let stopped = pool
            .db
            .transaction::<_, Option<Vec<(i64, Option<i64>, i32)>>, Error>(|txn| {
                Box::pin(async move {
                    // Locked, so the terminal check and the write below cannot
                    // straddle the agent's own `complete`.
                    let job_row = job::load_validate_job_locked(txn, job_handle, agent_id).await?;
                    // It finished under us — nothing to stop, and no error either.
                    if job_row.state.is_terminal() {
                        return Ok(None);
                    }

                    let suite_id = job_row.task_suite_id;
                    let mut job_row: SuiteAgentJobs::ActiveModel = job_row.into();
                    job_row.state = Set(SuiteJobState::Killed);
                    job_row.updated_at = Set(now);
                    job_row.update(txn).await?;

                    // Same guard as `agent_complete_job`: only release an agent
                    // still bound to this job's suite, never one teardown has
                    // already unassigned.
                    Agent::Entity::update_many()
                        .col_expr(Agent::Column::State, Expr::value(AgentState::Idle))
                        .col_expr(Agent::Column::AssignedTaskSuiteId, Expr::value(None::<i64>))
                        .col_expr(Agent::Column::UpdatedAt, Expr::value(now))
                        .filter(Agent::Column::Id.eq(agent_id))
                        .filter(Agent::Column::AssignedTaskSuiteId.eq(suite_id))
                        .exec(txn)
                        .await?;

                    // The kill is the last moment we know these will never be
                    // committed; scoped to this suite, as `complete` does.
                    let reclaimed =
                        job::reclaim_agent_tasks(txn, agent_uuid, Some(suite_id), now).await?;

                    Ok(Some(reclaimed))
                })
            })
            .await?;

        let Some(reclaimed) = stopped else {
            return Ok(StopAgentJobResp {
                stopped: false,
                suite_uuid: None,
                job_id: None,
            });
        };

        let suite_id = job.task_suite_id;
        pool.suite_queues.close(suite_id, agent_id, reclaimed.len());
        if !reclaimed.is_empty() {
            notify_suite_tasks_ready(
                pool,
                suite_id,
                reclaimed
                    .into_iter()
                    .map(|(id, _, priority)| (id, priority)),
            )
            .await;
        }
    }

    tracing::info!(
        agent_uuid = %agent_uuid,
        job_id,
        %suite_uuid,
        graceful = op == StopJobOp::Graceful,
        "Stopping an agent's job on request"
    );

    AgentWsRouter::notify(
        &pool.ws_router_tx,
        agent_uuid,
        AgentNotification::StopJob {
            suite_uuid,
            graceful: op == StopJobOp::Graceful,
        },
    );

    Ok(StopAgentJobResp {
        stopped: true,
        suite_uuid: Some(suite_uuid),
        job_id: Some(job_id),
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// Execution loop (agent-authed)
// ─────────────────────────────────────────────────────────────────────────────

/// Record a heartbeat and hand back everything the agent has not seen.
///
///
/// Heartbeat is used both to re-notify the agent of pending tasks, and also
/// use for syncing the counters so that if the agent has a counter ahead of
/// the coordinator, the coordinator must have been restarted, and we should
/// notify the agent to resync.
pub async fn agent_heartbeat(
    agent_id: i64,
    agent_uuid: Uuid,
    pool: &InfraPool,
    req: AgentHeartbeatReq,
) -> Result<AgentHeartbeatResp> {
    let now = TimeDateTimeWithTimeZone::now_utc();

    let _ = pool
        .agent_heartbeat_queue_tx
        .send(AgentHeartbeatOp::Heartbeat(agent_id));

    // The agent's own state wins: it is the authority on what it is doing.
    let agent = Agent::Entity::update_many()
        .col_expr(Agent::Column::State, Expr::value(req.state))
        .col_expr(Agent::Column::LastHeartbeat, Expr::value(now))
        .col_expr(Agent::Column::UpdatedAt, Expr::value(now))
        .filter(Agent::Column::Id.eq(agent_id))
        .exec_with_returning(&pool.db)
        .await?
        .into_iter()
        .next()
        .ok_or(Error::ApiError(ApiError::NotFound("Agent".to_string())))?;

    // An idle agent with nothing assigned and work waiting gets a fresh nudge.
    // The suite it would land on is not named: it is re-picked in `accept`, and
    // by then a reservation may have moved the answer on.
    if req.state == AgentState::Idle && agent.assigned_task_suite_id.is_none() {
        let blocked = pool.suite_queues.blocked_for(agent_id);
        if matching::agent_has_available_suite(&pool.db, agent_id, &blocked).await? {
            AgentWsRouter::notify(
                &pool.ws_router_tx,
                agent_uuid,
                AgentNotification::SuiteAvailable,
            );
        }
    }

    // The agent claiming a higher notification id than we ever issued means our
    // sequence restarted with the process; hand it our boot id and counter.
    if let Some(counter) = AgentWsRouter::counter(&pool.ws_router_tx, agent_uuid).await {
        if req.last_notification_id > counter {
            tracing::warn!(
                agent_uuid = %agent_uuid,
                agent_counter = req.last_notification_id,
                coordinator_counter = counter,
                "Notification counter desync — sending CounterSync"
            );
            AgentWsRouter::notify(
                &pool.ws_router_tx,
                agent_uuid,
                AgentNotification::CounterSync {
                    counter,
                    boot_id: pool.boot_uuid,
                },
            );
        }
    }

    // Any unacked messages get sent in batch in heartbeat response
    let notifications = AgentWsRouter::pending_notifications(
        &pool.ws_router_tx,
        agent_uuid,
        req.last_notification_id,
    )
    .await;

    Ok(AgentHeartbeatResp { notifications })
}

/// A suite gained claimable tasks: queue them and offer the suite.
///
/// The ids are a dispatch hint for whoever claims next; the tasks themselves are
/// already `Ready` in the database, which is what any claim actually reads.
pub async fn notify_suite_tasks_ready(
    pool: &InfraPool,
    suite_id: i64,
    tasks: impl IntoIterator<Item = (i64, i32)>,
) {
    pool.suite_queues.push_ready(suite_id, tasks);
    notify_suite_available(pool, suite_id).await;
}

/// Offer a suite that has work: to the jobs already running it, and to idle
/// agents only for what those jobs cannot take.
///
/// **The jobs on the suite are asked first, fullest first.** Each has a
/// provisioned workspace and room to spare; an idle agent would pay a whole
/// provision hook to run what they can absorb in a round trip. Only as many as
/// it takes to cover the queue are told, so work packs onto the jobs that are
/// nearly full rather than spreading a task each across the fleet
/// ([`queue::SuiteQueues::follow_up_offer`]).
///
/// **An offer they can cover reserves the suite** ([`queue::RESERVATION_WINDOW`]),
/// so an idle agent cannot open a second job for work that is spoken for. One
/// they cannot is a queue that more agents genuinely help with: the idle agents
/// are told in the same round, and any window in force is released rather than
/// holding them out of work nobody warm is going to take.
///
/// Idle agents are told unaddressed, because which suite is best for any of them
/// is a question answered in `accept`, not here.
///
/// TODO: check according to agent scheduling strategy, and notify some agents
/// running low-priority suites to stop the job and switch to this suite
pub async fn notify_suite_available(pool: &InfraPool, suite_id: i64) {
    let offer = pool.suite_queues.follow_up_offer(suite_id);
    if !offer.agents.is_empty() {
        let suite_uuid = match TaskSuites::Entity::find_by_id(suite_id).one(&pool.db).await {
            Ok(Some(suite)) => suite.uuid,
            Ok(None) => return,
            Err(e) => {
                tracing::error!(suite_id, "Failed to load suite for agent notification: {e}");
                return;
            }
        };
        if offer.covered {
            pool.suite_queues.reserve(
                suite_id,
                offer.agents.iter().map(|(agent_id, _)| *agent_id).collect(),
                queue::RESERVATION_WINDOW,
            );
        }
        tracing::debug!(
            suite_id,
            jobs = offer.agents.len(),
            covered = offer.covered,
            "Offering the suite's new work to the jobs already running it"
        );
        for (_, agent_uuid) in &offer.agents {
            AgentWsRouter::notify(
                &pool.ws_router_tx,
                *agent_uuid,
                AgentNotification::TasksAvailable { suite_uuid },
            );
        }
        if offer.covered {
            return;
        }
    }

    // Nothing already running job for this suite can take all of it — a suite with no job at all included —
    // so the rest is work worth provisioning for, through a window opened for a
    // smaller queue a moment ago if need be.
    pool.suite_queues.release_reservation(suite_id);

    let agents = match matching::idle_eligible_agent_uuids(&pool.db, suite_id).await {
        Ok(agents) => agents,
        Err(e) => {
            tracing::error!(suite_id, "Failed to resolve eligible agents: {e}");
            return;
        }
    };
    for agent_uuid in agents {
        AgentWsRouter::notify(
            &pool.ws_router_tx,
            agent_uuid,
            AgentNotification::SuiteAvailable,
        );
    }
}

async fn suite_to_spec<C: ConnectionTrait>(
    db: &C,
    suite: TaskSuites::Model,
) -> Result<TaskSuiteSpec> {
    let group = Group::Entity::find_by_id(suite.group_id)
        .one(db)
        .await?
        .ok_or_else(|| Error::ApiError(ApiError::NotFound("Group of the suite".to_string())))?;
    let worker_schedule: WorkerSchedulePlan = serde_json::from_value(suite.worker_schedule)?;
    let exec_hooks: Option<ExecHooks> = suite.exec_hooks.map(serde_json::from_value).transpose()?;

    Ok(TaskSuiteSpec {
        uuid: suite.uuid,
        name: suite.name,
        description: suite.description,
        group_name: group.group_name,
        tags: suite.tags,
        labels: suite.labels,
        priority: suite.priority,
        worker_schedule,
        exec_hooks,
        state: suite.state,
        total_tasks: suite.total_tasks,
        incomplete_tasks: suite.incomplete_tasks,
    })
}

/// Pick a suite and claim it in one transaction: agent `Idle → Provisioning`, a
/// fresh job row, and the spec to run. Choosing under the suite's row lock is
/// what leaves no window for another agent to take it in between.
///
/// `req.suite_uuid` is a preference. A hint that no longer points at runnable
/// work — drained, cancelled, no longer this agent's, or simply gone — falls
/// through to the best available suite rather than answering "nothing".
///
/// Rejections that are the coordinator's decision rather than a client error
/// (nothing available, agent already busy) come back as `accepted: false` with a
/// reason, not an HTTP error.
pub async fn agent_accept_suite(
    agent_id: i64,
    agent_uuid: Uuid,
    pool: &InfraPool,
    req: AcceptSuiteReq,
) -> Result<AcceptSuiteResp> {
    let now = TimeDateTimeWithTimeZone::now_utc();
    // Read before the transaction opens: an in-memory answer that is at most one
    // round trip stale, and the window it comes from is a preference, not a
    // guarantee — the suite's row lock is still what settles who gets it.
    let blocked = pool.suite_queues.blocked_for(agent_id);

    let outcome = pool
        .db
        .transaction::<_, std::result::Result<(TaskSuiteSpec, i64, i32, i64), String>, Error>(
            |txn| {
                let blocked = blocked;
                Box::pin(async move {
                    // One suite at a time, checked before anything is locked. A busy
                    // agent that asks again is racing its own state, not erroring.

                    let agent = Agent::Entity::find_by_id(agent_id)
                        .one(txn)
                        .await?
                        .ok_or(Error::ApiError(ApiError::NotFound("Agent".to_string())))?;
                    if agent.assigned_task_suite_id.is_some() || agent.state.is_busy() {
                        return Ok(Err(format!(
                            "Agent is already {} and cannot accept another suite",
                            agent.state
                        )));
                    }

                    // What the agent asked for, if it is still worth running.
                    let requested = match req.suite_uuid {
                        Some(suite_uuid) => {
                            lock_runnable_suite(
                                txn,
                                agent_id,
                                SuiteTarget::Uuid(suite_uuid),
                                &blocked,
                            )
                            .await?
                        }
                        None => None,
                    };
                    // Otherwise — or if the hint went stale — the best on offer.
                    let suite = match requested {
                        Some(suite) => suite,
                        None => {
                            match matching::best_available_suite_id(txn, agent_id, &blocked).await?
                            {
                                Some(suite_id) => {
                                    match lock_runnable_suite(
                                        txn,
                                        agent_id,
                                        SuiteTarget::Id(suite_id),
                                        &blocked,
                                    )
                                    .await?
                                    {
                                        Some(suite) => suite,
                                        // Drained between the pick and the lock.
                                        None => {
                                            return Ok(Err(
                                                "No suite is available for this agent".to_string()
                                            ))
                                        }
                                    }
                                }
                                None => {
                                    return Ok(Err(
                                        "No suite is available for this agent".to_string()
                                    ))
                                }
                            }
                        }
                    };

                    let suite_id = suite.id;
                    let spec = suite_to_spec(txn, suite).await?;

                    let mut agent: Agent::ActiveModel = agent.into();
                    agent.assigned_task_suite_id = Set(Some(suite_id));
                    agent.updated_at = Set(now);
                    agent.update(txn).await?;

                    let job = job::create_job(txn, suite_id, agent_id, now).await?;
                    Ok(Ok((spec, job.id, job.job_id, suite_id)))
                })
            },
        )
        .await?;

    match outcome {
        Ok((suite, job, job_id, suite_id)) => {
            // The suite's first job: nothing has been pushed at an entry that
            // did not exist, so the work already waiting has to be read in.
            if pool.suite_queues.open(
                suite_id,
                agent_id,
                agent_uuid,
                job_id,
                queue::task_budget(&suite.worker_schedule),
                0,
            ) {
                match queue::ready_tasks(&pool.db, vec![suite_id]).await {
                    Ok(tasks) => pool.suite_queues.push_ready(
                        suite_id,
                        tasks.into_iter().map(|(_, id, priority)| (id, priority)),
                    ),
                    // The job runs either way — claims read the database, not
                    // this. What suffers is the offer: until the next fetch's
                    // own check notices, the suite looks emptier than it is.
                    Err(e) => tracing::error!(
                        suite_id,
                        "Failed to read a suite's claimable tasks for its first job: {e}"
                    ),
                }
            }
            Ok(AcceptSuiteResp {
                accepted: true,
                suite: Some(suite),
                job: Some(job),
                job_id: Some(job_id),
                reason: None,
            })
        }
        Err(reason) => Ok(AcceptSuiteResp {
            accepted: false,
            suite: None,
            job: None,
            job_id: None,
            reason: Some(reason),
        }),
    }
}

/// How a candidate suite was arrived at: named by the agent, or picked for it.
enum SuiteTarget {
    Uuid(Uuid),
    Id(i64),
}

/// Lock one candidate suite and answer whether this agent may run it now.
///
/// `None` covers every way a candidate can fail — gone, not this agent's,
/// terminal, or drained — since the caller falls through to the next candidate
/// for all of them. The checks are re-run under the lock even for a suite
/// `best_available_suite_id` just returned: that query takes no locks, so its
/// answer can go stale before we take one.
/// `blocked` names the suites reserved for other agents, which are refused here
/// too: `best_available_suite_id` never offers one, but an agent may still ask
/// for one by uuid.
async fn lock_runnable_suite<C: ConnectionTrait>(
    txn: &C,
    agent_id: i64,
    target: SuiteTarget,
    blocked: &[i64],
) -> Result<Option<TaskSuites::Model>> {
    let query = TaskSuites::Entity::find();
    let suite = match target {
        SuiteTarget::Uuid(uuid) => query.filter(TaskSuites::Column::Uuid.eq(uuid)),
        SuiteTarget::Id(id) => query.filter(TaskSuites::Column::Id.eq(id)),
    }
    .lock_exclusive()
    .one(txn)
    .await?;

    let Some(suite) = suite else {
        return Ok(None);
    };
    if blocked.contains(&suite.id) {
        return Ok(None);
    }
    // Same predicate as `matching::suite_has_work`, which is what the picker
    // filters on: runnable state, plus a task this agent could claim right now.
    if !matches!(suite.state, TaskSuiteState::Open | TaskSuiteState::Closed) {
        return Ok(None);
    }
    let claimable = ActiveTasks::Entity::find()
        .filter(ActiveTasks::Column::TaskSuiteId.eq(suite.id))
        .filter(ActiveTasks::Column::State.eq(TaskState::Ready))
        .count(txn)
        .await?;
    if claimable == 0 {
        return Ok(None);
    }
    if !matching::is_agent_eligible(txn, suite.id, agent_id).await? {
        return Ok(None);
    }
    Ok(Some(suite))
}

/// Provisioning finished: job `Provisioning → Executing`, agent `Executing`.
pub async fn agent_start_job(agent_id: i64, pool: &InfraPool, job_handle: i64) -> Result<()> {
    advance_job(
        agent_id,
        pool,
        job_handle,
        SuiteJobState::Provisioning,
        SuiteJobState::Executing,
        AgentState::Executing,
    )
    .await
}

/// Tasks drained: job `Executing → Cleanup`, agent `Cleaning`.
pub async fn agent_enter_cleanup(agent_id: i64, pool: &InfraPool, job_handle: i64) -> Result<()> {
    advance_job(
        agent_id,
        pool,
        job_handle,
        SuiteJobState::Executing,
        SuiteJobState::Cleanup,
        AgentState::Cleaning,
    )
    .await
}

/// One ordered job transition plus the agent state that goes with it, in one
/// transaction so the pair cannot be observed (or crash) half-applied.
async fn advance_job(
    agent_id: i64,
    pool: &InfraPool,
    job_handle: i64,
    from: SuiteJobState,
    to: SuiteJobState,
    agent_state: AgentState,
) -> Result<()> {
    let now = TimeDateTimeWithTimeZone::now_utc();

    pool.db
        .transaction::<_, (), Error>(|txn| {
            Box::pin(async move {
                // The lock is what makes `expect_state` binding: a coordinator
                // terminal written between the read and the write would
                // otherwise drag a dead job's agent into a live-looking state.
                let job_row = job::load_validate_job_locked(txn, job_handle, agent_id).await?;
                job::expect_state(&job_row, from)?;

                let suite_id = job_row.task_suite_id;
                let mut job: SuiteAgentJobs::ActiveModel = job_row.into();
                job.state = Set(to);
                job.updated_at = Set(now);
                job.update(txn).await?;

                // Same guard as `agent_complete_job`: never write over an agent
                // that teardown has already unassigned.
                Agent::Entity::update_many()
                    .col_expr(Agent::Column::State, Expr::value(agent_state))
                    .col_expr(Agent::Column::UpdatedAt, Expr::value(now))
                    .filter(Agent::Column::Id.eq(agent_id))
                    .filter(Agent::Column::AssignedTaskSuiteId.eq(suite_id))
                    .exec(txn)
                    .await?;

                Ok(())
            })
        })
        .await?;

    Ok(())
}

/// Finish the job and release the agent back to `Idle`.
///
/// The agent reports only what it did — `Completed` or `Failed` with a reason.
/// `Lost` and `Killed` are the coordinator's to write. A job that is already
/// terminal answers 409 so the agent knows it was torn down under it.
///
/// Both writes — the job's terminal and the agent's release — land in one
/// transaction: a half-applied pair would leave the agent pointing at a finished
/// suite, which `agent_accept_suite` reads as "still busy" and which nothing
/// short of a restart would clear.
///
/// The same transaction reclaims whatever this agent still holds uncommitted in
/// the suite. A task it ran but never committed is in no state anything else
/// would move it out of — not terminal, so nothing archives it, and not `Ready`,
/// so nobody can claim it — and the job ending is the last moment we know it
/// will never be committed. Those tasks go back to `Ready` for a re-run, and
/// that run's results are dropped.
pub async fn agent_complete_job(
    agent_id: i64,
    agent_uuid: Uuid,
    pool: &InfraPool,
    req: CompleteJobReq,
) -> Result<CompleteJobResp> {
    let now = TimeDateTimeWithTimeZone::now_utc();
    let job_handle = req.job;

    let (job_id, terminal, released, reclaimed, suite_id) = pool
        .db
        .transaction::<_, (i32, SuiteJobState, bool, Vec<(i64, Option<i64>, i32)>, i64), Error>(
            |txn| {
                Box::pin(async move {
                    // Locked, so the terminal check and the write below cannot
                    // straddle a `Killed`/`Lost` written by teardown.
                    let job_row = job::load_validate_job_locked(txn, job_handle, agent_id).await?;
                    job::reject_if_terminal(&job_row)?;

                    let terminal = match &req.outcome {
                        SuiteJobOutcome::Completed => SuiteJobState::Completed,
                        SuiteJobOutcome::Failed { reason } => {
                            tracing::warn!(
                                job = job_handle,
                                job_id = job_row.job_id,
                                kind = ?reason.kind,
                                "Agent reported a failed job: {}",
                                reason.message
                            );
                            SuiteJobState::Failed
                        }
                    };

                    let suite_id = job_row.task_suite_id;
                    let job_id = job_row.job_id;
                    let mut job_row: SuiteAgentJobs::ActiveModel = job_row.into();
                    job_row.state = Set(terminal);
                    job_row.updated_at = Set(now);
                    job_row.update(txn).await?;

                    // Release the agent to Idle only while it is still bound to *this*
                    // job's suite. A force shutdown or a heartbeat timeout parks it
                    // `Offline` and clears the assignment; without this filter a
                    // completion racing that teardown would resurrect the agent as
                    // `Idle` with nothing tracking its liveness.
                    let released = Agent::Entity::update_many()
                        .col_expr(Agent::Column::State, Expr::value(AgentState::Idle))
                        .col_expr(Agent::Column::AssignedTaskSuiteId, Expr::value(None::<i64>))
                        .col_expr(Agent::Column::UpdatedAt, Expr::value(now))
                        .filter(Agent::Column::Id.eq(agent_id))
                        .filter(Agent::Column::AssignedTaskSuiteId.eq(suite_id))
                        .exec(txn)
                        .await?
                        .rows_affected
                        > 0;

                    // Scoped to this suite; what the agent holds elsewhere is left
                    // alone.
                    let reclaimed =
                        job::reclaim_agent_tasks(txn, agent_uuid, Some(suite_id), now).await?;

                    Ok((job_id, terminal, released, reclaimed, suite_id))
                })
            },
        )
        .await?;

    tracing::info!(
        "Agent finished a suite job: job_id {job_id} (handle {job_handle}), \
         terminal state: {terminal}"
    );
    if !released {
        tracing::info!(
            agent_id,
            job_id,
            "Agent was no longer assigned to the completed job's suite; left its state alone"
        );
    }
    // The job is over before anything else reads this: the suite must stop
    // counting it as somewhere to send work, and stop reserving on its behalf.
    // Every slot has joined by the time an agent calls this, so the count it was
    // holding and what it just gave back have no race left to disagree over.
    pool.suite_queues.close(suite_id, agent_id, reclaimed.len());

    if !reclaimed.is_empty() {
        tracing::warn!(
            agent_id,
            job_id,
            reclaimed = reclaimed.len(),
            "Agent completed its job holding uncommitted tasks; they were returned to Ready \
             and will be re-run, and that run's results are lost"
        );
        // Claimable again, and this agent is not the one to run them.
        notify_suite_tasks_ready(
            pool,
            suite_id,
            reclaimed
                .into_iter()
                .map(|(id, _, priority)| (id, priority)),
        )
        .await;
    }

    let blocked = pool.suite_queues.blocked_for(agent_id);
    let next_suite_available =
        matching::agent_has_available_suite(&pool.db, agent_id, &blocked).await?;
    Ok(CompleteJobResp {
        next_suite_available,
    })
}

/// Push a notification to every agent id in `agent_ids` (resolving uuids first).
pub(crate) async fn notify_agents_by_id(
    pool: &InfraPool,
    agent_ids: &[i64],
    event: AgentNotification,
) {
    if agent_ids.is_empty() {
        return;
    }
    let uuids = Agent::Entity::find()
        .select_only()
        .column(Agent::Column::Uuid)
        .filter(Agent::Column::Id.is_in(agent_ids.to_vec()))
        .into_tuple::<Uuid>()
        .all(&pool.db)
        .await;
    match uuids {
        Ok(uuids) => {
            for uuid in uuids {
                AgentWsRouter::notify(&pool.ws_router_tx, uuid, event.clone());
            }
        }
        Err(e) => tracing::error!("Failed to resolve agent uuids for notification: {e}"),
    }
}

/// On coordinator start, tell the router what boot it is on. Agents learn the
/// new `boot_id` from the `CounterSync` and reset their own sequence, so a
/// restart does not leave them ignoring notifications whose ids look stale.
pub async fn notify_agents_of_restart(pool: &InfraPool) -> Result<()> {
    let agents = Agent::Entity::find()
        .select_only()
        .column(Agent::Column::Uuid)
        .into_tuple::<Uuid>()
        .all(&pool.db)
        .await?;
    tracing::info!(
        agents = agents.len(),
        boot_id = %pool.boot_uuid,
        "Announcing coordinator restart to known agents"
    );
    for uuid in agents {
        AgentWsRouter::notify(
            &pool.ws_router_tx,
            uuid,
            AgentNotification::CounterSync {
                counter: 0,
                boot_id: pool.boot_uuid,
            },
        );
    }
    Ok(())
}
