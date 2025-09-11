use std::{cmp::Reverse, time::Duration};

use priority_queue::PriorityQueue;
use sea_orm::prelude::*;
#[cfg(not(feature = "crossfire-channel"))]
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::{config::InfraPool, entity::workers as Worker};

// MARK: HeartbeatQueue
#[derive(Debug)]
pub struct HeartbeatQueue {
    pub workers: PriorityQueue<i64, Reverse<Instant>>,
    cancel_token: CancellationToken,
    heartbeat_timeout: Duration,
    pool: InfraPool,
    #[cfg(not(feature = "crossfire-channel"))]
    rx: UnboundedReceiver<HeartbeatOp>,
    #[cfg(feature = "crossfire-channel")]
    rx: crossfire::AsyncRx<HeartbeatOp>,
}

pub enum HeartbeatOp {
    UnregisterWorker(i64),
    Heartbeat(i64),
}

impl HeartbeatQueue {
    pub fn new(
        cancel_token: CancellationToken,
        heartbeat_timeout: Duration,
        pool: InfraPool,
        #[cfg(not(feature = "crossfire-channel"))] rx: UnboundedReceiver<HeartbeatOp>,
        #[cfg(feature = "crossfire-channel")] rx: crossfire::AsyncRx<HeartbeatOp>,
    ) -> Self {
        Self {
            workers: PriorityQueue::new(),
            cancel_token,
            heartbeat_timeout,
            pool,
            rx,
        }
    }

    fn unregister_worker(&mut self, worker_id: i64) {
        tracing::debug!("worker {} unregistered", worker_id);
        self.workers.remove(&worker_id);
    }

    fn heartbeat(&mut self, worker_id: i64) {
        tracing::debug!("worker {} heartbeat", worker_id);
        self.workers
            .push(worker_id, Reverse(Instant::now() + self.heartbeat_timeout));
    }

    fn handle_op(&mut self, op: HeartbeatOp) {
        match op {
            HeartbeatOp::UnregisterWorker(worker_id) => {
                self.unregister_worker(worker_id);
            }
            HeartbeatOp::Heartbeat(worker_id) => {
                self.heartbeat(worker_id);
            }
        }
    }

    async fn handle_timeout(&mut self) -> crate::error::Result<()> {
        if let Some(true) = self.workers.peek().map(|(_, r)| r.0 <= Instant::now()) {
            let (worker_id, _) = self.workers.pop().unwrap();
            if let Some(worker) = Worker::Entity::find_by_id(worker_id)
                .one(&self.pool.db)
                .await?
            {
                super::remove_worker(worker, &self.pool).await?;
            }
        }
        Ok(())
    }

    pub async fn run(&mut self) {
        let mut timeout_duration = self.heartbeat_timeout;
        #[cfg(not(feature = "crossfire-channel"))]
        loop {
            tokio::select! {
                biased;
                _ = self.cancel_token.cancelled() => {
                    break;
                }
                op = self.rx.recv() => match op {
                    None => {
                        break;
                    }
                    Some(op) => {
                        self.handle_op(op);
                        timeout_duration = self
                            .workers
                            .peek()
                            .map(|(_, r)| r.0 - Instant::now())
                            .unwrap_or(self.heartbeat_timeout);
                    }
                },
                _ = tokio::time::sleep(timeout_duration) => {
                    if let Err(e) = self.handle_timeout().await {
                        tracing::error!("handle timeout failed: {:?}", e);
                        self.cancel_token.cancel();
                        break;
                    }
                    timeout_duration = self
                            .workers
                            .peek()
                            .map(|(_, r)| r.0 - Instant::now())
                            .unwrap_or(self.heartbeat_timeout);
                }
            }
        }
        #[cfg(feature = "crossfire-channel")]
        loop {
            tokio::select! {
                biased;
                _ = self.cancel_token.cancelled() => {
                    break;
                }
                op = self.rx.recv() => match op.ok() {
                    None => {
                        break;
                    }
                    Some(op) => {
                        self.handle_op(op);
                        timeout_duration = self
                            .workers
                            .peek()
                            .map(|(_, r)| r.0 - Instant::now())
                            .unwrap_or(self.heartbeat_timeout);
                    }
                },
                _ = tokio::time::sleep(timeout_duration) => {
                    if let Err(e) = self.handle_timeout().await {
                        tracing::error!("handle timeout failed: {:?}", e);
                        self.cancel_token.cancel();
                        break;
                    }
                    timeout_duration = self
                            .workers
                            .peek()
                            .map(|(_, r)| r.0 - Instant::now())
                            .unwrap_or(self.heartbeat_timeout);
                }
            }
        }
    }
}
