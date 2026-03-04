//! Agent service for managing agent lifecycles

use sea_orm::sea_query::{extension::postgres::PgExpr, Alias, OnConflict, PgFunc, Query};
use sea_orm::{prelude::*, FromQueryResult, QueryOrder, Set, TransactionTrait};
use uuid::Uuid;

use crate::config::InfraPool;
use crate::entity::{
    agents as Agent, group_agent as GroupAgent, groups as Group, machines as Machines,
    role::{GroupAgentRole, UserGroupRole},
    state::{AgentState, TaskSuiteState},
    task_suite_agent as TaskSuiteAgent, task_suites as TaskSuites, user_group as UserGroup,
    users as User,
};
use crate::error::{ApiError, Error, Result};
use crate::schema::{
    AcceptSuiteReq, AcceptSuiteResp, AgentHeartbeatReq, AgentHeartbeatResp, AgentInfo,
    AgentNotification, AgentsQueryReq, AgentsQueryResp, CompleteSuiteReq, CompleteSuiteResp,
    CountQuery, ExecHooks, FetchSuiteResp, RegisterAgentReq, RegisterAgentResp, TaskSuiteSpec,
    WorkerSchedulePlan,
};
use crate::service::agent_heartbeat::AgentHeartbeatOp;
use crate::service::auth::{gen_agent_jwt, AgentJwtPayload};
use crate::service::suite_task_dispatcher::SuiteDispatcherOp;
use crate::ws::connection::{AgentWsRouter, RouterOp};

/// Default token lifetime for agents (30 days)
const DEFAULT_AGENT_LIFETIME_SECS: u64 = 30 * 24 * 60 * 60;

/// Register a new agent.
/// User must have Write or Admin role in each specified group.
// TODO: with boot_id we can reset agent's internal id to 0 with different boot_id
pub async fn user_register_agent(
    user_id: i64,
    pool: &InfraPool,
    req: RegisterAgentReq,
) -> Result<RegisterAgentResp> {
    let now = TimeDateTimeWithTimeZone::now_utc();
    let tags: Vec<String> = req.tags.into_iter().collect();
    let labels: Vec<String> = req.labels.into_iter().collect();
    let groups: Vec<String> = req.groups.into_iter().collect();
    let machine_code = req.machine_code;

    let lifetime = req.lifetime.unwrap_or(std::time::Duration::from_secs(
        DEFAULT_AGENT_LIFETIME_SECS,
    ));

    let (agent_uuid, agent_id) = pool
        .db
        .transaction::<_, (Uuid, i64), Error>(|txn| {
            Box::pin(async move {
                // Verify user has Write/Admin permission in all specified groups
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
                        .ok_or(Error::AuthError(crate::error::AuthError::PermissionDenied))?;

                    if !matches!(user_group.role, UserGroupRole::Write | UserGroupRole::Admin) {
                        return Err(Error::AuthError(crate::error::AuthError::PermissionDenied));
                    }
                }

                // Upsert machine record if machine_code was provided.
                // INSERT ... ON CONFLICT (machine_code) DO UPDATE SET last_seen_at = EXCLUDED.last_seen_at
                // This ensures first_seen_at is preserved and last_seen_at is always refreshed.
                let machine_id: Option<i64> = if let Some(code) = machine_code {
                    let machine = Machines::Entity::insert(Machines::ActiveModel {
                        machine_code: Set(code),
                        metadata: Set(None),
                        first_seen_at: Set(now),
                        last_seen_at: Set(now),
                        ..Default::default()
                    })
                    .on_conflict(
                        OnConflict::column(Machines::Column::MachineCode)
                            .update_column(Machines::Column::LastSeenAt)
                            .to_owned(),
                    )
                    .exec_with_returning(txn)
                    .await?;
                    Some(machine.id)
                } else {
                    None
                };

                // Create agent
                let agent_uuid = Uuid::new_v4();
                let new_agent = Agent::ActiveModel {
                    uuid: Set(agent_uuid),
                    creator_id: Set(user_id),
                    machine_id: Set(machine_id),
                    tags: Set(tags),
                    labels: Set(labels),
                    state: Set(AgentState::Idle),
                    last_heartbeat: Set(now),
                    assigned_task_suite_id: Set(None),
                    created_at: Set(now),
                    updated_at: Set(now),
                    ..Default::default()
                };
                let new_agent = new_agent.insert(txn).await?;

                // Create group_agent relationships
                for group_name in &groups {
                    let group = Group::Entity::find()
                        .filter(Group::Column::GroupName.eq(group_name.clone()))
                        .one(txn)
                        .await?
                        .ok_or(Error::ApiError(ApiError::NotFound(format!(
                            "Group {group_name}"
                        ))))?;

                    let gnm = GroupAgent::ActiveModel {
                        group_id: Set(group.id),
                        agent_id: Set(new_agent.id),
                        role: Set(GroupAgentRole::Write),
                        ..Default::default()
                    };
                    gnm.insert(txn).await?;
                }

                Ok((agent_uuid, new_agent.id))
            })
        })
        .await?;

    // Generate JWT token
    let payload = AgentJwtPayload {
        agent_id,
        agent_uuid,
    };
    let token = gen_agent_jwt(&payload, lifetime)?;

    // Start tracking this agent's heartbeat
    let _ = pool
        .agent_heartbeat_queue_tx
        .send(AgentHeartbeatOp::Heartbeat(agent_id));

    // Get initial notification counter
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = pool.ws_router_tx.send(RouterOp::GetCounter {
        uuid: agent_uuid,
        tx,
    });
    let notification_counter = rx.await.unwrap_or_default().unwrap_or_default();

    Ok(RegisterAgentResp {
        agent_uuid,
        token,
        notification_counter,
    })
}

