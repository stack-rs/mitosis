//! Agents: registration, queries, heartbeats and shutdown for users, and the job loop for agents.
//!
//! ## The job loop
//!
//! ```text
//! register ──▶ heartbeat ──▶ accept suite ──▶ start ──▶ …tasks… ──▶ cleanup ──▶ complete
//!                  ▲                                                                │
//!                  └─────────────────────── Idle ◀───────────────────────────────────┘
//! ```
//!
//! Only `accept` claims a suite. One transaction picks the suite, checks it again, assigns it to
//! the agent, and creates a `suite_agent_jobs` row and its hook rows. The agent sends the job
//! handle on each later call. `start`, `cleanup` and `complete` move the job row forward.
//!
//! ## How work is offered
//!
//! When a suite gets ready tasks, [`notify_suite_available`] first notifies the jobs on the suite.
//! For a short time, the suite is reserved for these jobs. Idle agents get a notification only for
//! the tasks that these jobs cannot take, or for a suite without a job. This notification does not
//! name the suite: [`agent_accept_suite`] picks the best suite when the agent accepts one.
//! [`queue`] keeps the in-memory state for this.
//!
//! ## No automatic preemption
//!
//! An agent runs the suite that it accepted until the job ends. The priority of a suite only
//! changes which suite [`matching::best_available_suite_id`] picks when an agent looks for work.
//! To move an agent to a suite with a higher priority, a user stops its job
//! ([`user_stop_agent_job`]). The agent then picks a suite again.

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
    AgentNotification, AgentShutdownOp, AgentsQueryReq, AgentsQueryResp, CountQuery, ExecHooks,
    JobHook, RegisterAgentReq, RegisterAgentResp, StopAgentJobResp, StopJobOp, SuiteJobOutcome,
    TaskSuiteSpec, WorkerSchedulePlan,
};
use crate::service::auth::token::{generate_runner_token, TokenRole};
use crate::ws::AgentWsRouter;

use heartbeat::AgentHeartbeatOp;
use queue::SuiteQueues;

