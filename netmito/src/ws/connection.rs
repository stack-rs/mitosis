//! WebSocket connection registry for tracking connected agents

use std::collections::{HashMap, VecDeque};

use axum::extract::ws::Message;
use speedy::Writable;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::schema::{AgentNotification, WsNotificationEvent};

/// Internal command for the WebSocket router
#[derive(Debug)]
pub enum RouterOp {
    AgentWsRegister {
        uuid: Uuid,
        sender: crossfire::MAsyncTx<Message>,
    },
    AgentWsUnregister {
        uuid: Uuid,
    },
    RemoveAgent {
        uuid: Uuid,
    },
    AgentAckBy {
        uuid: Uuid,
        id: u64,
    },
    NotifyAgent {
        uuid: Uuid,
        event: AgentNotification,
    },
    GetNotifications {
        uuid: Uuid,
        tx: oneshot::Sender<Vec<WsNotificationEvent>>,
    },
    GetCounter {
        uuid: Uuid,
        tx: oneshot::Sender<Option<u64>>,
    },
}

/// Session of a single agent (connection + notifications)
struct AgentSession {
    /// Active WebSocket connection (if any)
    sender: Option<crossfire::MAsyncTx<Message>>,
    /// Monotonically increasing notification counter
    counter: u64,
    /// Ring buffer of recent notifications (for heartbeat catch-up)
    buffer: VecDeque<WsNotificationEvent>,
}

impl AgentSession {
    fn new() -> Self {
        Self {
            sender: None,
            counter: 0,
            buffer: VecDeque::with_capacity(8),
        }
    }

    fn add_notification(&mut self, event: AgentNotification) -> WsNotificationEvent {
        self.counter = self.counter.saturating_add(1);

        let event = WsNotificationEvent {
            id: self.counter,
            event,
        };

        tracing::trace!(
            notification_event = ?event,
            "Adding notification to agent session buffer"
        );

        self.buffer.push_back(event.clone());

        event
    }

    /// Remove notifications that have been acknowledged (id <= ack_id)
    /// Simplified identity-based ACK: since we replay the buffer on reconnect,
    /// we just need to remove exactly what the client confirms it has processed.
    fn ack(&mut self, ack_id: u64) {
        tracing::trace!(ack_id = ack_id, "Acknowledging notifications up to ID");
        if self.buffer.is_empty() {
            return;
        }
        if let Some(pos) = self.buffer.iter().position(|e| e.id == ack_id) {
            self.buffer.drain(..=pos);
        } else {
            tracing::debug!(
                ack_id = ack_id,
                "Acknowledged notification ID not found in buffer"
            );
        }
    }

    fn get_remaining_notifications(&mut self) -> Vec<WsNotificationEvent> {
        self.buffer.drain(..).collect()
    }

    /// Get all buffered notifications for replay without modifying the state
    fn peek_all(&self) -> Vec<WsNotificationEvent> {
        self.buffer.iter().cloned().collect()
    }
}

/// Central router task for managing WebSocket connections and notifications
pub struct AgentWsRouter {
    states: HashMap<Uuid, AgentSession>,
    cancel_token: CancellationToken,
    rx: crossfire::AsyncRx<RouterOp>,
}

impl AgentWsRouter {
    pub fn new(cancel_token: CancellationToken, rx: crossfire::AsyncRx<RouterOp>) -> Self {
        Self {
            states: HashMap::new(),
            cancel_token,
            rx,
        }
    }

    pub async fn run(&mut self) {
        tracing::info!("Agent WebSocket Router started");

        loop {
            tokio::select! {
                biased;
                _ = self.cancel_token.cancelled() => {
                    tracing::info!("Agent WebSocket Router received shutdown signal");
                    break;
                }
                op = self.rx.recv() => if self.handle_op(op.ok()).await {
                    self.cancel_token.cancel();
                    break;
                }
            }
        }
        tracing::info!("Agent WebSocket Router stopped");
    }