/// Update agent heartbeat and return pending actions.
/// Called by agents to report their current state and receive pending actions.
pub async fn agent_heartbeat(
    agent_id: i64,
    agent_uuid: Uuid,
    pool: &InfraPool,
    req: AgentHeartbeatReq,
) -> Result<AgentHeartbeatResp> {
    let now = TimeDateTimeWithTimeZone::now_utc();

    // Renew heartbeat deadline in the priority queue
    let _ = pool
        .agent_heartbeat_queue_tx
        .send(AgentHeartbeatOp::Heartbeat(agent_id));

    // Update agent state and heartbeat
    let mut update = Agent::Entity::update_many()
        .col_expr(Agent::Column::State, Expr::value(req.state))
        .col_expr(Agent::Column::LastHeartbeat, Expr::value(now))
        .col_expr(Agent::Column::UpdatedAt, Expr::value(now))
        .filter(Agent::Column::Id.eq(agent_id));

    // Update assigned suite if provided
    if let Some(suite_uuid) = req.assigned_suite_uuid {
        // Find suite ID from UUID
        let suite = TaskSuites::Entity::find()
            .filter(TaskSuites::Column::Uuid.eq(suite_uuid))
            .one(&pool.db)
            .await?;

        if let Some(suite) = suite {
            update = update.col_expr(
                Agent::Column::AssignedTaskSuiteId,
                Expr::value(Some(suite.id)),
            );
        }
    } else {
        update = update.col_expr(Agent::Column::AssignedTaskSuiteId, Expr::value(None::<i64>));
    }

    update.exec(&pool.db).await?;

    // Consistency checks: Verify agent state matches DB expectations
    // This helps recover from coordinator restarts and state divergence
    let db_agent = Agent::Entity::find()
        .filter(Agent::Column::Id.eq(agent_id))
        .one(&pool.db)
        .await?
        .ok_or(Error::Custom("Agent not found".to_string()))?;

    // Check 1: Agent reports Idle but DB says it should execute a suite
    if req.state == AgentState::Idle && db_agent.assigned_task_suite_id.is_some() {
        if let Some(suite_id) = db_agent.assigned_task_suite_id {
            let suite = TaskSuites::Entity::find_by_id(suite_id)
                .one(&pool.db)
                .await?;

            if let Some(suite) = suite {
                if !matches!(
                    suite.state,
                    TaskSuiteState::Cancelled | TaskSuiteState::Complete
                ) {
                    tracing::info!(
                        agent_uuid = %agent_uuid,
                        suite_uuid = %suite.uuid,
                        "Agent reports Idle but should be executing suite - regenerating notification"
                    );

                    let _ = AgentWsRouter::notify(
                        &pool.ws_router_tx,
                        agent_uuid,
                        AgentNotification::SuiteAvailable {
                            suite_uuid: Some(suite.uuid),
                            priority: suite.priority,
                        },
                    );
                }
            }
        }
    }

    // Check 2: Agent executing wrong suite (UUID mismatch)
    if let Some(reported_suite_uuid) = req.assigned_suite_uuid {
        if let Some(db_suite_id) = db_agent.assigned_task_suite_id {
            let db_suite = TaskSuites::Entity::find_by_id(db_suite_id)
                .one(&pool.db)
                .await?;

            if let Some(db_suite) = db_suite {
                if db_suite.uuid != reported_suite_uuid
                    && !matches!(
                        db_suite.state,
                        TaskSuiteState::Cancelled | TaskSuiteState::Complete
                    )
                {
                    tracing::warn!(
                        agent_uuid = %agent_uuid,
                        reported_suite = %reported_suite_uuid,
                        db_suite = %db_suite.uuid,
                        "Agent executing wrong suite - sending preemption notification"
                    );

                    let _ = AgentWsRouter::notify(
                        &pool.ws_router_tx,
                        agent_uuid,
                        AgentNotification::PreemptSuite {
                            new_suite_uuid: db_suite.uuid,
                            new_priority: db_suite.priority,
                            current_suite_uuid: reported_suite_uuid,
                        },
                    );
                }
            }
        }
    }

    // Check 3: Agent executing cancelled suite
    if let Some(reported_suite_uuid) = req.assigned_suite_uuid {
        let suite = TaskSuites::Entity::find()
            .filter(TaskSuites::Column::Uuid.eq(reported_suite_uuid))
            .one(&pool.db)
            .await?;

        if let Some(suite) = suite {
            if suite.state == TaskSuiteState::Cancelled && !matches!(req.state, AgentState::Idle) {
                tracing::info!(
                    agent_uuid = %agent_uuid,
                    suite_uuid = %reported_suite_uuid,
                    "Agent executing cancelled suite - regenerating cancellation notification"
                );

                let _ = AgentWsRouter::notify(
                    &pool.ws_router_tx,
                    agent_uuid,
                    AgentNotification::SuiteCancelled {
                        suite_uuid: reported_suite_uuid,
                        reason: "Suite was cancelled".to_string(),
                    },
                );
            }
        }
    }

    // Check 4: Detect counter desync (coordinator restart)
    // If the agent's last_notification_id is higher than the coordinator's current counter,
    // the coordinator must have restarted and lost its state. Send a CounterSync so the agent
    // can reset to the coordinator's current sequence.
    let (counter_tx, counter_rx) = tokio::sync::oneshot::channel();
    let _ = pool.ws_router_tx.send(RouterOp::GetCounter {
        uuid: agent_uuid,
        tx: counter_tx,
    });
    if let Ok(Some(coordinator_counter)) = counter_rx.await {
        if req.last_notification_id > coordinator_counter {
            tracing::warn!(
                agent_uuid = %agent_uuid,
                agent_counter = req.last_notification_id,
                coordinator_counter = coordinator_counter,
                "Counter desync detected — sending CounterSync"
            );
            let _ = AgentWsRouter::notify(
                &pool.ws_router_tx,
                agent_uuid,
                AgentNotification::CounterSync {
                    counter: coordinator_counter,
                    boot_id: pool.boot_uuid,
                },
            );
        }
    }

    // Check if we need to notify about available suites (existing logic)
    if req.state == AgentState::Idle && db_agent.assigned_task_suite_id.is_none() {
        if check_suite_available_for_agent(agent_id, pool).await? {
            let _ = AgentWsRouter::notify(
                &pool.ws_router_tx,
                agent_uuid,
                AgentNotification::SuiteAvailable {
                    suite_uuid: None,
                    priority: 0,
                },
            );
        }
    }

    // Return missed notifications
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = pool.ws_router_tx.send(RouterOp::GetNotifications {
        uuid: agent_uuid,
        tx,
    });
    let notifications = rx.await.unwrap_or_default();

    Ok(AgentHeartbeatResp { notifications })
}

