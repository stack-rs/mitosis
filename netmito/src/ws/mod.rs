//! Notifications from the coordinator to agents on a WebSocket.
//!
//! The WebSocket only makes delivery faster. The router keeps each notification in a per-agent
//! buffer, and the next heartbeat response also returns it. Thus an agent without a connection
//! still gets all notifications. See [`connection::AgentWsRouter`] for the buffer and the
//! acknowledgements.

pub mod connection;
pub mod handler;

pub use connection::{AgentWsRouter, RouterOp};
pub use handler::websocket_handler;