/// Register an agent. If the machine is already known, use its agent row again.
///
/// `machines.machine_code` and `machines.agent_id` are both unique, thus one machine has one agent
/// row for all its life. For a known machine, this function updates that agent row: new tags,
/// labels, groups and metadata, and a new token. The caller must be an Admin of `admin_group`, and
/// must have at least the Write role in each group of `groups`.
///
/// The request gives all the groups that can use the agent. Group access that is not in the request
/// is removed.
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

    let queues = pool.suite_queues.clone();
    let (agent_uuid, agent_id, reused, retired) = pool
        .db
        .transaction::<_, (Uuid, i64, bool, Option<RetiredWork>), Error>(move |txn| {
            Box::pin(async move {
                // Exactly one group has the Admin role on the agent, and only
                // its Admins can shut the agent down.
                let admin_group_name = match admin_group {
                    Some(name) => name,
                    None => {
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
                let admin_membership = UserGroup::Entity::find()
                    .filter(UserGroup::Column::UserId.eq(user_id))
                    .filter(UserGroup::Column::GroupId.eq(admin_group.id))
                    .one(txn)
                    .await?
                    .ok_or(Error::AuthError(AuthError::PermissionDenied))?;
                if admin_membership.role != UserGroupRole::Admin {
                    return Err(Error::AuthError(AuthError::PermissionDenied));
                }

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
                    if user_group.role < UserGroupRole::Write {
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
                                Error::Custom(format!(
                                    "Machine {machine_code} points at missing agent {}",
                                    machine.agent_id
                                ))
                            })?;
                        // A new registration from the same machine means that
                        // the previous agent process has stopped, whatever its
                        // state shows. Retire it in this transaction, so that
                        // its work is not held until its heartbeat times out.
                        // This comes after the permission checks, so that a
                        // known machine code alone cannot retire an agent.
                        let agent_state = agent.state;
                        if agent_state != AgentState::Offline {
                            retired = Some(
                                retire_agent_writes(
                                    txn,
                                    &queues,
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
                        agent.state = Set(AgentState::Idle);
                        agent.assigned_task_suite_id = Set(None);
                        agent.last_heartbeat = Set(now);
                        agent.updated_at = Set(now);
                        let agent = agent.update(txn).await?;
                        tracing::info!(
                            agent_uuid = %agent.uuid,
                            previous_state = %agent_state,
                            "Registering the known machine again with its agent row"
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

                GroupAgent::Entity::delete_many()
                    .filter(GroupAgent::Column::AgentId.eq(agent.id))
                    .exec(txn)
                    .await?;

                // The admin group goes first. If it is also in `groups`, it
                // keeps the Admin role and does not get a second Write row.
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

    // This must come before the heartbeat below. It stops tracking the previous process, and the
    // heartbeat then starts to track the new one.
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

    let token = generate_runner_token(agent_uuid.to_string(), 0, req.lifetime, TokenRole::Agent)?;

    let _ = pool
        .agent_heartbeat_queue_tx
        .send(AgentHeartbeatOp::Heartbeat(agent_id));

    // Notifications queued for the previous process do not apply to this one. The router applies
    // operations in order, thus the counter read below comes after the reset.
    AgentWsRouter::reset(&pool.ws_router_tx, agent_uuid);
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

/// Query the agents of a group. The caller must have at least the Read role in the group.
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

/// Shut an agent down. The caller must be an Admin of the admin group of the agent.
///
/// - `Graceful`: only tell the agent to stop. The agent finishes its current work and then retires
///   itself with `DELETE /agents`. Its heartbeat is still tracked, thus an agent that stops
///   responding still times out.
/// - `Force`: retire the agent now, then tell it to stop.
pub async fn user_shutdown_agent_by_uuid(
    user_id: i64,
    agent_uuid: Uuid,
    op: AgentShutdownOp,
    pool: &InfraPool,
) -> Result<()> {
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
        AgentShutdownOp::Graceful => tracing::info!(
            agent_uuid = %agent.uuid,
            "Graceful shutdown requested; the agent stops after its current work"
        ),
    }

    // A retired agent cannot authenticate, thus it cannot read this from a heartbeat. It gets the
    // notification only on this WebSocket that is already connected. If there is no websocket, the
    // next heartbeat will return not authenticated and the agent will shut itself down.
    AgentWsRouter::notify(
        &pool.ws_router_tx,
        agent.uuid,
        AgentNotification::Shutdown {
            graceful: matches!(op, AgentShutdownOp::Graceful),
        },
    );

    Ok(())
}

/// The reason to retire an agent. It also gives the terminal state of the jobs of the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetireCause {
    /// The agent did not send a heartbeat in time.
    HeartbeatTimeout,
    /// A user forced the agent to shut down.
    ForceShutdown,
    /// A new agent process registered from the same machine.
    Reregistered,
    /// The agent told the coordinator that it exits.
    SelfExit,
}

impl RetireCause {
    fn into_job_terminal(self) -> SuiteJobState {
        match self {
            // The coordinator stopped the jobs.
            Self::ForceShutdown => SuiteJobState::Killed,
            // An agent that exits normally completed its job before. `Lost` labels only the jobs
            // that it did not complete.
            Self::HeartbeatTimeout | Self::Reregistered | Self::SelfExit => SuiteJobState::Lost,
        }
    }
}

/// Retire an agent: mark it `Offline` without a suite, write a terminal state to its jobs that are
/// not terminal, put the tasks that it did not commit back to `Ready`, stop tracking its heartbeat,
/// remove its buffered notifications, and offer the returned work to other agents. Its token stops
/// working.
///
/// These steps occur together, because each step without the others leaves work that nothing can
/// claim.
///
/// This function does nothing for an agent that is already `Offline`, thus it is safe to call it
/// more than once.
pub async fn retire_agent(pool: &InfraPool, agent_id: i64, cause: RetireCause) -> Result<()> {
    let now = TimeDateTimeWithTimeZone::now_utc();

    let Some(agent) = Agent::Entity::find_by_id(agent_id).one(&pool.db).await? else {
        tracing::debug!(agent_id, "Agent not found; nothing to retire");
        return Ok(());
    };
    if agent.state == AgentState::Offline {
        return Ok(());
    }
    let agent_uuid = agent.uuid;

    let queues = pool.suite_queues.clone();
    let retired = pool
        .db
        .transaction::<_, RetiredWork, Error>(move |txn| {
            Box::pin(async move {
                retire_agent_writes(txn, &queues, agent_id, agent_uuid, cause, now).await
            })
        })
        .await?;

    retire_agent_post_commit(pool, agent_id, agent_uuid, cause, retired).await;
    Ok(())
}

/// The work that a retirement freed: the suites of the ended jobs, and the returned tasks as
/// `(task id, suite id, priority)`.
type RetiredWork = (Vec<i64>, Vec<(i64, Option<i64>, i32)>);

/// The database writes of [`retire_agent`]. Registration also calls this in its own transaction,
/// because it retires the previous agent and updates the agent row in one step.
async fn retire_agent_writes<C: ConnectionTrait>(
    txn: &C,
    queues: &SuiteQueues,
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

    // All the suites: the agent leaves each suite at the same time.
    let reclaimed = job::reclaim_agent_tasks(txn, agent_uuid, None, now).await?;
    // Add the tasks before the commit. See `SuiteQueues::push_ready`.
    queues.push_reclaimed(&reclaimed);

    Ok((suite_ids, reclaimed))
}

/// The steps of [`retire_agent`] outside the database. They run after the commit, because a
/// rollback cannot undo them.
async fn retire_agent_post_commit(
    pool: &InfraPool,
    agent_id: i64,
    agent_uuid: Uuid,
    cause: RetireCause,
    (suite_ids, reclaimed): RetiredWork,
) {
    // Count the returned tasks for each suite. `close` compares this count with the count of the
    // queue.
    let mut by_suite: HashMap<i64, usize> = HashMap::new();
    for suite_id in reclaimed.iter().filter_map(|(_, suite_id, _)| *suite_id) {
        *by_suite.entry(suite_id).or_default() += 1;
    }
    for suite_id in &suite_ids {
        let count = by_suite.get(suite_id).copied().unwrap_or(0);
        pool.suite_queues.close(*suite_id, agent_id, count);
    }
    let _ = pool
        .agent_heartbeat_queue_tx
        .send(AgentHeartbeatOp::Remove(agent_id));
    AgentWsRouter::reset(&pool.ws_router_tx, agent_uuid);
    tracing::info!(
        agent_id,
        %agent_uuid,
        ?cause,
        jobs = suite_ids.len(),
        reclaimed = reclaimed.len(),
        "Retired an agent"
    );

    // Offer the work again for each suite. This includes the suites whose jobs held no task,
    // because the end of a job can let a different agent take the suite.
    let mut suites: HashSet<i64> = suite_ids.into_iter().collect();
    suites.extend(by_suite.into_keys());
    for suite_id in suites {
        notify_suite_available(pool, suite_id).await;
    }
}

/// Stop the job that the agent runs now. The agent stays up. The caller must be an Admin of the
/// admin group of the agent, the same as for a shutdown.
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

    // `accept` refuses a busy agent, thus there is at most one job. The order only makes the
    // answer stable if there are more.
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

/// Stop one job that is not terminal. Both stop endpoints use this function.
///
/// The agent stays `Idle`, not `Offline`: after the stop, it picks a suite again. Thus a user can
/// move an agent to a suite with a higher priority.
///
/// `Force` writes the terminal state here, thus the coordinator rejects later reports for the job.
/// `Graceful` writes nothing: the agent moves the job to `Completed` itself, thus its running tasks
/// can commit.
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
        let queues = pool.suite_queues.clone();
        let stopped = pool
            .db
            .transaction::<_, Option<Vec<(i64, Option<i64>, i32)>>, Error>(move |txn| {
                Box::pin(async move {
                    // One statement, thus the terminal check and the write cannot occur on the two
                    // sides of a `Complete` of the agent.
                    let killed = job::transition_job(
                        txn,
                        job_handle,
                        agent_id,
                        &job::IN_FLIGHT,
                        SuiteJobState::Killed,
                        now,
                    )
                    .await?;
                    let Some(job_row) = killed else {
                        // A job that is missing or of a different agent is an error. A job that is
                        // already terminal is not: it ended first, thus there is nothing to stop.
                        job::load_validate_job(txn, job_handle, agent_id).await?;
                        return Ok(None);
                    };
                    hook::close_still_open_hooks(txn, vec![job_handle], now).await?;

                    let suite_id = job_row.task_suite_id;

                    // Release the agent only while it is still assigned to the suite of this job,
                    // the same as `agent_complete_job`.
                    Agent::Entity::update_many()
                        .col_expr(Agent::Column::State, Expr::value(AgentState::Idle))
                        .col_expr(Agent::Column::AssignedTaskSuiteId, Expr::value(None::<i64>))
                        .col_expr(Agent::Column::UpdatedAt, Expr::value(now))
                        .filter(Agent::Column::Id.eq(agent_id))
                        .filter(Agent::Column::AssignedTaskSuiteId.eq(suite_id))
                        .exec(txn)
                        .await?;

                    // After the kill, these tasks will never commit. Only this suite, the same as
                    // `Complete`.
                    let reclaimed =
                        job::reclaim_agent_tasks(txn, agent_uuid, Some(suite_id), now).await?;
                    // Add the tasks before the commit. See `SuiteQueues::push_ready`.
                    queues.push_reclaimed(&reclaimed);

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
            notify_suite_available(pool, suite_id).await;
        }
    }

    tracing::info!(
        agent_uuid = %agent_uuid,
        job_id,
        %suite_uuid,
        graceful = op == StopJobOp::Graceful,
        "Stopping a job of an agent on request"
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

/// Record a heartbeat and return the notifications that the agent has not acknowledged.
pub async fn agent_heartbeat(
    agent_id: i64,
    agent_uuid: Uuid,
    pool: &InfraPool,
    req: AgentHeartbeatReq,
) -> Result<AgentHeartbeatResp> {
    if req.state == AgentState::Offline {
        return Err(Error::ApiError(ApiError::InvalidRequest(
            "an agent exits with DELETE /agents, not with a heartbeat".to_string(),
        )));
    }
    let now = TimeDateTimeWithTimeZone::now_utc();

    // The agent knows best what it does, thus its reported state replaces the recorded one. The
    // filter on `Offline` prevents the heartbeat that is initiated from an agent that is in the process
    // of retiring to re-activate the agent row.
    let Some(agent) = Agent::Entity::update_many()
        .col_expr(Agent::Column::State, Expr::value(req.state))
        .col_expr(Agent::Column::LastHeartbeat, Expr::value(now))
        .col_expr(Agent::Column::UpdatedAt, Expr::value(now))
        .filter(Agent::Column::Id.eq(agent_id))
        .filter(Agent::Column::State.ne(AgentState::Offline))
        .exec_with_returning(&pool.db)
        .await?
        .into_iter()
        .next()
    else {
        return Err(Error::AuthError(AuthError::WrongCredentials));
    };

    let _ = pool
        .agent_heartbeat_queue_tx
        .send(AgentHeartbeatOp::Heartbeat(agent_id));

    // An idle agent without a suite gets a new `SuiteAvailable` if a suite has work, in case it
    // lost the earlier one.
    if req.state == AgentState::Idle
        && agent.assigned_task_suite_id.is_none()
        && matching::agent_has_available_suite(
            &pool.db,
            agent_id,
            &pool.suite_queues.blocked_for(agent_id),
        )
        .await?
    {
        AgentWsRouter::notify(
            &pool.ws_router_tx,
            agent_uuid,
            AgentNotification::SuiteAvailable,
        );
    }

    // If the agent has processed a higher id than the coordinator has issued, the coordinator has
    // restarted and its ids started again. Tell the agent the current counter and boot id.
    if let Some(counter) = AgentWsRouter::counter(&pool.ws_router_tx, agent_uuid).await {
        if req.last_notification_id > counter {
            tracing::warn!(
                agent_uuid = %agent_uuid,
                agent_counter = req.last_notification_id,
                coordinator_counter = counter,
                "Notification counter mismatch; sending CounterSync"
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

    let notifications = AgentWsRouter::pending_notifications(
        &pool.ws_router_tx,
        agent_uuid,
        req.last_notification_id,
    )
    .await;

    Ok(AgentHeartbeatResp { notifications })
}

/// Offer the ready tasks of a suite. First notify the jobs on the suite, then the `Idle` agents for
/// the tasks that these jobs cannot take.
///
/// A job on the suite is already provisioned. Thus it gets `TasksAvailable` before an idle agent
/// runs a provision hook. [`SuiteQueues::follow_up_offer`] selects the jobs. If they have room for
/// all the ready tasks, the suite is reserved for them ([`queue::RESERVATION_WINDOW`]), and no idle
/// agent gets a notification. If not, the reservation ends, and the idle agents get
/// `SuiteAvailable`.
///
/// `SuiteAvailable` does not name the suite. The agent gets the best suite for it when it accepts
/// one.
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
            "Offering the ready tasks of the suite to its jobs"
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

    // The jobs cannot take all the tasks, or the suite has no job. Thus a new job is necessary. End
    // the reservation of an earlier, smaller offer.
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

/// Pick a suite and claim it in one transaction: assign it to the agent, and create the job row and
/// its hook rows. The pick occurs under the lock of the suite row, thus no other agent can take the
/// suite between the pick and the claim.
///
/// `req.suite_uuid` is a preference. If that suite cannot run now (no ready task, cancelled, not
/// for this agent, or missing), the coordinator picks the best available suite.
///
/// When the agent gets no suite (no suite is available, or the agent already runs a job), the
/// response has `accepted: false` and a reason. This is not an HTTP error.
pub async fn agent_accept_suite(
    agent_id: i64,
    agent_uuid: Uuid,
    pool: &InfraPool,
    req: AcceptSuiteReq,
) -> Result<AcceptSuiteResp> {
    let now = TimeDateTimeWithTimeZone::now_utc();
    // Read this before the transaction. A reservation is only a preference, thus a value from a
    // short time before is sufficient.
    let blocked = pool.suite_queues.blocked_for(agent_id);

    let outcome = pool
        .db
        .transaction::<_, std::result::Result<(TaskSuiteSpec, i64, i32, i64, Vec<JobHook>), String>, Error>(
            |txn| {
                Box::pin(async move {
                    // One suite at a time. A busy agent that asks again is not an error.
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

                    // The suite that the agent asked for, if it can still run.
                    let requested = match req.suite_uuid {
                        Some(suite_uuid) => {
                            runnable_suite(txn, agent_id, SuiteTarget::Uuid(suite_uuid), &blocked)
                                .await?
                        }
                        None => None,
                    };
                    // If not, the best available suite.
                    let suite = match requested {
                        Some(suite) => suite,
                        None => {
                            let picked = match matching::best_available_suite_id(
                                txn, agent_id, &blocked,
                            )
                            .await?
                            {
                                Some(suite_id) => {
                                    runnable_suite(
                                        txn,
                                        agent_id,
                                        SuiteTarget::Id(suite_id),
                                        &blocked,
                                    )
                                    .await?
                                }
                                None => None,
                            };
                            // `None` after a pick: the suite lost its work between the pick and
                            // the check.
                            let Some(suite) = picked else {
                                return Ok(Err("No suite is available for this agent".to_string()));
                            };
                            suite
                        }
                    };

                    let suite_id = suite.id;
                    // Lock order: the job row locks the suite row, before the agent row.
                    let job = job::create_job(txn, suite_id, agent_id, now).await?;
                    let spec = suite_to_spec(txn, suite).await?;
                    let hooks =
                        hook::create_job_hooks(txn, job.id, spec.exec_hooks.as_ref(), now).await?;

                    let mut agent: Agent::ActiveModel = agent.into();
                    agent.assigned_task_suite_id = Set(Some(suite_id));
                    agent.updated_at = Set(now);
                    agent.update(txn).await?;

                    Ok(Ok((spec, job.id, job.job_id, suite_id, hooks)))
                })
            },
        )
        .await?;

    match outcome {
        Ok((suite, job, job_id, suite_id, hooks)) => {
            let span = tracing::Span::current();
            span.record("suite_uuid", tracing::field::display(suite.uuid));
            span.record("job_id", job_id);

            // The first job of the suite creates the queue entry. Read the ready tasks from the
            // database, because the queue ignored them before the entry existed.
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
                    // The job can still claim tasks, because a claim reads the database. Only the
                    // offers count too few ready tasks.
                    Err(e) => tracing::error!(
                        suite_id,
                        "Failed to read the ready tasks of a suite for its first job: {e}"
                    ),
                }
            }
            Ok(AcceptSuiteResp {
                accepted: true,
                suite: Some(suite),
                job: Some(job),
                job_id: Some(job_id),
                hooks,
                reason: None,
            })
        }
        Err(reason) => Ok(AcceptSuiteResp {
            accepted: false,
            suite: None,
            job: None,
            job_id: None,
            hooks: Vec::new(),
            reason: Some(reason),
        }),
    }
}

/// How the agent got a candidate suite: it named the suite, or the coordinator picked it.
enum SuiteTarget {
    Uuid(Uuid),
    Id(i64),
}

/// Return the suite if this agent can run it now. `None` for each reason that it cannot: missing,
/// not for this agent, in the wrong state, no ready task, or reserved for other agents (`blocked`).
/// The caller then tries the next candidate. `best_available_suite_id` skips a reserved suite, but
/// an agent can still ask for one by its UUID.
///
/// This function takes no lock. A lock taken before the checks stays when the function returns
/// `None`. Then an agent whose preferred suite cannot run holds that suite while it locks a
/// different one, and two agents can deadlock. The checks are only advice: the hard rule on this
/// path is the unique index on the job id, and `create_job` locks the suite at the end.
async fn runnable_suite<C: ConnectionTrait>(
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
    .one(txn)
    .await?;

    let Some(suite) = suite else {
        return Ok(None);
    };
    if blocked.contains(&suite.id) {
        return Ok(None);
    }
    // The same rule as `matching::suite_has_work`, which the pick uses.
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

/// The provision hook is done: job `Provisioning → Executing`, agent `Executing`.
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

/// The agent claims no more tasks: job `Executing → Cleanup`, agent `Cleaning`.
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

/// One ordered job transition and the agent state that goes with it, in one transaction.
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
                // The source state is in the `WHERE`. Thus a terminal state that the coordinator
                // writes at the same time cannot give the agent of a closed job a live state.
                let advanced =
                    job::transition_job(txn, job_handle, agent_id, &[from], to, now).await?;
                let Some(job_row) = advanced else {
                    return Err(job::explain_failed_transition(
                        txn,
                        job_handle,
                        agent_id,
                        Some(from),
                    )
                    .await);
                };

                // Do not change an agent that a retirement already released.
                Agent::Entity::update_many()
                    .col_expr(Agent::Column::State, Expr::value(agent_state))
                    .col_expr(Agent::Column::UpdatedAt, Expr::value(now))
                    .filter(Agent::Column::Id.eq(agent_id))
                    .filter(Agent::Column::AssignedTaskSuiteId.eq(job_row.task_suite_id))
                    .exec(txn)
                    .await?;

                Ok(())
            })
        })
        .await?;

    Ok(())
}

/// End the job and release the agent to `Idle`. Returns whether a different suite is available
/// for this agent now.
///
/// The agent reports only `Completed` or `Failed`. Only the coordinator writes `Lost` and `Killed`.
/// A job that is already terminal answers `NotFound`, thus the agent knows that the job was closed.
///
/// One transaction writes the terminal state, closes the open hooks, releases the agent, and puts
/// the tasks that the agent did not commit in this suite back to `Ready`. If the release does not
/// occur, the agent stays assigned to a closed suite, and `accept` rejects it. The tasks must go
/// back, because nothing else moves a task that ran without a commit.
pub async fn agent_complete_job(
    agent_id: i64,
    agent_uuid: Uuid,
    pool: &InfraPool,
    job_handle: i64,
    outcome: SuiteJobOutcome,
) -> Result<bool> {
    let now = TimeDateTimeWithTimeZone::now_utc();

    let queues = pool.suite_queues.clone();
    let (job_id, terminal, released, reclaimed, suite_id) = pool
        .db
        .transaction::<_, (i32, SuiteJobState, bool, Vec<(i64, Option<i64>, i32)>, i64), Error>(
            move |txn| {
                Box::pin(async move {
                    let terminal = match &outcome {
                        SuiteJobOutcome::Completed => SuiteJobState::Completed,
                        SuiteJobOutcome::Failed { .. } => SuiteJobState::Failed,
                    };

                    // One statement, thus the terminal check and the write cannot occur on the two
                    // sides of a `Killed` or `Lost` from a retirement.
                    let finished = job::transition_job(
                        txn,
                        job_handle,
                        agent_id,
                        &job::IN_FLIGHT,
                        terminal,
                        now,
                    )
                    .await?;
                    let Some(job_row) = finished else {
                        return Err(job::explain_failed_transition(
                            txn, job_handle, agent_id, None,
                        )
                        .await);
                    };
                    hook::close_still_open_hooks(txn, vec![job_handle], now).await?;

                    if let SuiteJobOutcome::Failed { reason } = &outcome {
                        tracing::warn!(
                            job = job_handle,
                            job_id = job_row.job_id,
                            kind = ?reason.kind,
                            "Agent reported a failed job: {}",
                            reason.message
                        );
                    }

                    let suite_id = job_row.task_suite_id;
                    let job_id = job_row.job_id;

                    // Release the agent only while it is still assigned to the suite of this job.
                    // A retirement makes the agent `Offline` without a suite. Without this filter,
                    // a completion at the same time would make the agent `Idle` again, while
                    // nothing tracks its heartbeat.
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

                    // Only this suite.
                    let reclaimed =
                        job::reclaim_agent_tasks(txn, agent_uuid, Some(suite_id), now).await?;
                    // Add the tasks before the commit. See `SuiteQueues::push_ready`.
                    queues.push_reclaimed(&reclaimed);

                    Ok((job_id, terminal, released, reclaimed, suite_id))
                })
            },
        )
        .await?;

    tracing::info!(job = job_handle, job_id, %terminal, "Agent finished a job");
    if !released {
        tracing::info!(
            agent_id,
            job_id,
            "Agent was no longer assigned to the suite of the job; left its state alone"
        );
    }
    // Remove the ended job, thus the suite does not offer tasks to it or keep a reservation for it.
    pool.suite_queues.close(suite_id, agent_id, reclaimed.len());

    if !reclaimed.is_empty() {
        tracing::warn!(
            agent_id,
            job_id,
            reclaimed = reclaimed.len(),
            "Agent completed its job with uncommitted tasks; they went back to Ready and run \
             again, and the results of this run are lost"
        );
        notify_suite_available(pool, suite_id).await;
    }

    matching::agent_has_available_suite(
        &pool.db,
        agent_id,
        &pool.suite_queues.blocked_for(agent_id),
    )
    .await
}

/// At coordinator start, track the heartbeat of each agent that is not `Offline`, and send it a
/// `CounterSync` with the new boot id.
///
/// An agent that stopped while the coordinator was down then times out. The `CounterSync` tells the
/// agent that the notification ids start again, so that it does not ignore new notifications with
/// low ids.
pub async fn restore_agents(pool: &InfraPool) -> Result<()> {
    let agents = Agent::Entity::find()
        .select_only()
        .columns([Agent::Column::Id, Agent::Column::Uuid])
        .filter(Agent::Column::State.ne(AgentState::Offline))
        .into_tuple::<(i64, Uuid)>()
        .all(&pool.db)
        .await?;
    tracing::info!(
        agents = agents.len(),
        boot_id = %pool.boot_uuid,
        "Restoring agents after coordinator start"
    );
    for (id, uuid) in agents {
        if pool
            .agent_heartbeat_queue_tx
            .send(AgentHeartbeatOp::Heartbeat(id))
            .is_err()
        {
            return Err(Error::Custom("send agent heartbeat failed".to_string()));
        }
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
