//! Agent heartbeat queue for detecting timed-out agents and reclaiming their tasks.

use std::{cmp::Reverse, time::Duration};

use priority_queue::PriorityQueue;
use sea_orm::prelude::*;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::{
    config::InfraPool,
    entity::{
        active_tasks as ActiveTasks,
        agents as Agent,
        state::{AgentState, TaskState},
    },
    service::suite_task_dispatcher::SuiteDispatcherOp,
};

#[derive(Debug)]
pub struct AgentHeartbeatQueue {
    agents: PriorityQueue<i64, Reverse<Instant>>,
    cancel_token: CancellationToken,
    heartbeat_timeout: Duration,
    pool: InfraPool,
    rx: crossfire::AsyncRx<AgentHeartbeatOp>,
}

pub enum AgentHeartbeatOp {
    /// Remove agent from tracking (e.g., agent was deleted)
    Remove(i64),
    /// Record a heartbeat for the agent, resetting its deadline
    Heartbeat(i64),
}

impl AgentHeartbeatQueue {
    pub fn new(
        cancel_token: CancellationToken,
        heartbeat_timeout: Duration,
        pool: InfraPool,
        rx: crossfire::AsyncRx<AgentHeartbeatOp>,
    ) -> Self {
        Self {
            agents: PriorityQueue::new(),
            cancel_token,
            heartbeat_timeout,
            pool,
            rx,
        }
    }

    fn remove(&mut self, agent_id: i64) {
        tracing::debug!(agent_id = agent_id, "Agent removed from heartbeat tracking");
        self.agents.remove(&agent_id);
    }

    fn heartbeat(&mut self, agent_id: i64) {
        tracing::debug!(agent_id = agent_id, "Agent heartbeat received");
        self.agents
            .push(agent_id, Reverse(Instant::now() + self.heartbeat_timeout));
    }

    fn handle_op(&mut self, op: AgentHeartbeatOp) {
        match op {
            AgentHeartbeatOp::Remove(agent_id) => self.remove(agent_id),
            AgentHeartbeatOp::Heartbeat(agent_id) => self.heartbeat(agent_id),
        }
    }

