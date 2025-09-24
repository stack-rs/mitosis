use std::collections::{HashMap, HashSet};

use axum::response::sse::{Event, KeepAlive, Sse};
use futures::{
    pin_mut,
    stream::{self, Stream},
    StreamExt,
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
#[cfg(not(feature = "crossfire-channel"))]
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    entity::state::{TaskExecState, TaskState},
    error::Result,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskStateChangeNotification {
    pub task_uuid: Uuid,
    pub old_state: TaskExecState,
    pub new_state: TaskExecState,
    pub timestamp: OffsetDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionCredentials {
    pub session_id: String,
    pub client_id: String,
}

#[derive(Debug)]
struct ClientSession {
    client_id: String,
    sender: UnboundedSender<TaskStateChangeNotification>,
    subscribed_tasks: HashSet<Uuid>,
}

fn task_state_to_exec_state(task_state: TaskState) -> TaskExecState {
    match task_state {
        TaskState::Pending | TaskState::Ready => TaskExecState::ExecPending,
        TaskState::Running => TaskExecState::ExecSpawned,
        TaskState::Finished => TaskExecState::ExecFinished,
        TaskState::Cancelled => TaskExecState::UploadCancelledResult,
        TaskState::Unknown => TaskExecState::Unknown,
    }
}

#[derive(Debug)]
pub struct SubscriptionManager {
    /// Map from task UUID to set of session IDs subscribed to that task
    task_subscriptions: HashMap<Uuid, HashSet<String>>,
    /// Map from session ID to client session
    client_sessions: HashMap<String, ClientSession>,
    /// Cancellation token for shutdown
    cancel_token: CancellationToken,
    /// Channel receiver for subscription operations
    #[cfg(not(feature = "crossfire-channel"))]
    rx: UnboundedReceiver<SubscriptionManagerOp>,
    #[cfg(feature = "crossfire-channel")]
    rx: crossfire::AsyncRx<SubscriptionManagerOp>,
}

#[derive(Debug)]
pub enum SubscriptionManagerOp {
    /// Client subscribes to a task
    Subscribe {
        session_id: String,
        task_uuid: Uuid,
        response_tx: oneshot::Sender<Result<()>>,
    },
    /// Client unsubscribes from a task
    Unsubscribe {
        session_id: String,
        task_uuid: Uuid,
        response_tx: oneshot::Sender<Result<bool>>,
    },
    /// Client registers for SSE stream and gets session credentials
    RegisterClient {
        client_id: String,
        sender: UnboundedSender<TaskStateChangeNotification>,
        response_tx: oneshot::Sender<Result<SessionCredentials>>,
    },
    /// Client disconnects
    UnregisterClient { session_id: String },
    /// Get client subscriptions
    GetClientSubscriptions {
        session_id: String,
        response_tx: oneshot::Sender<Result<Vec<Uuid>>>,
    },
    /// Unsubscribe client from all tasks
    UnsubscribeAll {
        session_id: String,
        response_tx: oneshot::Sender<Result<Vec<Uuid>>>,
    },
    /// Publish task state change notification
    PublishStateChange {
        task_uuid: Uuid,
        old_state: TaskState,
        new_state: TaskState,
    },
}

impl SubscriptionManager {
    pub fn new(
        cancel_token: CancellationToken,
        #[cfg(not(feature = "crossfire-channel"))] rx: UnboundedReceiver<SubscriptionManagerOp>,
        #[cfg(feature = "crossfire-channel")] rx: crossfire::AsyncRx<SubscriptionManagerOp>,
    ) -> Self {
        Self {
            task_subscriptions: HashMap::new(),
            client_sessions: HashMap::new(),
            cancel_token,
            rx,
        }
    }

    fn subscribe_session_to_task(&mut self, session_id: String, task_uuid: Uuid) -> Result<()> {
        // Add to task subscriptions
        self.task_subscriptions
            .entry(task_uuid)
            .or_default()
            .insert(session_id.clone());

        // Add to session's subscribed tasks
        if let Some(session) = self.client_sessions.get_mut(&session_id) {
            session.subscribed_tasks.insert(task_uuid);
            tracing::debug!("Session {} subscribed to task {}", session_id, task_uuid);
            Ok(())
        } else {
            Err(crate::error::Error::Custom("Session not found".to_string()))
        }
    }

    fn unsubscribe_session_from_task(&mut self, session_id: &str, task_uuid: Uuid) -> Result<bool> {
        let mut removed = false;

        // Remove from task subscriptions
        if let Some(sessions) = self.task_subscriptions.get_mut(&task_uuid) {
            removed = sessions.remove(session_id);
            if sessions.is_empty() {
                self.task_subscriptions.remove(&task_uuid);
            }
        }

        // Remove from session's subscribed tasks
        if let Some(session) = self.client_sessions.get_mut(session_id) {
            session.subscribed_tasks.remove(&task_uuid);
        }

        if removed {
            tracing::debug!(
                "Session {} unsubscribed from task {}",
                session_id,
                task_uuid
            );
        }

        Ok(removed)
    }

    fn register_client(
        &mut self,
        client_id: String,
        sender: UnboundedSender<TaskStateChangeNotification>,
    ) -> Result<SessionCredentials> {
        // Generate a random session ID
        let session_id = Uuid::new_v4().to_string();

        let session = ClientSession {
            client_id: client_id.clone(),
            sender,
            subscribed_tasks: HashSet::new(),
        };

        self.client_sessions.insert(session_id.clone(), session);
        tracing::debug!(
            "Client {} registered for SSE with session {}",
            client_id,
            session_id
        );

        Ok(SessionCredentials {
            session_id,
            client_id,
        })
    }

    fn unregister_session(&mut self, session_id: &str) -> Vec<Uuid> {
        if let Some(session) = self.client_sessions.remove(session_id) {
            let unsubscribed_tasks: Vec<Uuid> = session.subscribed_tasks.iter().cloned().collect();

            // Remove session from all task subscriptions
            for task_uuid in &unsubscribed_tasks {
                if let Some(sessions) = self.task_subscriptions.get_mut(task_uuid) {
                    sessions.remove(session_id);
                    if sessions.is_empty() {
                        self.task_subscriptions.remove(task_uuid);
                    }
                }
            }

            if !unsubscribed_tasks.is_empty() {
                tracing::debug!(
                    "Session {} (client {}) unregistered and unsubscribed from {} tasks",
                    session_id,
                    session.client_id,
                    unsubscribed_tasks.len()
                );
            }

            unsubscribed_tasks
        } else {
            Vec::new()
        }
    }

    fn get_session_subscriptions(&self, session_id: &str) -> Result<Vec<Uuid>> {
        if let Some(session) = self.client_sessions.get(session_id) {
            Ok(session.subscribed_tasks.iter().cloned().collect())
        } else {
            Err(crate::error::Error::Custom("Session not found".to_string()))
        }
    }

    fn unsubscribe_all(&mut self, session_id: &str) -> Result<Vec<Uuid>> {
        if let Some(session) = self.client_sessions.get_mut(session_id) {
            let unsubscribed_tasks: Vec<Uuid> = session.subscribed_tasks.drain().collect();

            // Remove session from all task subscriptions
            for task_uuid in &unsubscribed_tasks {
                if let Some(sessions) = self.task_subscriptions.get_mut(task_uuid) {
                    sessions.remove(session_id);
                    if sessions.is_empty() {
                        self.task_subscriptions.remove(task_uuid);
                    }
                }
            }

            if !unsubscribed_tasks.is_empty() {
                tracing::debug!(
                    "Session {} unsubscribed from {} tasks",
                    session_id,
                    unsubscribed_tasks.len()
                );
            }

            Ok(unsubscribed_tasks)
        } else {
            Err(crate::error::Error::Custom("Session not found".to_string()))
        }
    }

    fn publish_state_change(
        &mut self,
        task_uuid: Uuid,
        old_state: TaskState,
        new_state: TaskState,
    ) {
        let notification = TaskStateChangeNotification {
            task_uuid,
            old_state: task_state_to_exec_state(old_state),
            new_state: task_state_to_exec_state(new_state),
            timestamp: OffsetDateTime::now_utc(),
        };

        if let Some(subscribed_sessions) = self.task_subscriptions.get(&task_uuid) {
            let mut disconnected_sessions = Vec::new();
            let session_count = subscribed_sessions.len();

            for session_id in subscribed_sessions {
                if let Some(session) = self.client_sessions.get(session_id) {
                    if session.sender.send(notification.clone()).is_err() {
                        // Session disconnected, mark for removal
                        disconnected_sessions.push(session_id.clone());
                    }
                }
            }

            // Clean up disconnected sessions
            for session_id in disconnected_sessions {
                self.unregister_session(&session_id);
            }

            tracing::debug!(
                "Published task state change for {} to {} sessions: {:?} -> {:?}",
                task_uuid,
                session_count,
                old_state,
                new_state
            );
        }
    }

    fn handle_op(&mut self, op: Option<SubscriptionManagerOp>) -> bool {
        match op {
            None => return true,
            Some(op) => match op {
                SubscriptionManagerOp::Subscribe {
                    session_id,
                    task_uuid,
                    response_tx,
                } => {
                    let result = self.subscribe_session_to_task(session_id, task_uuid);
                    let _ = response_tx.send(result);
                }
                SubscriptionManagerOp::Unsubscribe {
                    session_id,
                    task_uuid,
                    response_tx,
                } => {
                    let result = self.unsubscribe_session_from_task(&session_id, task_uuid);
                    let _ = response_tx.send(result);
                }
                SubscriptionManagerOp::RegisterClient {
                    client_id,
                    sender,
                    response_tx,
                } => {
                    let result = self.register_client(client_id, sender);
                    let _ = response_tx.send(result);
                }
                SubscriptionManagerOp::UnregisterClient { session_id } => {
                    self.unregister_session(&session_id);
                }
                SubscriptionManagerOp::GetClientSubscriptions {
                    session_id,
                    response_tx,
                } => {
                    let result = self.get_session_subscriptions(&session_id);
                    let _ = response_tx.send(result);
                }
                SubscriptionManagerOp::UnsubscribeAll {
                    session_id,
                    response_tx,
                } => {
                    let result = self.unsubscribe_all(&session_id);
                    let _ = response_tx.send(result);
                }
                SubscriptionManagerOp::PublishStateChange {
                    task_uuid,
                    old_state,
                    new_state,
                } => {
                    self.publish_state_change(task_uuid, old_state, new_state);
                }
            },
        }
        false
    }

    pub async fn run(&mut self) {
        #[cfg(not(feature = "crossfire-channel"))]
        loop {
            tokio::select! {
                biased;
                _ = self.cancel_token.cancelled() => {
                    tracing::info!("Subscription manager shutting down");
                    break;
                }
                op = self.rx.recv() => {
                    if self.handle_op(op) {
                        break;
                    }
                }
            }
        }

        #[cfg(feature = "crossfire-channel")]
        loop {
            tokio::select! {
                biased;
                _ = self.cancel_token.cancelled() => {
                    tracing::info!("Subscription manager shutting down");
                    break;
                }
                op = self.rx.recv() => {
                    if self.handle_op(op.ok()) {
                        break;
                    }
                }
            }
        }
    }
}

// Helper functions for sending operations to the subscription manager
pub async fn subscribe_to_task(
    tx: &UnboundedSender<SubscriptionManagerOp>,
    session_id: String,
    task_uuid: Uuid,
) -> Result<()> {
    let (response_tx, response_rx) = oneshot::channel();
    let op = SubscriptionManagerOp::Subscribe {
        session_id,
        task_uuid,
        response_tx,
    };

    tx.send(op)
        .map_err(|_| crate::error::Error::Custom("Subscription manager unavailable".to_string()))?;
    response_rx
        .await
        .map_err(|_| crate::error::Error::Custom("Response channel closed".to_string()))?
}

pub async fn unsubscribe_from_task(
    tx: &UnboundedSender<SubscriptionManagerOp>,
    session_id: String,
    task_uuid: Uuid,
) -> Result<bool> {
    let (response_tx, response_rx) = oneshot::channel();
    let op = SubscriptionManagerOp::Unsubscribe {
        session_id,
        task_uuid,
        response_tx,
    };

    tx.send(op)
        .map_err(|_| crate::error::Error::Custom("Subscription manager unavailable".to_string()))?;
    response_rx
        .await
        .map_err(|_| crate::error::Error::Custom("Response channel closed".to_string()))?
}

pub async fn get_client_subscriptions(
    tx: &UnboundedSender<SubscriptionManagerOp>,
    session_id: String,
) -> Result<Vec<Uuid>> {
    let (response_tx, response_rx) = oneshot::channel();
    let op = SubscriptionManagerOp::GetClientSubscriptions {
        session_id,
        response_tx,
    };

    tx.send(op)
        .map_err(|_| crate::error::Error::Custom("Subscription manager unavailable".to_string()))?;
    response_rx
        .await
        .map_err(|_| crate::error::Error::Custom("Response channel closed".to_string()))?
}

pub async fn unsubscribe_all(
    tx: &UnboundedSender<SubscriptionManagerOp>,
    session_id: String,
) -> Result<Vec<Uuid>> {
    let (response_tx, response_rx) = oneshot::channel();
    let op = SubscriptionManagerOp::UnsubscribeAll {
        session_id,
        response_tx,
    };

    tx.send(op)
        .map_err(|_| crate::error::Error::Custom("Subscription manager unavailable".to_string()))?;
    response_rx
        .await
        .map_err(|_| crate::error::Error::Custom("Response channel closed".to_string()))?
}

pub fn publish_task_state_change(
    tx: &UnboundedSender<SubscriptionManagerOp>,
    task_uuid: Uuid,
    old_state: TaskState,
    new_state: TaskState,
) -> Result<()> {
    let op = SubscriptionManagerOp::PublishStateChange {
        task_uuid,
        old_state,
        new_state,
    };

    tx.send(op)
        .map_err(|_| crate::error::Error::Custom("Subscription manager unavailable".to_string()))?;
    Ok(())
}

pub async fn register_client_sse(
    tx: &UnboundedSender<SubscriptionManagerOp>,
    client_id: String,
) -> Result<(
    SessionCredentials,
    impl Stream<Item = std::result::Result<Event, axum::Error>>,
)> {
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let (response_tx, response_rx) = oneshot::channel();

    let op = SubscriptionManagerOp::RegisterClient {
        client_id: client_id.clone(),
        sender,
        response_tx,
    };

    tx.send(op)
        .map_err(|_| crate::error::Error::Custom("Subscription manager unavailable".to_string()))?;
    let credentials = response_rx
        .await
        .map_err(|_| crate::error::Error::Custom("Response channel closed".to_string()))??;

    // Send unregister message when stream is dropped
    let tx_clone = tx.clone();
    let session_id_clone = credentials.session_id.clone();

    let stream = stream::unfold(
        (receiver, tx_clone, session_id_clone, false),
        |(mut rx, tx, session_id, disconnected)| async move {
            if disconnected {
                return None;
            }

            match rx.recv().await {
                Some(notification) => match serde_json::to_string(&notification) {
                    Ok(json) => {
                        let event = Event::default().data(json);
                        Some((Ok(event), (rx, tx, session_id, false)))
                    }
                    Err(e) => {
                        tracing::error!("Failed to serialize notification: {}", e);
                        let _ = tx.send(SubscriptionManagerOp::UnregisterClient {
                            session_id: session_id.clone(),
                        });
                        None
                    }
                },
                None => {
                    // Channel closed, unregister session
                    let _ = tx.send(SubscriptionManagerOp::UnregisterClient {
                        session_id: session_id.clone(),
                    });
                    None
                }
            }
        },
    );

    Ok((credentials, stream))
}

pub fn create_sse_stream(
    client_id: String,
    tx: UnboundedSender<SubscriptionManagerOp>,
) -> Sse<impl Stream<Item = std::result::Result<Event, axum::Error>>> {
    Sse::new(async_stream::stream! {
        let register_result = register_client_sse(&tx, client_id).await;
        match register_result {
            Ok((credentials, stream)) => {
                // First, send the session credentials to the client
                let credentials_json = serde_json::to_string(&credentials)
                    .unwrap_or_else(|_| r#"{"error":"Failed to serialize credentials"}"#.to_string());
                let credentials_event = Event::default()
                    .event("session_credentials")
                    .data(credentials_json);
                yield Ok(credentials_event);

                // Then stream the notifications
                pin_mut!(stream);
                while let Some(event) = StreamExt::next(&mut stream).await {
                    yield event;
                }
            }
            Err(e) => {
                tracing::error!("Failed to register SSE client: {}", e);
                let error_event = Event::default()
                    .event("error")
                    .data(format!("Failed to register: {}", e));
                yield Ok(error_event);
            }
        }
    })
    .keep_alive(KeepAlive::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn test_subscription_manager() {
        let cancel_token = CancellationToken::new();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut manager = SubscriptionManager::new(cancel_token.clone(), rx);

        let client_id = "test-client".to_string();
        let task_uuid = Uuid::new_v4();

        // First register a client session
        let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let (register_tx, register_rx) = oneshot::channel();
        let register_op = SubscriptionManagerOp::RegisterClient {
            client_id: client_id.clone(),
            sender,
            response_tx: register_tx,
        };
        tx.send(register_op).unwrap();

        // Process register operation
        if let Some(op) = manager.rx.recv().await {
            manager.handle_op(Some(op));
        }

        // Get the session ID from the credentials
        let credentials = register_rx.await.unwrap().unwrap();
        let session_id = credentials.session_id;

        // Test subscribe
        let (resp_tx, _resp_rx) = oneshot::channel();
        let op = SubscriptionManagerOp::Subscribe {
            session_id: session_id.clone(),
            task_uuid,
            response_tx: resp_tx,
        };

        tx.send(op).unwrap();

        // Process subscribe operation
        if let Some(op) = manager.rx.recv().await {
            manager.handle_op(Some(op));
        }

        // Verify subscription
        assert!(manager
            .task_subscriptions
            .get(&task_uuid)
            .unwrap()
            .contains(&session_id));
    }
}
