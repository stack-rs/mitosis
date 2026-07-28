//! `GET /ws/agents` — the agent's notification socket.
//!
//! Authentication is the agent JWT, checked by `agent_auth_middleware` before
//! the upgrade. Once upgraded, the task bridges two directions: router → socket
//! (notifications) and socket → router (acknowledgements).

use axum::{
    extract::{
        ws::{Message, WebSocket},
        State, WebSocketUpgrade,
    },
    response::IntoResponse,
    Extension,
};
use futures::{SinkExt, StreamExt};
use speedy::Readable;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::{
    config::InfraPool, schema::AgentWsMessage, service::auth::AuthAgent,
    ws::connection::AgentWsRouter,
};

/// How many notifications may sit in a single connection's outbound queue
/// before the router starts skipping pushes (the events stay buffered and are
/// delivered on the next heartbeat instead).
const OUTBOUND_QUEUE_LEN: usize = 128;

pub async fn websocket_handler(
    ws: WebSocketUpgrade,
    State(pool): State<InfraPool>,
    Extension(agent): Extension<AuthAgent>,
) -> impl IntoResponse {
    let agent_uuid = agent.uuid;
    tracing::debug!(agent_uuid = %agent_uuid, "Agent WebSocket upgrade accepted");
    ws.on_upgrade(move |socket| handle_agent_socket(socket, agent_uuid, pool))
}

async fn handle_agent_socket(socket: WebSocket, agent_uuid: Uuid, pool: InfraPool) {
    let (mut sender, mut receiver) = socket.split();
    let (tx, mut rx) = mpsc::channel::<Message>(OUTBOUND_QUEUE_LEN);

    AgentWsRouter::register(&pool.ws_router_tx, agent_uuid, tx);

    // Keepalive so idle connections survive intermediate proxies.
    let mut ping_interval = tokio::time::interval(pool.ws_ping_interval);
    ping_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    ping_interval.tick().await; // the first tick fires immediately

    loop {
        tokio::select! {
            incoming = receiver.next() => match incoming {
                Some(Ok(msg)) => {
                    if !handle_ws_message(agent_uuid, &pool, msg) {
                        break;
                    }
                }
                Some(Err(e)) => {
                    tracing::debug!(agent_uuid = %agent_uuid, "Agent WebSocket error: {e}");
                    break;
                }
                None => break,
            },

            outgoing = rx.recv() => match outgoing {
                Some(msg) => {
                    if let Err(e) = sender.send(msg).await {
                        tracing::debug!(agent_uuid = %agent_uuid, "Failed to push to agent: {e}");
                        break;
                    }
                }
                None => break,
            },

            _ = ping_interval.tick() => {
                if let Err(e) = sender.send(Message::Ping(Vec::new().into())).await {
                    tracing::debug!(agent_uuid = %agent_uuid, "Keepalive ping failed: {e}");
                    break;
                }
            }
        }
    }

    AgentWsRouter::unregister(&pool.ws_router_tx, agent_uuid);
    tracing::debug!(agent_uuid = %agent_uuid, "Agent WebSocket closed");
}

/// Returns false when the connection should be torn down.
fn handle_ws_message(agent_uuid: Uuid, pool: &InfraPool, msg: Message) -> bool {
    match msg {
        Message::Binary(bytes) => match AgentWsMessage::read_from_buffer(&bytes) {
            Ok(msg) => handle_agent_message(agent_uuid, msg, pool),
            Err(e) => {
                tracing::debug!(agent_uuid = %agent_uuid, "Unparseable agent message: {e}");
            }
        },
        Message::Close(frame) => {
            tracing::debug!(agent_uuid = %agent_uuid, ?frame, "Agent closed the WebSocket");
            return false;
        }
        // axum answers pings itself; frames are `speedy` binary, never text.
        Message::Ping(_) | Message::Pong(_) => {}
        Message::Text(text) => {
            tracing::debug!(agent_uuid = %agent_uuid, %text, "Ignoring an unexpected text frame");
        }
    }
    true
}

fn handle_agent_message(agent_uuid: Uuid, message: AgentWsMessage, pool: &InfraPool) {
    match message {
        AgentWsMessage::Ack { notification_id } => {
            AgentWsRouter::ack(&pool.ws_router_tx, agent_uuid, notification_id);
        }
        AgentWsMessage::Pong { client_time } => {
            let latency = time::OffsetDateTime::now_utc().unix_timestamp() - client_time;
            tracing::trace!(agent_uuid = %agent_uuid, latency_secs = latency, "Agent pong");
        }
    }
}