/// Check if there's an available suite for an agent
async fn check_suite_available_for_agent(agent_id: i64, pool: &InfraPool) -> Result<bool> {
    // Find if any suite is assigned to this agent and is in Open state
    let count = TaskSuiteAgent::Entity::find()
        .inner_join(TaskSuites::Entity)
        .filter(TaskSuiteAgent::Column::AgentId.eq(agent_id))
        .filter(
            TaskSuites::Column::State
                .eq(TaskSuiteState::Open)
                .or(TaskSuites::Column::State.eq(TaskSuiteState::Closed)),
        )
        .filter(TaskSuites::Column::PendingTasks.gt(0))
        .count(&pool.db)
        .await?;

    Ok(count > 0)
}

/// Notify all agents about coordinator restart on startup.
/// Sends a CounterSync notification to all known agents so they can re-sync
/// their notification counters after reconnecting via WebSocket or heartbeat.
pub async fn notify_all_agents_of_restart(pool: &InfraPool) -> Result<()> {
    // Get all agents from database
    let agents = Agent::Entity::find().all(&pool.db).await?;

    tracing::info!(
        "Notifying {} agents about coordinator restart (boot_id={})",
        agents.len(),
        pool.boot_uuid,
    );

    for agent in &agents {
        let _ = AgentWsRouter::notify(
            &pool.ws_router_tx,
            agent.uuid,
            AgentNotification::CounterSync {
                counter: 0,
                boot_id: pool.boot_uuid,
            },
        );
        tracing::debug!(
            agent_uuid = %agent.uuid,
            "Queued CounterSync notification for agent"
        );
    }

    Ok(())
}

