use std::collections::HashSet;

use sea_orm::FromQueryResult;
use serde::{Deserialize, Serialize};
use speedy::{Readable, Writable};
use time::OffsetDateTime;
use uuid::Uuid;

use super::exec::ExecHooks;
use super::suite::WorkerSchedulePlan;
use super::task::{ReportTaskOp, WorkerTaskResp};

/// Request to register a new agent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterAgentReq {
    /// Tags for suite matching (e.g., ["gpu", "linux", "cuda:11.8"])
    #[serde(default)]
    pub tags: HashSet<String>,
    /// Labels for querying/filtering (e.g., ["datacenter:us-west", "machine_id:server-42"])
    #[serde(default)]
    pub labels: HashSet<String>,
    /// Groups to associate with (agent will get Write role from these groups)
    #[serde(default)]
    pub groups: HashSet<String>,
    /// Optional token lifetime (default: forever)
    #[serde(default, with = "humantime_serde")]
    pub lifetime: Option<std::time::Duration>,
}

/// Response after registering an agent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterAgentResp {
    pub agent_uuid: Uuid,
    pub token: String,
    /// Current notification counter for this manager (start of sequence)
    pub notification_counter: u64,
}

/// Request for agent heartbeat
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentHeartbeatReq {
    /// Current agent state
    pub state: crate::entity::state::AgentState,
    /// Currently assigned suite UUID (if any)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assigned_suite_uuid: Option<Uuid>,
    /// The last notification ID the agent has processed
    #[serde(default)]
    pub last_notification_id: u64,
    /// Optional metrics
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics: Option<AgentMetrics>,
}

/// Metrics reported by agent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMetrics {
    pub active_workers: u32,
    pub tasks_completed: u64,
    pub tasks_failed: u64,
}

/// Query parameters for listing agents
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentsQueryReq {
    pub group_name: Option<String>,
    pub tags: Option<HashSet<String>>,
    pub labels: Option<HashSet<String>>,
    pub states: Option<HashSet<crate::entity::state::AgentState>>,
    pub creator_username: Option<String>,
    pub limit: Option<u64>,
    pub offset: Option<u64>,
    pub count: bool,
}

/// Information about an agent
#[derive(Debug, Clone, Serialize, Deserialize, FromQueryResult)]
pub struct AgentInfo {
    pub uuid: Uuid,
    pub creator_username: String,
    pub tags: Vec<String>,
    pub labels: Vec<String>,
    pub state: crate::entity::state::AgentState,
    pub last_heartbeat: OffsetDateTime,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assigned_suite_uuid: Option<Uuid>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// Response for agent query
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentsQueryResp {
    pub count: u64,
    pub agents: Vec<AgentInfo>,
    pub group_name: String,
}

/// Request to shutdown a agent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentShutdownReq {
    #[serde(default)]
    pub op: AgentShutdownOp,
}

/// Shutdown operation type
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub enum AgentShutdownOp {
    #[default]
    #[serde(alias = "graceful")]
    Graceful,
    #[serde(alias = "force")]
    Force,
}

/// Response for agent heartbeat containing pending actions
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentHeartbeatResp {
    /// Missed notifications since last_notification_id
    pub notifications: Vec<WsNotificationEvent>,
}

/// Request to fetch an available suite for execution
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FetchSuiteReq {
    /// If specified, only consider this specific suite
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suite_uuid: Option<Uuid>,
}

/// Response containing suite details for execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchSuiteResp {
    /// Suite to execute (None if no suitable suite available)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suite: Option<TaskSuiteSpec>,
}

/// Full specification of a task suite for agent execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSuiteSpec {
    pub uuid: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub group_id: i64,
    pub group_name: String,
    pub tags: Vec<String>,
    pub labels: Vec<String>,
    pub priority: i32,
    pub worker_schedule: WorkerSchedulePlan,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exec_hooks: Option<ExecHooks>,
    pub state: crate::entity::state::TaskSuiteState,
    pub total_tasks: i32,
    pub pending_tasks: i32,
}

/// Request to accept a suite assignment (claim it for execution)
/// POST /managers/suite/accept
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcceptSuiteReq {
    pub suite_uuid: Uuid,
}

