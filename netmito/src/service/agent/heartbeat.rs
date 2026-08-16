//! Liveness tracking for agents.

use std::{cmp::Reverse, time::Duration};

use priority_queue::PriorityQueue;
use sea_orm::prelude::*;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::{
    channel::MRx,
    config::InfraPool,
    entity::{
        agents as Agent,
        state::{AgentState, SuiteJobState},
    },
    service::agent::job,
};

/// Ceiling on any single database call made while handling a timeout, so a
/// stalled database cannot wedge the actor.
const DB_TIMEOUT: Duration = Duration::from_secs(10);

pub enum AgentHeartbeatOp {
    /// Record a heartbeat, resetting the agent's deadline.
    Heartbeat(i64),
    /// Stop tracking the agent (it shut down, or was already marked offline).
    Remove(i64),
}

pub struct AgentHeartbeatQueue {
    agents: PriorityQueue<i64, Reverse<Instant>>,
    cancel_token: CancellationToken,
    heartbeat_timeout: Duration,
    pool: InfraPool,
    rx: MRx<AgentHeartbeatOp>,
}

impl AgentHeartbeatQueue {
    pub fn new(
        cancel_token: CancellationToken,
        heartbeat_timeout: Duration,
        pool: InfraPool,
        rx: MRx<AgentHeartbeatOp>,
    ) -> Self {
        Self {
            agents: PriorityQueue::new(),
            cancel_token,
            heartbeat_timeout,
            pool,
            rx,
        }
    }

    fn handle_op(&mut self, op: AgentHeartbeatOp) {
        match op {
            AgentHeartbeatOp::Heartbeat(agent_id) => {
                self.agents
                    .push(agent_id, Reverse(Instant::now() + self.heartbeat_timeout));
            }
            AgentHeartbeatOp::Remove(agent_id) => {
                self.agents.remove(&agent_id);
            }
        }
    }

    /// Time until the earliest deadline, or a full timeout when idle.
    fn next_deadline(&self) -> Duration {
        self.agents
            .peek()
            .map(|(_, deadline)| deadline.0.saturating_duration_since(Instant::now()))
            .unwrap_or(self.heartbeat_timeout)
    }

    async fn handle_timeout(&mut self) -> crate::error::Result<()> {
        let expired = self
            .agents
            .peek()
            .is_some_and(|(_, deadline)| deadline.0 <= Instant::now());
        if !expired {
            return Ok(());
        }
        let (agent_id, _) = self.agents.pop().unwrap();

        let agent = match tokio::time::timeout(
            DB_TIMEOUT,
            Agent::Entity::find_by_id(agent_id).one(&self.pool.db),
        )
        .await
        {
            Ok(Ok(Some(agent))) => agent,
            Ok(Ok(None)) => {
                tracing::debug!(agent_id, "Agent gone before its heartbeat timeout fired");
                return Ok(());
            }
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => {
                tracing::warn!(agent_id, "Agent lookup timed out during heartbeat check");
                return Ok(());
            }
        };

        // An agent already parked Offline (e.g. a user shutdown) has had its
        // jobs and tasks handled; nothing to redo.
        if agent.state == AgentState::Offline {
            return Ok(());
        }

        tracing::info!(
            agent_id,
            agent_uuid = %agent.uuid,
            "Agent heartbeat timed out — marking offline and reclaiming its work"
        );

        let now = TimeDateTimeWithTimeZone::now_utc();
        match tokio::time::timeout(DB_TIMEOUT, mark_offline(&self.pool, agent_id, now)).await {
            Ok(res) => res?,
            Err(_) => {
                tracing::warn!(agent_id, "Marking the agent offline timed out");
                return Ok(());
            }
        }

        match tokio::time::timeout(
            DB_TIMEOUT,
            job::terminate_agent_jobs(&self.pool.db, agent_id, SuiteJobState::Lost, now),
        )
        .await
        {
            Ok(res) => {
                res?;
            }
            Err(_) => tracing::warn!(agent_id, "Marking the agent's jobs Lost timed out"),
        }

        match tokio::time::timeout(
            DB_TIMEOUT,
            job::reclaim_agent_tasks(&self.pool.db, agent.uuid, None, now),
        )
        .await
        {
            Ok(res) => {
                res?;
            }
            Err(_) => tracing::warn!(agent_id, "Reclaiming the agent's tasks timed out"),
        }

        Ok(())
    }

    pub async fn run(&mut self) {
        tracing::info!("Agent heartbeat queue started");
        let mut timeout_duration = self.heartbeat_timeout;
        loop {
            tokio::select! {
                biased;
                _ = self.cancel_token.cancelled() => break,
                op = self.rx.recv() => match op {
                    None => break,
                    Some(op) => {
                        self.handle_op(op);
                        timeout_duration = self.next_deadline();
                    }
                },
                _ = tokio::time::sleep(timeout_duration) => {
                    if let Err(e) = self.handle_timeout().await {
                        if self.cancel_token.is_cancelled() {
                            tracing::warn!("Agent timeout handling failed during shutdown: {e}");
                        } else {
                            tracing::error!("Agent heartbeat timeout handling failed: {e}");
                        }
                    }
                    timeout_duration = self.next_deadline();
                }
            }
        }
        tracing::info!("Agent heartbeat queue stopped");
    }
}

/// Park an agent `Offline` and drop its suite assignment.
pub(crate) async fn mark_offline(
    pool: &InfraPool,
    agent_id: i64,
    now: TimeDateTimeWithTimeZone,
) -> crate::error::Result<()> {
    Agent::Entity::update_many()
        .col_expr(Agent::Column::State, Expr::value(AgentState::Offline))
        .col_expr(Agent::Column::AssignedTaskSuiteId, Expr::value(None::<i64>))
        .col_expr(Agent::Column::UpdatedAt, Expr::value(now))
        .filter(Agent::Column::Id.eq(agent_id))
        .exec(&pool.db)
        .await?;
    Ok(())
}
