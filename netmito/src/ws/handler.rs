//! WebSocket handler for agent connections

use axum::{
    extract::{
        ws::{Message, WebSocket},
        State, WebSocketUpgrade,
    },
    http::StatusCode,
    response::IntoResponse,
    Extension,
};
use futures::{SinkExt, StreamExt};
use speedy::Readable;
use uuid::Uuid;

use crate::{
    config::InfraPool, schema::AgentWsMessage, service::auth::AuthAgent,
    ws::connection::AgentWsRouter,
};

/// WebSocket upgrade handler for agent connections
///
/// Authentication: Bearer token (agent JWT) - handled by middleware
pub async fn websocket_handler(
    ws: WebSocketUpgrade,
    State(pool): State<InfraPool>,
    Extension(agent): Extension<AuthAgent>,
) -> Result<impl IntoResponse, StatusCode> {
    let agent_uuid = agent.uuid;

    tracing::debug!(
        agent_id = %agent.id,
        agent_uuid = %agent_uuid,
        "Agent WebSocket upgrade request accepted"
    );

    // Upgrade connection
    Ok(ws.on_upgrade(move |socket| handle_agent_socket(socket, agent_uuid, pool)))
}

/// Handle an established WebSocket connection
async fn handle_agent_socket(socket: WebSocket, agent_uuid: Uuid, pool: InfraPool) {
    let (mut sender, mut receiver) = socket.split();

    let ws_tx = pool.ws_router_tx.clone();
    // Create a channel for this connection
    // XXX: we currently are using a bounded channel to avoid unbounded memory growth but if this
    // becomes the bottleneck we might need to revisit this decision.
    let (tx, rx) = crossfire::mpsc::bounded_async(128);

    // Register connection
    AgentWsRouter::register(&ws_tx, agent_uuid, tx);

    // Periodic keepalive: send a WS Ping frame at the configured interval so that
    // intermediate proxies/load-balancers do not drop idle connections.
    let mut ping_interval = tokio::time::interval(pool.ws_ping_interval);
    ping_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Consume the first tick which fires immediately so we don't ping right on connect.
    ping_interval.tick().await;

    // Handle incoming messages (from socket) and outgoing messages (from router)
    loop {
        tokio::select! {
            // Receive from WebSocket
            msg_result = receiver.next() => {
                match msg_result {
                    Some(Ok(msg)) => {
                        if !handle_ws_message(&agent_uuid, &pool, msg).await {
                            // Connection closed by agent or some error occurred
                            break;
                        }
                    }
                    Some(Err(e)) => {
                        tracing::warn!(
                            agent_uuid = %agent_uuid,
                            error = %e,
                            "WebSocket error"
                        );
                        break;
                    }
                    None => {
                        // Stream finished
                        break;
                    }
                }
            }

            // Receive from Router (application outgoing messages)
            msg_result = rx.recv() => {
                match msg_result {
                    Ok(msg) => {
                        if let Err(e) = sender.send(msg).await {
                            tracing::warn!(
                                agent_uuid = %agent_uuid,
                                error = %e,
                                "Failed to send WebSocket message"
                            );
                            break;
                        }
                    }
                    Err(_) => {
                        // Channel closed (router shutdown or unregistered)
                        break;
                    }
                }
            }

            // Send keepalive ping
            _ = ping_interval.tick() => {
                tracing::trace!(agent_uuid = %agent_uuid, "Sending keepalive ping");
                if let Err(e) = sender.send(Message::Ping(vec![].into())).await {
                    tracing::warn!(
                        agent_uuid = %agent_uuid,
                        error = %e,
                        "Failed to send keepalive ping, closing connection"
                    );
                    break;
                }
            }
        }
    }

    // Cleanup on disconnect
    AgentWsRouter::unregister(&ws_tx, agent_uuid);
}

async fn handle_ws_message(agent_uuid: &Uuid, pool: &InfraPool, msg: Message) -> bool {
    match msg {
        Message::Binary(bytes) => {
            // Parse and handle agent message
            match crate::schema::AgentWsMessage::read_from_buffer(&bytes) {
                Ok(msg) => {
                    handle_agent_message(&agent_uuid, msg, &pool).await;
                }
                Err(e) => {
                    tracing::debug!(
                        agent_uuid = %agent_uuid,
                        error = %e,
                        "Failed to parse agent WebSocket message"
                    );
                }
            }
        }
        Message::Ping(_) => {
            // axum handles ping/pong, just trace
            tracing::trace!(agent_uuid = %agent_uuid, "Received ping");
        }
        Message::Pong(_) => {
            tracing::trace!(agent_uuid = %agent_uuid, "Received pong");
        }
        Message::Close(frame) => {
            tracing::debug!(
                agent_uuid = %agent_uuid,
                frame = ?frame,
                "Agent closed WebSocket connection"
            );
            return false;
        }
        Message::Text(text) => {
            tracing::debug!(
                agent_uuid = %agent_uuid,
                text = %text,
                "Received unexpected text WebSocket message"
            );
        }
    }
    true
}

/// Handle a message from an agent
/// TODO: add more logic
async fn handle_agent_message(agent_uuid: &Uuid, message: AgentWsMessage, pool: &InfraPool) {
    match message {
        AgentWsMessage::Ack { notification_id } => {
            tracing::trace!(
                agent_uuid = %agent_uuid,
                notification_id = notification_id,
                "Received notification acknowledgment"
            );
            AgentWsRouter::ack(&pool.ws_router_tx, *agent_uuid, notification_id);
        }
        AgentWsMessage::Pong { client_time } => {
            let server_time = time::OffsetDateTime::now_utc().unix_timestamp();
            let latency = server_time - client_time;
            tracing::trace!(
                agent_uuid = %agent_uuid,
                latency_ms = latency * 1000,
                "Received pong"
            );
        }
    }
}
