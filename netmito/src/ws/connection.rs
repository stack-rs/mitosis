//! Per-agent notification sessions and the router actor that owns them.
//!
//! The service layer sends a [`RouterOp`] to [`AgentWsRouter`], and the router applies it. Each
//! agent has an [`AgentSession`]: a sequence counter, a buffer of notifications that are not
//! acknowledged, and the sender of its WebSocket while one is connected.
//!
//! A notification always goes into the buffer. If a socket is connected, the router also sends it
//! on the socket. The notification leaves the buffer only when the agent acknowledges it: on the
//! socket ([`RouterOp::AckBy`]), or with the `last_notification_id` of its next heartbeat. Thus a
//! session continues after its socket closes. A new socket gets the buffered notifications again,
//! and a heartbeat reads them. A session ends only with the agent process, at
//! [`RouterOp::ResetBuffer`].

use std::collections::{HashMap, VecDeque};

use axum::extract::ws::Message;
use crossfire::{AsyncRx, MTx, Tx};
use speedy::Writable;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::schema::{AgentNotification, WsNotificationEvent};

#[derive(Debug)]
pub enum RouterOp {
    /// A WebSocket connected. Attach its sender to the session of the agent.
    Register { uuid: Uuid, sender: Tx<Message> },
    /// The WebSocket closed. The session and its buffer stay.
    Unregister { uuid: Uuid },
    /// The process that had this uuid has stopped. Remove its buffered
    /// notifications, because a `Shutdown` sent again to the next process
    /// would stop the wrong process. The counter stays, thus the ids continue
    /// to increase while this coordinator runs.
    ResetBuffer { uuid: Uuid },
    /// The agent processed all notifications up to and including `id`.
    AckBy { uuid: Uuid, id: u64 },
    /// Buffer a notification, and send it on the socket if one is connected.
    Notify {
        uuid: Uuid,
        event: AgentNotification,
    },
    /// For a heartbeat: acknowledge up to `ack_by_id`, then return all
    /// notifications after it. The returned notifications stay in the buffer.
    PendingNotifications {
        uuid: Uuid,
        ack_by_id: u64,
        tx: Tx<Vec<WsNotificationEvent>>,
    },
    /// Read the sequence counter of the agent, `None` if it has no session.
    GetCounter { uuid: Uuid, tx: Tx<Option<u64>> },
}

struct AgentSession {
    /// Sender of the connected WebSocket, if any.
    sender: Option<Tx<Message>>,
    /// The last sequence id given to a notification.
    counter: u64,
    /// Notifications that are not acknowledged, oldest first.
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

    fn push(&mut self, event: AgentNotification) -> WsNotificationEvent {
        self.counter = self.counter.saturating_add(1);
        let event = WsNotificationEvent {
            id: self.counter,
            event,
        };
        self.buffer.push_back(event.clone());
        event
    }

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

/// The actor that owns the notification sessions of all agents.
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
                // Send the buffered notifications again, so that the agent
                // does not wait for its next heartbeat to get them.
                let pending = session.pending_after(0);
                if let Some(sender) = session.sender.as_ref() {
                    for event in &pending {
                        if !Self::push_to_socket(sender, event, uuid) {
                            break;
                        }
                    }
                }
                tracing::debug!(
                    agent_uuid = %uuid,
                    replayed = pending.len(),
                    sessions = self.sessions.len(),
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
                        "Dropped the notifications of a stopped agent process"
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
                let event = session.push(event);
                if let Some(sender) = session.sender.as_ref() {
                    Self::push_to_socket(sender, &event, uuid);
                }
            }
            RouterOp::PendingNotifications {
                uuid,
                ack_by_id,
                tx,
            } => {
                let pending = match self.sessions.get_mut(&uuid) {
                    Some(session) => {
                        session.ack(ack_by_id);
                        session.pending_after(ack_by_id)
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

    /// Return false if the socket did not accept the notification. The
    /// notification stays in the buffer for the next heartbeat.
    fn push_to_socket(sender: &Tx<Message>, event: &WsNotificationEvent, uuid: Uuid) -> bool {
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
                    "Notification not sent on WebSocket ({e}); kept for heartbeat"
                );
                false
            }
        }
    }

    pub fn register(tx: &MTx<RouterOp>, uuid: Uuid, sender: Tx<Message>) {
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

    /// Queue a notification for an agent. The router stops only during
    /// shutdown, thus a failed send is logged and not returned to the request
    /// handler.
    pub fn notify(tx: &MTx<RouterOp>, uuid: Uuid, event: AgentNotification) {
        if tx.send(RouterOp::Notify { uuid, event }).is_err() {
            tracing::debug!(agent_uuid = %uuid, "Notification dropped: WebSocket router is down");
        }
    }

    /// Acknowledge up to `ack_by_id`, then read the notifications after it.
    pub async fn pending_notifications(
        tx: &MTx<RouterOp>,
        uuid: Uuid,
        ack_by_id: u64,
    ) -> Vec<WsNotificationEvent> {
        let (resp_tx, resp_rx) = crossfire::spsc::unbounded_async::<Vec<WsNotificationEvent>>();
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
        resp_rx.recv().await.unwrap_or_default()
    }

    pub async fn counter(tx: &MTx<RouterOp>, uuid: Uuid) -> Option<u64> {
        let (resp_tx, resp_rx) = crossfire::spsc::unbounded_async::<Option<u64>>();
        if tx.send(RouterOp::GetCounter { uuid, tx: resp_tx }).is_err() {
            return None;
        }
        resp_rx.recv().await.ok().flatten()
    }
}
