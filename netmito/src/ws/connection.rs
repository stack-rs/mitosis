//! Per-agent notification sessions and the router actor that owns them.
//!
//! [`AgentWsRouter`] owns every agent's notification state. The service layer
//! sends a [`RouterOp`] to the router and the router applies it. Each agent gets
//! an [`AgentSession`] that contains a sequence counter, a buffer of notifications
//! it has not acknowledged, and, while connected, the sender half of its WebSocket.
//!
//! A notification always lands in the buffer and is pushed over the socket when
//! one is attached; it leaves the buffer only once the agent confirms it, over
//! the socket ([`RouterOp::AckBy`]) or via the `last_notification_id` on its
//! next heartbeat. So a session outlives its socket — a reconnect replays what
//! is buffered, a heartbeat reads it — and ends only with the agent's process,
//! at [`RouterOp::ResetBuffer`].

use std::collections::{HashMap, VecDeque};

use axum::extract::ws::Message;
use crossfire::{AsyncRx, MTx};
use speedy::Writable;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::schema::{AgentNotification, WsNotificationEvent};

/// Command for the [`AgentWsRouter`] actor.
#[derive(Debug)]
pub enum RouterOp {
    /// A WebSocket connected; attach its sender to the agent's session.
    Register { uuid: Uuid, sender: MTx<Message> },
    /// The WebSocket dropped; the session and its buffer survive.
    Unregister { uuid: Uuid },
    /// The process that held this uuid is over — retired, or displaced by a
    /// registration; drop what was queued for it, since a `Shutdown` replayed
    /// to its successor would kill the wrong one. The counter is kept, so ids
    /// stay monotonic for this boot.
    ResetBuffer { uuid: Uuid },
    /// The agent processed everything up to and including `id`.
    AckBy { uuid: Uuid, id: u64 },
    /// Queue a notification (and push it over the socket if connected).
    Notify {
        uuid: Uuid,
        event: AgentNotification,
    },
    /// Heartbeat catch-up: acknowledge through `ack_by_id`, then return
    /// everything still buffered beyond it **without** dropping it.
    PendingNotifications {
        uuid: Uuid,
        ack_by_id: u64,
        tx: oneshot::Sender<Vec<WsNotificationEvent>>,
    },
    /// Read the agent's current sequence counter (`None` if unknown).
    GetCounter {
        uuid: Uuid,
        tx: oneshot::Sender<Option<u64>>,
    },
}

/// One agent's notification state.
struct AgentSession {
    /// Sender of the live WebSocket, if any.
    sender: Option<MTx<Message>>,
    /// Last allocated sequence ID.
    counter: u64,
    /// Notifications not yet acknowledged, oldest first.
    buffer: VecDeque<WsNotificationEvent>,
}

impl AgentSession {
    fn new() -> Self {
        Self {
            sender: None,
            counter: 0,
            buffer: VecDeque::new(),
        }
    }

    /// Queue an event, unless one already waiting makes it redundant.
    ///
    /// `None` means nothing was queued and nothing should be pushed: a nudge
    /// the agent has not acted on yet is still the same nudge.
    fn push(&mut self, event: AgentNotification) -> Option<WsNotificationEvent> {
        if self.buffer.iter().any(|e| e.event.coalesces_with(&event)) {
            return None;
        }
        self.counter = self.counter.saturating_add(1);
        let event = WsNotificationEvent {
            id: self.counter,
            event,
        };
        self.buffer.push_back(event.clone());
        Some(event)
    }

    /// Drop everything the agent has confirmed it processed.
    fn ack(&mut self, ack_id: u64) {
        while self.buffer.front().is_some_and(|e| e.id <= ack_id) {
            self.buffer.pop_front();
        }
    }

    fn pending_after(&self, after_id: u64) -> Vec<WsNotificationEvent> {
        self.buffer
            .iter()
            .filter(|e| e.id > after_id)
            .cloned()
            .collect()
    }
}

/// Actor owning every agent's notification session.
pub struct AgentWsRouter {
    sessions: HashMap<Uuid, AgentSession>,
    cancel_token: CancellationToken,
    rx: AsyncRx<RouterOp>,
}

impl AgentWsRouter {
    pub fn new(cancel_token: CancellationToken, rx: AsyncRx<RouterOp>) -> Self {
        Self {
            sessions: HashMap::new(),
            cancel_token,
            rx,
        }
    }

    pub async fn run(&mut self) {
        tracing::info!("Agent WebSocket router started");
        loop {
            tokio::select! {
                biased;
                _ = self.cancel_token.cancelled() => break,
                op = self.rx.recv() => match op.ok() {
                    None => break,
                    Some(op) => self.handle_op(op),
                },
            }
        }
        tracing::info!("Agent WebSocket router stopped");
    }