/// Response after accepting a suite
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcceptSuiteResp {
    /// Whether the suite was successfully accepted
    pub accepted: bool,
    /// Reason if not accepted (e.g., already taken by another manager)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Request to report suite execution started
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartSuiteReq {
    pub suite_uuid: Uuid,
}

/// Request to report suite completion
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompleteSuiteReq {
    pub suite_uuid: Uuid,
    /// Total tasks completed during this execution
    pub tasks_completed: u64,
    /// Total tasks failed during this execution
    pub tasks_failed: u64,
    /// Reason for completion (normal, cancelled, error)
    #[serde(default)]
    pub completion_reason: SuiteCompletionReason,
}

/// Reason for suite completion
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub enum SuiteCompletionReason {
    /// All tasks completed normally
    #[default]
    Normal,
    /// Suite was preempted by higher priority suite
    Preempted,
    /// Suite was cancelled by user
    Cancelled,
    /// Environment preparation failed
    PrepFailed,
    /// Environment cleanup failed (but tasks completed)
    CleanupFailed,
    /// Other error
    Error(String),
}

/// Response after completing a suite
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompleteSuiteResp {
    /// Whether there's another suite available immediately
    pub next_suite_available: bool,
}

/// Request to fetch tasks for execution (for managed workers)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FetchTasksReq {
    /// Suite UUID to fetch tasks from
    pub suite_uuid: Uuid,
    /// Maximum number of tasks to fetch (for prefetching)
    #[serde(default = "default_fetch_count")]
    pub max_count: u32,
}

fn default_fetch_count() -> u32 {
    1
}

/// Response containing tasks for execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchTasksResp {
    /// Tasks to execute (empty if no tasks available)
    pub tasks: Vec<WorkerTaskResp>,
}

/// Request to report task execution result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportAgentTaskReq {
    /// Task ID
    pub task_id: i64,
    /// Task UUID
    pub task_uuid: Uuid,
    /// Operation to perform
    pub op: ReportTaskOp,
}

/// Request to report task execution failure (for retry tracking)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportTaskFailureReq {
    pub task_uuid: Uuid,
    pub suite_uuid: Uuid,
    pub failure_count: u32,
    pub error_message: String,
    /// Which local worker failed (for diagnostics)
    pub worker_local_id: u32,
}

/// Request to abort a task (return it to the queue)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbortTaskReq {
    pub task_uuid: Uuid,
    pub reason: String,
}

// ============================================================================
// WebSocket Notification Types (Coordinator → Agent)
// ============================================================================

/// Packet containing a sequenced notification event
#[derive(Debug, Clone, Serialize, Deserialize, Readable, Writable)]
pub struct WsNotificationEvent {
    /// Monotonically increasing sequence ID
    pub id: u64,
    /// The notification event
    pub event: AgentNotification,
}

/// Notification sent from Coordinator to Agent via WebSocket
/// These are lightweight push notifications that prompt the agent to
/// take action via HTTP APIs
/// TODO: change definition
#[derive(Debug, Clone, Serialize, Deserialize, Readable, Writable)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentNotification {
    /// A new suite is available for this agent to execute
    SuiteAvailable {
        /// Optional hint about which suite (agent still needs to fetch)
        suite_uuid: Option<Uuid>,
        priority: i32,
    },

    /// Agent should preempt current suite for a higher priority one
    /// Agent should gracefully stop current work and fetch new suite
    PreemptSuite {
        /// The new suite to execute
        new_suite_uuid: Uuid,
        new_priority: i32,
        /// Current suite that should be preempted
        current_suite_uuid: Uuid,
    },

    /// Current suite has been cancelled
    /// Agent should stop execution and clean up
    SuiteCancelled { suite_uuid: Uuid, reason: String },

    /// Specific tasks have been cancelled
    /// Agent should stop executing these tasks if in progress
    TasksCancelled { task_uuids: Vec<Uuid> },

    /// Agent should shut down
    Shutdown { graceful: bool },

    /// Heartbeat acknowledgment (sent periodically to keep connection alive)
    Ping { server_time: i64 },

    /// Sync the notification counter (e.g., after wrap-around or connection)
    CounterSync { counter: u64, boot_id: Uuid },
}

/// Message from Agent to Coordinator via WebSocket (if needed)
/// Currently WebSocket is primarily for server-push notifications
#[derive(Debug, Clone, Serialize, Deserialize, Readable, Writable)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentWsMessage {
    /// Acknowledge receipt of a notification
    Ack { notification_id: u64 },
    /// Pong response to Ping
    Pong { client_time: i64 },
}