/// Query agents visible to the user.
/// User must have at least Read role in the specified group.
pub async fn user_query_agents(
    user_id: i64,
    pool: &InfraPool,
    mut query: AgentsQueryReq,
) -> Result<AgentsQueryResp> {
    // Default to user's personal group if not specified
    if query.group_name.is_none() {
        let user = User::Entity::find_by_id(user_id)
            .one(&pool.db)
            .await?
            .ok_or(Error::ApiError(ApiError::NotFound(
                "User not found".to_string(),
            )))?;
        query.group_name = Some(user.username);
    }

    let group_name = query.group_name.clone().unwrap();

    // Verify user has Read access to the group
    let group = Group::Entity::find()
        .filter(Group::Column::GroupName.eq(&group_name))
        .one(&pool.db)
        .await?
        .ok_or(Error::ApiError(ApiError::NotFound(format!(
            "Group {group_name}"
        ))))?;

    let user_group = UserGroup::Entity::find()
        .filter(UserGroup::Column::UserId.eq(user_id))
        .filter(UserGroup::Column::GroupId.eq(group.id))
        .one(&pool.db)
        .await?
        .ok_or(Error::AuthError(crate::error::AuthError::PermissionDenied))?;

    if !matches!(
        user_group.role,
        UserGroupRole::Read | UserGroupRole::Write | UserGroupRole::Admin
    ) {
        return Err(Error::AuthError(crate::error::AuthError::PermissionDenied));
    }

    // Build query
    let builder = pool.db.get_database_backend();
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
        );
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
            TaskSuites::Entity,
            Expr::col((TaskSuites::Entity, TaskSuites::Column::Id)).eq(Expr::col((
                Agent::Entity,
                Agent::Column::AssignedTaskSuiteId,
            ))),
        )
        .and_where(Expr::col((GroupAgent::Entity, GroupAgent::Column::GroupId)).eq(group.id));

    // Apply filters
    if let Some(ref tags) = query.tags {
        let tags_vec: Vec<String> = tags.iter().cloned().collect();
        stmt.and_where(Expr::col((Agent::Entity, Agent::Column::Tags)).contains(tags_vec));
    }

    if let Some(ref labels) = query.labels {
        let labels_vec: Vec<String> = labels.iter().cloned().collect();
        stmt.and_where(Expr::col((Agent::Entity, Agent::Column::Labels)).contains(labels_vec));
    }

    if let Some(ref states) = query.states {
        let states_vec: Vec<AgentState> = states.iter().copied().collect();
        stmt.and_where(
            Expr::col((Agent::Entity, Agent::Column::State)).eq(PgFunc::any(states_vec)),
        );
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

/// Mark an agent as offline (for heartbeat timeout handling)
pub async fn mark_agent_offline(pool: &InfraPool, agent_id: i64) -> Result<()> {
    let now = TimeDateTimeWithTimeZone::now_utc();

    Agent::Entity::update_many()
        .col_expr(Agent::Column::State, Expr::value(AgentState::Offline))
        .col_expr(Agent::Column::UpdatedAt, Expr::value(now))
        .col_expr(Agent::Column::AssignedTaskSuiteId, Expr::value(None::<i64>))
        .filter(Agent::Column::Id.eq(agent_id))
        .exec(&pool.db)
        .await?;

    Ok(())
}

// ============================================================================
// Suite Fetch and Execution APIs (Agent-authenticated)
// ============================================================================

/// Fetch an available suite for the agent to execute.
/// Returns the highest priority suite that matches the agent's tags and
/// that the agent's groups have Write access to.
pub async fn agent_fetch_suite(
    agent_id: i64,
    pool: &InfraPool,
    specific_suite_uuid: Option<Uuid>,
) -> Result<FetchSuiteResp> {
    // If a specific suite is requested, fetch that one
    if let Some(suite_uuid) = specific_suite_uuid {
        let suite = fetch_specific_suite(agent_id, suite_uuid, pool).await?;
        return Ok(FetchSuiteResp { suite });
    }

    // Otherwise, find the best available suite for this agent
    let suite = fetch_best_available_suite(agent_id, pool).await?;
    Ok(FetchSuiteResp { suite })
}

/// Fetch a specific suite by UUID if the agent has access
async fn fetch_specific_suite(
    agent_id: i64,
    suite_uuid: Uuid,
    pool: &InfraPool,
) -> Result<Option<TaskSuiteSpec>> {
    // Check if this agent is assigned to this suite
    let assignment = TaskSuiteAgent::Entity::find()
        .inner_join(TaskSuites::Entity)
        .filter(TaskSuiteAgent::Column::AgentId.eq(agent_id))
        .filter(TaskSuites::Column::Uuid.eq(suite_uuid))
        .one(&pool.db)
        .await?;

    if assignment.is_none() {
        return Ok(None);
    }

    // Fetch the suite details
    let suite = TaskSuites::Entity::find()
        .filter(TaskSuites::Column::Uuid.eq(suite_uuid))
        .filter(
            TaskSuites::Column::State
                .eq(TaskSuiteState::Open)
                .or(TaskSuites::Column::State.eq(TaskSuiteState::Closed)),
        )
        .one(&pool.db)
        .await?;

    match suite {
        Some(suite) => Ok(Some(convert_suite_to_spec(suite, pool).await?)),
        None => Ok(None),
    }
}

/// Fetch the best available suite for an agent (highest priority first)
async fn fetch_best_available_suite(
    agent_id: i64,
    pool: &InfraPool,
) -> Result<Option<TaskSuiteSpec>> {
    // Find suites assigned to this agent using explicit join
    // Only consider suites in Open or Closed state with pending tasks
    use sea_orm::QuerySelect;

    let suite = TaskSuites::Entity::find()
        .join(
            sea_orm::JoinType::InnerJoin,
            TaskSuiteAgent::Relation::TaskSuites.def().rev(),
        )
        .filter(TaskSuiteAgent::Column::AgentId.eq(agent_id))
        .filter(
            TaskSuites::Column::State
                .eq(TaskSuiteState::Open)
                .or(TaskSuites::Column::State.eq(TaskSuiteState::Closed)),
        )
        .filter(TaskSuites::Column::PendingTasks.gt(0))
        .order_by_desc(TaskSuites::Column::Priority)
        .order_by_asc(TaskSuites::Column::CreatedAt)
        .one(&pool.db)
        .await?;

    match suite {
        Some(suite) => Ok(Some(convert_suite_to_spec(suite, pool).await?)),
        None => Ok(None),
    }
}

/// Convert a TaskSuites::Model to TaskSuiteSpec
async fn convert_suite_to_spec(
    suite: TaskSuites::Model,
    pool: &InfraPool,
) -> Result<TaskSuiteSpec> {
    // Get the group name
    let group = Group::Entity::find_by_id(suite.group_id)
        .one(&pool.db)
        .await?
        .ok_or_else(|| Error::ApiError(ApiError::NotFound("Group not found".to_string())))?;

    // Parse worker_schedule from JSON
    let worker_schedule: WorkerSchedulePlan = serde_json::from_value(suite.worker_schedule.clone())
        .map_err(|e| {
            Error::ApiError(ApiError::InvalidRequest(format!(
                "Invalid worker_schedule: {}",
                e
            )))
        })?;

    // Parse exec_hooks from JSON
    let exec_hooks: Option<ExecHooks> = suite
        .exec_hooks
        .as_ref()
        .map(|v| serde_json::from_value(v.clone()))
        .transpose()
        .map_err(|e| {
            Error::ApiError(ApiError::InvalidRequest(format!(
                "Invalid exec_hooks: {}",
                e
            )))
        })?;

    Ok(TaskSuiteSpec {
        uuid: suite.uuid,
        name: suite.name,
        description: suite.description,
        group_id: suite.group_id,
        group_name: group.group_name,
        tags: suite.tags,
        labels: suite.labels,
        priority: suite.priority,
        worker_schedule,
        exec_hooks,
        state: suite.state,
        total_tasks: suite.total_tasks,
        pending_tasks: suite.pending_tasks,
    })
}

/// Accept a suite for execution
/// This updates the agent's assigned_task_suite_id and state
pub async fn agent_accept_suite(
    agent_id: i64,
    pool: &InfraPool,
    req: AcceptSuiteReq,
) -> Result<AcceptSuiteResp> {
    let now = TimeDateTimeWithTimeZone::now_utc();

    // Verify the suite exists and is in a valid state
    let suite = TaskSuites::Entity::find()
        .filter(TaskSuites::Column::Uuid.eq(req.suite_uuid))
        .one(&pool.db)
        .await?
        .ok_or_else(|| Error::ApiError(ApiError::NotFound("Suite not found".to_string())))?;

    // Verify the agent is assigned to this suite
    let assignment = TaskSuiteAgent::Entity::find()
        .filter(TaskSuiteAgent::Column::AgentId.eq(agent_id))
        .filter(TaskSuiteAgent::Column::TaskSuiteId.eq(suite.id))
        .one(&pool.db)
        .await?;

    if assignment.is_none() {
        return Ok(AcceptSuiteResp {
            accepted: false,
            reason: Some("Agent is not assigned to this suite".to_string()),
        });
    }

    // Check suite state
    if !matches!(suite.state, TaskSuiteState::Open | TaskSuiteState::Closed) {
        return Ok(AcceptSuiteResp {
            accepted: false,
            reason: Some(format!(
                "Suite is in {:?} state, cannot accept",
                suite.state
            )),
        });
    }

    // Update agent state to Preparing and assign the suite
    Agent::Entity::update_many()
        .col_expr(Agent::Column::State, Expr::value(AgentState::Provision))
        .col_expr(
            Agent::Column::AssignedTaskSuiteId,
            Expr::value(Some(suite.id)),
        )
        .col_expr(Agent::Column::UpdatedAt, Expr::value(now))
        .filter(Agent::Column::Id.eq(agent_id))
        .exec(&pool.db)
        .await?;

    // Notify the dispatcher about the agent's batch capacity so it can size the buffer.
    let schedule: WorkerSchedulePlan =
        serde_json::from_value(suite.worker_schedule.clone().take()).unwrap_or(
            WorkerSchedulePlan::FixedWorkers {
                worker_count: 1,
                cpu_binding: None,
                task_prefetch_count: 16,
            },
        );
    let batch_size = match schedule {
        WorkerSchedulePlan::FixedWorkers {
            worker_count,
            task_prefetch_count,
            ..
        } => worker_count.saturating_mul(task_prefetch_count),
    };
    let _ = pool.suite_task_dispatcher_tx.send(SuiteDispatcherOp::UpdateCapacity {
        suite_id: suite.id,
        batch_size,
    });

    Ok(AcceptSuiteResp {
        accepted: true,
        reason: None,
    })
}

/// Report that the agent has started executing the suite
/// (after env_preparation completed successfully)
pub async fn agent_start_suite(agent_id: i64, pool: &InfraPool, suite_uuid: Uuid) -> Result<()> {
    let now = TimeDateTimeWithTimeZone::now_utc();

    // Verify the suite exists
    let suite = TaskSuites::Entity::find()
        .filter(TaskSuites::Column::Uuid.eq(suite_uuid))
        .one(&pool.db)
        .await?
        .ok_or_else(|| Error::ApiError(ApiError::NotFound("Suite not found".to_string())))?;

    // Update agent state to Executing
    Agent::Entity::update_many()
        .col_expr(Agent::Column::State, Expr::value(AgentState::Executing))
        .col_expr(Agent::Column::UpdatedAt, Expr::value(now))
        .filter(Agent::Column::Id.eq(agent_id))
        .filter(Agent::Column::AssignedTaskSuiteId.eq(suite.id))
        .exec(&pool.db)
        .await?;

    Ok(())
}

/// Report suite execution completion
pub async fn agent_complete_suite(
    agent_id: i64,
    pool: &InfraPool,
    req: CompleteSuiteReq,
) -> Result<CompleteSuiteResp> {
    let now = TimeDateTimeWithTimeZone::now_utc();

    // Verify the suite exists
    let _suite = TaskSuites::Entity::find()
        .filter(TaskSuites::Column::Uuid.eq(req.suite_uuid))
        .one(&pool.db)
        .await?
        .ok_or_else(|| Error::ApiError(ApiError::NotFound("Suite not found".to_string())))?;

    // Log completion
    tracing::info!(
        agent_id = agent_id,
        suite_uuid = %req.suite_uuid,
        tasks_completed = req.tasks_completed,
        tasks_failed = req.tasks_failed,
        reason = ?req.completion_reason,
        "Agent completed suite execution"
    );

    // Update agent state to Idle and clear assigned suite
    Agent::Entity::update_many()
        .col_expr(Agent::Column::State, Expr::value(AgentState::Idle))
        .col_expr(Agent::Column::AssignedTaskSuiteId, Expr::value(None::<i64>))
        .col_expr(Agent::Column::UpdatedAt, Expr::value(now))
        .filter(Agent::Column::Id.eq(agent_id))
        .exec(&pool.db)
        .await?;

    // Check if there's another suite available
    let next_suite_available = check_suite_available_for_agent(agent_id, pool).await?;

    Ok(CompleteSuiteResp {
        next_suite_available,
    })
}

/// Report that agent is entering cleanup phase
pub async fn agent_enter_cleanup(agent_id: i64, pool: &InfraPool) -> Result<()> {
    let now = TimeDateTimeWithTimeZone::now_utc();

    Agent::Entity::update_many()
        .col_expr(Agent::Column::State, Expr::value(AgentState::Cleanup))
        .col_expr(Agent::Column::UpdatedAt, Expr::value(now))
        .filter(Agent::Column::Id.eq(agent_id))
        .exec(&pool.db)
        .await?;

    Ok(())
}

/// Helper function to remove an agent from the database
async fn remove_agent(agent: Agent::Model, pool: &InfraPool) -> Result<()> {
    // If the agent has an assigned suite, we should handle it
    // For now, we'll just clear the assignment and let the suite be picked up by another agent
    if let Some(suite_id) = agent.assigned_task_suite_id {
        tracing::warn!(
            agent_id = agent.id,
            suite_id = suite_id,
            "Removing agent with assigned suite"
        );
        // The suite remains in the database and can be picked up by other agents
    }

    // Delete the agent (this will cascade delete GroupAgent relationships)
    Agent::Entity::delete_by_id(agent.id)
        .exec(&pool.db)
        .await?;

    Ok(())
}

/// User-initiated shutdown of an agent by UUID
/// Requires user to have Admin role in a group where the agent has Admin role
pub async fn user_remove_agent_by_uuid(
    user_id: i64,
    agent_uuid: Uuid,
    op: crate::schema::AgentShutdownOp,
    pool: &InfraPool,
) -> Result<()> {
    use crate::entity::group_agent as GroupAgent;
    use crate::entity::user_group as UserGroup;

    // Find the agent
    let agent = Agent::Entity::find()
        .filter(Agent::Column::Uuid.eq(agent_uuid))
        .one(&pool.db)
        .await?
        .ok_or(Error::ApiError(ApiError::NotFound(format!(
            "Agent {agent_uuid} not found"
        ))))?;

    // Check if user has Admin permission in a group where the agent has Admin role
    // We need at least one group where both:
    // 1. The agent has Admin role
    // 2. The user has Admin role
    let builder = pool.db.get_database_backend();
    let stmt = Query::select()
        .expr(Expr::val(1))
        .from(GroupAgent::Entity)
        .inner_join(
            UserGroup::Entity,
            Expr::col((UserGroup::Entity, UserGroup::Column::GroupId))
                .eq(Expr::col((GroupAgent::Entity, GroupAgent::Column::GroupId))),
        )
        .and_where(Expr::col((GroupAgent::Entity, GroupAgent::Column::AgentId)).eq(agent.id))
        .and_where(
            Expr::col((GroupAgent::Entity, GroupAgent::Column::Role)).eq(GroupAgentRole::Admin),
        )
        .and_where(Expr::col((UserGroup::Entity, UserGroup::Column::UserId)).eq(user_id))
        .and_where(Expr::col((UserGroup::Entity, UserGroup::Column::Role)).eq(UserGroupRole::Admin))
        .limit(1)
        .to_owned();

    #[derive(Debug, FromQueryResult)]
    struct PermissionCheck {
        #[allow(dead_code)]
        val: i32,
    }

    let permission = PermissionCheck::find_by_statement(builder.build(&stmt))
        .one(&pool.db)
        .await?;

    if permission.is_none() {
        return Err(Error::AuthError(crate::error::AuthError::PermissionDenied));
    }

    // Remove from heartbeat tracking before any state changes
    let _ = pool
        .agent_heartbeat_queue_tx
        .send(AgentHeartbeatOp::Remove(agent.id));

    // Perform the shutdown operation
    match op {
        crate::schema::AgentShutdownOp::Force => {
            remove_agent(agent, pool).await?;
        }
        crate::schema::AgentShutdownOp::Graceful => {
            if agent.assigned_task_suite_id.is_none() {
                // No assigned suite, safe to remove immediately
                remove_agent(agent, pool).await?;
            } else {
                // Has assigned suite, mark as offline for graceful shutdown
                let now = TimeDateTimeWithTimeZone::now_utc();
                Agent::Entity::update_many()
                    .col_expr(Agent::Column::State, Expr::value(AgentState::Offline))
                    .col_expr(Agent::Column::UpdatedAt, Expr::value(now))
                    .filter(Agent::Column::Id.eq(agent.id))
                    .exec(&pool.db)
                    .await?;
            }
        }
    }

    Ok(())
}
