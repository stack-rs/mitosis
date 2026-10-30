//! Agent registration, queries, heartbeats and shutdown.

pub mod heartbeat;

use std::collections::HashSet;

use sea_orm::sea_query::extension::postgres::PgExpr;
use sea_orm::sea_query::{Alias, PgFunc, Query};
use sea_orm::{prelude::*, FromQueryResult, QuerySelect, Set, TransactionTrait};
use uuid::Uuid;

use crate::config::InfraPool;
use crate::entity::{
    agents as Agent, group_agent as GroupAgent, groups as Group, machines as Machines,
    role::{GroupAgentRole, UserGroupRole},
    state::AgentState,
    task_suites as TaskSuites, user_group as UserGroup, users as User,
};
use crate::error::{ApiError, AuthError, Error, Result};
use crate::schema::{
    AgentHeartbeatReq, AgentHeartbeatResp, AgentInfo, AgentNotification, AgentShutdownOp,
    AgentsQueryReq, AgentsQueryResp, CountQuery, RegisterAgentReq, RegisterAgentResp,
};
use crate::service::auth::token::{generate_runner_token, TokenRole};
use crate::ws::AgentWsRouter;

use heartbeat::AgentHeartbeatOp;

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

    let (agent_uuid, agent_id, reused, retired) = pool
        .db
        .transaction::<_, (Uuid, i64, bool, bool), Error>(move |txn| {
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

                let mut retired = false;
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
                            retire_agent_writes(txn, agent.id, now).await?;
                            retired = true;
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
    if retired {
        retire_agent_post_commit(pool, agent_id, agent_uuid, RetireCause::Reregistered);
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

/// The reason to retire an agent.
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

/// Retire an agent: mark it `Offline`, stop tracking its heartbeat, and remove its buffered
/// notifications. Its token stops working.
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

    pool.db
        .transaction::<_, (), Error>(move |txn| {
            Box::pin(async move { retire_agent_writes(txn, agent_id, now).await })
        })
        .await?;

    retire_agent_post_commit(pool, agent_id, agent_uuid, cause);
    Ok(())
}

/// The database writes of [`retire_agent`]. Registration also calls this in its own transaction,
/// because it retires the previous agent and updates the agent row in one step.
async fn retire_agent_writes<C: ConnectionTrait>(
    txn: &C,
    agent_id: i64,
    now: TimeDateTimeWithTimeZone,
) -> Result<()> {
    // TODO: when agents run jobs, end the in-flight jobs of the agent and put
    // the tasks that it holds back to `Ready` here, in the same transaction.
    Agent::Entity::update_many()
        .col_expr(Agent::Column::State, Expr::value(AgentState::Offline))
        .col_expr(Agent::Column::AssignedTaskSuiteId, Expr::value(None::<i64>))
        .col_expr(Agent::Column::UpdatedAt, Expr::value(now))
        .filter(Agent::Column::Id.eq(agent_id))
        .exec(txn)
        .await?;
    Ok(())
}

/// The steps of [`retire_agent`] outside the database. They run after the commit, because a
/// rollback cannot undo them.
fn retire_agent_post_commit(pool: &InfraPool, agent_id: i64, agent_uuid: Uuid, cause: RetireCause) {
    let _ = pool
        .agent_heartbeat_queue_tx
        .send(AgentHeartbeatOp::Remove(agent_id));
    AgentWsRouter::reset(&pool.ws_router_tx, agent_uuid);
    tracing::info!(agent_id, %agent_uuid, ?cause, "Retired an agent");
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
    let updated = Agent::Entity::update_many()
        .col_expr(Agent::Column::State, Expr::value(req.state))
        .col_expr(Agent::Column::LastHeartbeat, Expr::value(now))
        .col_expr(Agent::Column::UpdatedAt, Expr::value(now))
        .filter(Agent::Column::Id.eq(agent_id))
        .filter(Agent::Column::State.ne(AgentState::Offline))
        .exec(&pool.db)
        .await?;
    if updated.rows_affected == 0 {
        return Err(Error::AuthError(AuthError::WrongCredentials));
    }

    let _ = pool
        .agent_heartbeat_queue_tx
        .send(AgentHeartbeatOp::Heartbeat(agent_id));

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
