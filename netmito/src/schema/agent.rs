//! Wire types for agents: the user routes under `/agents`, the agent routes
//! under `/agents`, and the notifications on `/ws/agents`.

use std::collections::HashSet;

use sea_orm::FromQueryResult;
use serde::{Deserialize, Serialize};
use speedy::{Readable, Writable};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::entity::state::AgentState;

/// Request to register an agent.
///
/// One machine has one agent row for all its life, because `machine_code` is
/// unique. If the machine is already known, registration uses its agent row
/// again: it replaces the tags, labels, groups and metadata, and it issues a
/// new token.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterAgentReq {
    /// Tags that a suite must match to run on this agent (e.g. `["gpu", "linux"]`).
    #[serde(default)]
    pub tags: HashSet<String>,
    /// Labels for queries and filters (e.g. `["datacenter:us-west"]`).
    #[serde(default)]
    pub labels: HashSet<String>,
    /// The group that gets the Admin role on the agent. Only an Admin of this
    /// group can shut the agent down, and the caller must be one. The default
    /// is the personal group of the caller.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admin_group: Option<String>,
    /// Groups that get the Write role on the agent.
    #[serde(default)]
    pub groups: HashSet<String>,
    /// The lifetime of the agent token. `None` means the token never expires.
    #[serde(default, with = "humantime_serde")]
    pub lifetime: Option<std::time::Duration>,
    /// A stable identifier of the machine that runs the agent.
    pub machine_code: String,
    /// Static data about the agent process, kept on the machine row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<AgentMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterAgentResp {
    pub agent_uuid: Uuid,
    pub token: String,
    /// The last notification id issued to this agent. The agent counts its
    /// notifications from this value.
    pub notification_counter: u64,
    /// True if the machine was already known and its agent row is used again.
    pub reused: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMetadata {
    /// Agent binary version (e.g. "0.6.8").
    pub version: String,
    /// Long version string, with build data.
    pub long_version: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentsQueryReq {
    pub group_name: Option<String>,
    pub tags: Option<HashSet<String>>,
    pub labels: Option<HashSet<String>>,
    pub states: Option<HashSet<AgentState>>,
    pub creator_username: Option<String>,
    pub limit: Option<u64>,
    pub offset: Option<u64>,
    pub count: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromQueryResult)]
pub struct AgentInfo {
    pub uuid: Uuid,
    pub creator_username: String,
    pub tags: Vec<String>,
    pub labels: Vec<String>,
    pub state: AgentState,
    pub last_heartbeat: OffsetDateTime,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assigned_suite_uuid: Option<Uuid>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub machine_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentsQueryResp {
    pub count: u64,
    pub agents: Vec<AgentInfo>,
    pub group_name: String,
}

/// Query parameters of `DELETE /agents/{uuid}`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentShutdownReq {
    #[serde(default)]
    pub op: AgentShutdownOp,
}

/// How to shut an agent down. The agent row stays in both cases.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
pub enum AgentShutdownOp {
    /// Tell the agent to stop. The agent finishes its current work and then
    /// marks itself `Offline`.
    #[default]
    #[serde(alias = "graceful")]
    Graceful,
    /// Mark the agent `Offline` now, then tell it to stop.
    #[serde(alias = "force")]
    Force,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentHeartbeatReq {
    /// The state the agent reports for itself.
    pub state: AgentState,
    /// The highest notification id the agent has processed.
    #[serde(default)]
    pub last_notification_id: u64,
}

/// The notifications that the agent has not acknowledged yet.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentHeartbeatResp {
    pub notifications: Vec<WsNotificationEvent>,
}

/// A notification with its sequence id. The id tells an agent which
/// notifications it has already processed.
///
/// `speedy` encodes it on the WebSocket. The heartbeat response sends the same
/// events as JSON, thus it also needs serde.
#[derive(Debug, Clone, Serialize, Deserialize, Readable, Writable)]
pub struct WsNotificationEvent {
    /// Per-agent sequence id. It increases by one for each notification.
    pub id: u64,
    pub event: AgentNotification,
}

/// A message from the coordinator that tells an agent to do something.
///
/// Add new variants only at the end. `speedy` encodes the variant index, thus
/// a variant inserted in the middle changes the meaning of the variants after it.
#[derive(Debug, Clone, Serialize, Deserialize, Readable, Writable)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentNotification {
    /// Shut down. With `graceful`, finish the current work first.
    Shutdown { graceful: bool },
    /// The coordinator restarted and its notification ids start again from
    /// `counter`. `boot_id` identifies the new coordinator process.
    CounterSync { counter: u64, boot_id: Uuid },
}

/// A message from an agent to the coordinator on the WebSocket.
#[derive(Debug, Clone, Serialize, Deserialize, Readable, Writable)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentWsMessage {
    /// The agent processed all notifications up to and including this id.
    Ack { notification_id: u64 },
    /// The reply to a ping, with the agent's clock in Unix seconds.
    Pong { client_time: i64 },
}