    pub async fn handle_op(&mut self, op: Option<RouterOp>) -> bool {
        match op {
            None => {
                return true;
            }
            Some(op) => match op {
                RouterOp::AgentWsRegister { uuid, sender } => {
                    let is_new = !self.states.contains_key(&uuid);
                    let state = self.states.entry(uuid).or_insert_with(AgentSession::new);
                    state.sender = Some(sender);

                    if is_new {
                        tracing::debug!(
                            agent_uuid = %uuid,
                            "New agent session registered via WebSocket (no notifications to replay)"
                        );
                    } else {
                        // Existing session: Replay the buffer.
                        // This ensures the client catches up on everything it missed,
                        // including any CounterSync events that happened while disconnected.
                        let events = state.peek_all();
                        if let Some(sender) = &state.sender {
                            for event in &events {
                                if let Err(e) = Self::send_to_sender(sender, event).await {
                                    tracing::debug!(
                                        agent_uuid = %uuid,
                                        notification_id = event.id,
                                        error = %e,
                                        "Failed to send replayed notification via WebSocket"
                                    );
                                    break;
                                }
                            }
                            tracing::trace!(
                                agent_uuid = %uuid,
                                count = events.len(),
                                "Replayed buffered notifications via WebSocket"
                            );
                        }
                    }

                    tracing::debug!(
                        agent_uuid = %uuid,
                        total_agents = self.states.len(),
                        "Agent WebSocket registered"
                    );
                }
                RouterOp::AgentWsUnregister { uuid } => {
                    if let Some(state) = self.states.get_mut(&uuid) {
                        state.sender = None;
                        tracing::debug!(
                            agent_uuid = %uuid,
                            "Agent WebSocket unregistered"
                        );
                    }
                }
                RouterOp::RemoveAgent { uuid } => {
                    if self.states.remove(&uuid).is_some() {
                        tracing::debug!(
                            agent_uuid = %uuid,
                            total_agents = self.states.len(),
                            "Agent removed from router"
                        );
                    }
                }
                RouterOp::AgentAckBy { uuid, id } => {
                    if let Some(state) = self.states.get_mut(&uuid) {
                        state.ack(id);
                    }
                }
                RouterOp::NotifyAgent { uuid, event } => {
                    let state = self.states.entry(uuid).or_insert_with(AgentSession::new);
                    let event = state.add_notification(event);

                    if let Some(sender) = &state.sender {
                        if let Err(e) = Self::send_to_sender(sender, &event).await {
                            tracing::debug!(
                                agent_uuid = %uuid,
                                notification_id = event.id,
                                error = %e,
                                "Failed to send notification via WebSocket"
                            );
                        } else {
                            tracing::trace!(
                                agent_uuid = %uuid,
                                notification_id = event.id,
                                "Sent notification via WebSocket"
                            );
                        }
                    }
                }
                RouterOp::GetNotifications { uuid, tx } => {
                    let raw_notifications = if let Some(state) = self.states.get_mut(&uuid) {
                        state.get_remaining_notifications()
                    } else {
                        Vec::new()
                    };

                    // TODO:  Merge and deduplicate notifications before sending
                    let notifications = Self::merge_notifications(raw_notifications);

                    if let Err(_) = tx.send(notifications) {
                        tracing::error!(
                            agent_uuid = %uuid,
                            "Failed to send notifications via oneshot channel"
                        );
                    }
                }
                RouterOp::GetCounter { uuid, tx } => {
                    let counter = self.states.get(&uuid).map(|s| s.counter);
                    if let Err(_) = tx.send(counter) {
                        tracing::error!(
                            agent_uuid = %uuid,
                            "Failed to send counter via oneshot channel"
                        )
                    };
                }
            },
        }
        false
    }

