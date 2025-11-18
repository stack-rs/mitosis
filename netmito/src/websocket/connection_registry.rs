use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket};
use futures::{stream::SplitSink, SinkExt};
use tokio::sync::RwLock;
use uuid::Uuid;

/// Type alias for the sender half of a WebSocket connection
pub type WsSender = Arc<RwLock<SplitSink<WebSocket, Message>>>;

/// Registry to track active WebSocket connections from node managers
#[derive(Clone)]
pub struct ConnectionRegistry {
    connections: Arc<RwLock<HashMap<Uuid, WsSender>>>,
}

impl ConnectionRegistry {
    /// Create a new empty connection registry
    pub fn new() -> Self {
        Self {
            connections: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register a new manager WebSocket connection
    pub async fn register(&self, manager_uuid: Uuid, sender: WsSender) {
        self.connections.write().await.insert(manager_uuid, sender);
        tracing::info!(manager_uuid = %manager_uuid, "Manager WebSocket connected");
    }

    /// Unregister a manager WebSocket connection
    pub async fn unregister(&self, manager_uuid: &Uuid) {
        self.connections.write().await.remove(manager_uuid);
        tracing::info!(manager_uuid = %manager_uuid, "Manager WebSocket disconnected");
    }

    /// Get a sender for a specific manager
    pub async fn get(&self, manager_uuid: &Uuid) -> Option<WsSender> {
        self.connections.read().await.get(manager_uuid).cloned()
    }

    /// Get the count of currently connected managers
    pub async fn count(&self) -> usize {
        self.connections.read().await.len()
    }

    /// Send a message to a specific manager
    pub async fn send_to_manager(
        &self,
        manager_uuid: &Uuid,
        message: &crate::websocket::CoordinatorMessage,
    ) -> crate::error::Result<()> {
        if let Some(sender) = self.get(manager_uuid).await {
            let msg_json = serde_json::to_string(message)?;
            sender
                .write()
                .await
                .send(Message::Text(msg_json.into()))
                .await
                .map_err(|e| crate::error::Error::Custom(format!("Failed to send message: {}", e)))?;
        }
        Ok(())
    }

    /// Broadcast a message to all connected managers
    pub async fn broadcast(
        &self,
        message: &crate::websocket::CoordinatorMessage,
    ) -> crate::error::Result<()> {
        let msg_json = serde_json::to_string(message)?;
        let connections = self.connections.read().await;

        for (uuid, sender) in connections.iter() {
            if let Err(e) = sender
                .write()
                .await
                .send(Message::Text(msg_json.clone().into()))
                .await
            {
                tracing::error!(manager_uuid = %uuid, error = %e, "Failed to broadcast message");
            }
        }

        Ok(())
    }
}

impl Default for ConnectionRegistry {
    fn default() -> Self {
        Self::new()
    }
}