    async fn handle_timeout(&mut self) -> crate::error::Result<()> {
        if let Some(true) = self.agents.peek().map(|(_, r)| r.0 <= Instant::now()) {
            let (agent_id, _) = self.agents.pop().unwrap();

            let db_timeout = Duration::from_secs(10);

            // Fetch the agent to get its UUID (needed for task reclamation by runner_id)
            let find_result = tokio::time::timeout(
                db_timeout,
                Agent::Entity::find_by_id(agent_id).one(&self.pool.db),
            )
            .await;

            let agent = match find_result {
                Ok(Ok(Some(agent))) => agent,
                Ok(Ok(None)) => {
                    tracing::debug!(
                        agent_id = agent_id,
                        "Agent not found during heartbeat timeout — already removed"
                    );
                    return Ok(());
                }
                Ok(Err(e)) => return Err(e.into()),
                Err(_) => {
                    tracing::warn!(
                        agent_id = agent_id,
                        "Database query for agent timed out during heartbeat check"
                    );
                    return Ok(());
                }
            };

            tracing::info!(
                agent_id = agent_id,
                agent_uuid = %agent.uuid,
                "Agent heartbeat timed out — marking offline and reclaiming tasks"
            );

            let now = TimeDateTimeWithTimeZone::now_utc();
            let agent_uuid = agent.uuid;

            // Mark agent offline and clear assigned suite in one update
            let mark_result = tokio::time::timeout(db_timeout, async {
                Agent::Entity::update_many()
                    .col_expr(Agent::Column::State, Expr::value(AgentState::Offline))
                    .col_expr(
                        Agent::Column::AssignedTaskSuiteId,
                        Expr::value(None::<i64>),
                    )
                    .col_expr(Agent::Column::UpdatedAt, Expr::value(now))
                    .filter(Agent::Column::Id.eq(agent_id))
                    .exec(&self.pool.db)
                    .await
            })
            .await;

            match mark_result {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => {
                    tracing::error!(
                        agent_id = agent_id,
                        "Failed to mark agent offline: {}",
                        e
                    );
                    return Err(e.into());
                }
                Err(_) => {
                    tracing::warn!(agent_id = agent_id, "Marking agent offline timed out");
                    return Ok(());
                }
            }

            // Reclaim all Running tasks that were assigned to this agent.
            // runner_id holds the agent's UUID for suite tasks.
            // Use exec_with_returning to get the reclaimed tasks so we can
            // re-add them to the suite dispatcher buffers.
            let reclaim_result = tokio::time::timeout(db_timeout, async {
                ActiveTasks::Entity::update_many()
                    .col_expr(
                        ActiveTasks::Column::State,
                        Expr::value(TaskState::Ready),
                    )
                    .col_expr(
                        ActiveTasks::Column::RunnerId,
                        Expr::value(None::<uuid::Uuid>),
                    )
                    .col_expr(ActiveTasks::Column::UpdatedAt, Expr::value(now))
                    .filter(ActiveTasks::Column::RunnerId.eq(agent_uuid))
                    .filter(ActiveTasks::Column::State.eq(TaskState::Running))
                    .exec_with_returning(&self.pool.db)
                    .await
            })
            .await;

            match reclaim_result {
                Ok(Ok(reclaimed)) => {
                    if !reclaimed.is_empty() {
                        tracing::info!(
                            agent_id = agent_id,
                            agent_uuid = %agent_uuid,
                            reclaimed = reclaimed.len(),
                            "Reclaimed tasks from offline agent"
                        );
                        // Re-add reclaimed suite tasks to the dispatcher buffers so other
                        // agents (or the same agent on reconnect) can pick them up.
                        for task in &reclaimed {
                            if let Some(suite_id) = task.task_suite_id {
                                let _ = self.pool.suite_task_dispatcher_tx.send(
                                    SuiteDispatcherOp::AddTask {
                                        suite_id,
                                        task_id: task.id,
                                        priority: task.priority,
                                    },
                                );
                            }
                        }
                    }
                }
                Ok(Err(e)) => {
                    tracing::error!(
                        agent_id = agent_id,
                        "Failed to reclaim tasks from offline agent: {}",
                        e
                    );
                    return Err(e.into());
                }
                Err(_) => {
                    tracing::warn!(
                        agent_id = agent_id,
                        "Task reclamation timed out for offline agent"
                    );
                }
            }
        }
        Ok(())
    }

    pub async fn run(&mut self) {
        tracing::info!("Agent heartbeat queue started");
        let mut timeout_duration = self.heartbeat_timeout;
        loop {
            tokio::select! {
                biased;
                _ = self.cancel_token.cancelled() => {
                    break;
                }
                op = self.rx.recv() => match op.ok() {
                    None => break,
                    Some(op) => {
                        self.handle_op(op);
                        timeout_duration = self
                            .agents
                            .peek()
                            .map(|(_, r)| {
                                let deadline = r.0;
                                let now = Instant::now();
                                if deadline > now { deadline - now } else { Duration::ZERO }
                            })
                            .unwrap_or(self.heartbeat_timeout);
                    }
                },
                _ = tokio::time::sleep(timeout_duration) => {
                    if let Err(e) = self.handle_timeout().await {
                        if self.cancel_token.is_cancelled() {
                            tracing::warn!("Agent timeout handling failed during shutdown: {:?}", e);
                        } else {
                            tracing::error!("Agent handle_timeout failed: {:?}", e);
                            self.cancel_token.cancel();
                            break;
                        }
                    }
                    timeout_duration = self
                        .agents
                        .peek()
                        .map(|(_, r)| {
                            let deadline = r.0;
                            let now = Instant::now();
                            if deadline > now { deadline - now } else { Duration::ZERO }
                        })
                        .unwrap_or(self.heartbeat_timeout);
                }
            }
        }
        tracing::info!("Agent heartbeat queue stopped");
    }
}