    /// Merge and deduplicate notifications to reduce message volume
    fn merge_notifications(notifications: Vec<WsNotificationEvent>) -> Vec<WsNotificationEvent> {
        use std::collections::{HashMap, HashSet};

        if notifications.is_empty() {
            return notifications;
        }

        let original_count = notifications.len();
        let mut merged: HashMap<String, WsNotificationEvent> = HashMap::new();
        let mut cancelled_tasks: HashSet<Uuid> = HashSet::new();
        let mut last_ping: Option<WsNotificationEvent> = None;
        let mut max_id = 0;

        for notif in notifications {
            max_id = max_id.max(notif.id);

            match &notif.event {
                AgentNotification::SuiteAvailable {
                    suite_uuid,
                    priority,
                } => {
                    let key = format!("suite_available_{:?}", suite_uuid);
                    // Keep if higher priority or first occurrence
                    merged
                        .entry(key)
                        .and_modify(|existing| {
                            if let AgentNotification::SuiteAvailable {
                                priority: old_priority,
                                ..
                            } = existing.event
                            {
                                if *priority > old_priority {
                                    *existing = notif.clone();
                                }
                            }
                        })
                        .or_insert(notif);
                }

                AgentNotification::PreemptSuite { new_suite_uuid, .. } => {
                    // Preempt overrides SuiteAvailable for same suite
                    let suite_key = format!("suite_available_{:?}", Some(new_suite_uuid));
                    merged.remove(&suite_key);
                    merged.insert(format!("preempt_{}", new_suite_uuid), notif);
                }

                AgentNotification::TasksCancelled { task_uuids } => {
                    cancelled_tasks.extend(task_uuids.iter());
                }

                AgentNotification::Ping { .. } => {
                    last_ping = Some(notif);
                }

                AgentNotification::CounterSync { .. } => {
                    // Always keep CounterSync (needed for sync)
                    merged.insert(format!("counter_sync_{}", notif.id), notif);
                }

                AgentNotification::Shutdown { .. } => {
                    // Always keep Shutdown (critical)
                    merged.insert("shutdown".to_string(), notif);
                }

                AgentNotification::SuiteCancelled { suite_uuid, .. } => {
                    merged.insert(format!("suite_cancelled_{}", suite_uuid), notif);
                }
            }
        }

        // Reconstruct merged list
        let mut result: Vec<WsNotificationEvent> = merged.into_values().collect();

        // Add merged cancelled tasks if any
        if !cancelled_tasks.is_empty() {
            result.push(WsNotificationEvent {
                id: max_id + 1,
                event: AgentNotification::TasksCancelled {
                    task_uuids: cancelled_tasks.into_iter().collect(),
                },
            });
        }

        // Add last ping if exists
        if let Some(ping) = last_ping {
            result.push(ping);
        }

        // Sort by ID to maintain order
        result.sort_by_key(|e| e.id);

        tracing::trace!(
            original_count = original_count,
            merged_count = result.len(),
            "Merged notifications"
        );

        result
    }

    async fn send_to_sender(
        sender: &crossfire::MAsyncTx<Message>,
        event: &WsNotificationEvent,
    ) -> Result<(), String> {
        let bytes = event
            .write_to_vec()
            .map_err(|e| format!("Failed to serialize notification: {}", e))?;

        sender
            .send(Message::Binary(bytes.into()))
            .await
            .map_err(|_| "Channel closed".to_string())
    }

    pub(crate) fn register(
        tx: &crossfire::MTx<RouterOp>,
        uuid: Uuid,
        sender: crossfire::MAsyncTx<Message>,
    ) {
        let _ = tx.send(RouterOp::AgentWsRegister { uuid, sender });
    }

    pub(crate) fn unregister(tx: &crossfire::MTx<RouterOp>, uuid: Uuid) {
        let _ = tx.send(RouterOp::AgentWsUnregister { uuid });
    }

    pub(crate) fn ack(tx: &crossfire::MTx<RouterOp>, uuid: Uuid, id: u64) {
        let _ = tx.send(RouterOp::AgentAckBy { uuid, id });
    }

    pub(crate) fn notify(
        tx: &crossfire::MTx<RouterOp>,
        uuid: Uuid,
        event: AgentNotification,
    ) -> Result<(), String> {
        tx.send(RouterOp::NotifyAgent { uuid, event })
            .map_err(|_| "Router channel closed".to_string())
    }
}
