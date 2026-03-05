//! WebSocket module for Agent notifications
//!
//! This module provides a WebSocket-based notification system for pushing
//! real-time updates to Agents. The WebSocket connection is optional
//! and serves as an optimization - all operations can still be performed
//! via HTTP polling through heartbeats.
//!
//! Architecture:
//! - Coordinator pushes notifications to connected agents
//! - Agents receive notifications and act via HTTP APIs
//! - If WebSocket is unavailable, agents use heartbeat polling

pub mod connection;
pub mod handler;

pub use connection::{AgentWsRouter, RouterOp};
pub use handler::websocket_handler;
