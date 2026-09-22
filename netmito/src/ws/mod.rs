//! Coordinator → agent push notifications over WebSocket.
//!
//! The WS layer is a pure latency optimization: every notification is also
//! buffered per agent and handed back on the next heartbeat, so an agent that
//! never connects (or whose connection drops) still makes progress. See
//! [`connection::AgentWsRouter`] for the buffer/ack model.
//!
//! Frames are `speedy`-encoded binary in both directions, matching dev. The
//! same notification types also travel as JSON in the heartbeat response, so
//! they carry both derives.

pub mod connection;
pub mod handler;

pub use connection::{AgentWsRouter, RouterOp};
pub use handler::websocket_handler;