    fn handle_op(&mut self, op: RouterOp) {
        match op {
            RouterOp::Register { uuid, sender } => {
                let session = self.sessions.entry(uuid).or_insert_with(AgentSession::new);
                session.sender = Some(sender);
                // Replay whatever is still unacknowledged so a reconnecting
                // agent catches up without waiting for a heartbeat.
                let pending = session.pending_after(0);
                let sender = session.sender.clone();
                if let Some(sender) = sender {
                    for event in &pending {
                        if !Self::push_to_socket(&sender, event, uuid) {
                            break;
                        }
                    }
                }
                tracing::debug!(
                    agent_uuid = %uuid,
                    replayed = pending.len(),
                    connected_agents = self.sessions.len(),
                    "Agent WebSocket registered"
                );
            }
            RouterOp::Unregister { uuid } => {
                if let Some(session) = self.sessions.get_mut(&uuid) {
                    session.sender = None;
                }
                tracing::debug!(agent_uuid = %uuid, "Agent WebSocket unregistered");
            }
            RouterOp::ResetBuffer { uuid } => {
                if let Some(session) = self.sessions.get_mut(&uuid) {
                    let dropped = session.buffer.len();
                    session.buffer.clear();
                    tracing::debug!(
                        agent_uuid = %uuid,
                        dropped,
                        "Dropped what was queued for a process that is over"
                    );
                }
            }
            RouterOp::AckBy { uuid, id } => {
                if let Some(session) = self.sessions.get_mut(&uuid) {
                    session.ack(id);
                }
            }
            RouterOp::Notify { uuid, event } => {
                let session = self.sessions.entry(uuid).or_insert_with(AgentSession::new);
                let Some(event) = session.push(event) else {
                    return;
                };
                if let Some(sender) = session.sender.clone() {
                    Self::push_to_socket(&sender, &event, uuid);
                }
            }
            RouterOp::PendingNotifications {
                uuid,
                ack_by_id: after_id,
                tx,
            } => {
                let pending = match self.sessions.get_mut(&uuid) {
                    Some(session) => {
                        session.ack(after_id);
                        session.pending_after(after_id)
                    }
                    None => Vec::new(),
                };
                let _ = tx.send(pending);
            }
            RouterOp::GetCounter { uuid, tx } => {
                let _ = tx.send(self.sessions.get(&uuid).map(|s| s.counter));
            }
        }
    }

    fn push_to_socket(sender: &MTx<Message>, event: &WsNotificationEvent, uuid: Uuid) -> bool {
        let payload = match event.write_to_vec() {
            Ok(payload) => payload,
            Err(e) => {
                tracing::error!(agent_uuid = %uuid, "Failed to serialize notification: {e}");
                return false;
            }
        };
        match sender.send(Message::Binary(payload.into())) {
            Ok(()) => true,
            Err(e) => {
                tracing::debug!(
                    agent_uuid = %uuid,
                    notification_id = event.id,
                    "Notification not pushed over WebSocket ({e}); left buffered for heartbeat"
                );
                false
            }
        }
    }

    // ── convenience senders, used from the service layer ──

    pub fn register(tx: &MTx<RouterOp>, uuid: Uuid, sender: MTx<Message>) {
        let _ = tx.send(RouterOp::Register { uuid, sender });
    }

    pub fn unregister(tx: &MTx<RouterOp>, uuid: Uuid) {
        let _ = tx.send(RouterOp::Unregister { uuid });
    }

    pub fn reset(tx: &MTx<RouterOp>, uuid: Uuid) {
        let _ = tx.send(RouterOp::ResetBuffer { uuid });
    }

    pub fn ack(tx: &MTx<RouterOp>, uuid: Uuid, id: u64) {
        let _ = tx.send(RouterOp::AckBy { uuid, id });
    }

    /// Queue a notification for an agent. Best effort: a closed router (only
    /// possible during shutdown) is logged and ignored, never propagated into a
    /// request handler.
    pub fn notify(tx: &MTx<RouterOp>, uuid: Uuid, event: AgentNotification) {
        if tx.send(RouterOp::Notify { uuid, event }).is_err() {
            tracing::debug!(agent_uuid = %uuid, "Notification dropped: WebSocket router is down");
        }
    }

    /// Acknowledge through `after_id` and read back what is still pending.
    pub async fn pending_notifications(
        tx: &MTx<RouterOp>,
        uuid: Uuid,
        ack_by_id: u64,
    ) -> Vec<WsNotificationEvent> {
        let (resp_tx, resp_rx) = oneshot::channel();
        if tx
            .send(RouterOp::PendingNotifications {
                uuid,
                ack_by_id,
                tx: resp_tx,
            })
            .is_err()
        {
            return Vec::new();
        }
        resp_rx.await.unwrap_or_default()
    }

    pub async fn counter(tx: &MTx<RouterOp>, uuid: Uuid) -> Option<u64> {
        let (resp_tx, resp_rx) = oneshot::channel();
        if tx.send(RouterOp::GetCounter { uuid, tx: resp_tx }).is_err() {
            return None;
        }
        resp_rx.await.ok().flatten()
    }
}
