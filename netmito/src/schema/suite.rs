use std::collections::HashSet;

use sea_orm::FromQueryResult;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use super::exec::ExecHooks;

/// Request to create a new task suite
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateTaskSuiteReq {
    /// Optional human-readable name (non-unique)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Optional description
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Group that owns this suite (require user have permissions to that group)
    pub group_name: String,
    /// Tags for agent matching (e.g., ["wireless", "linux", "cuda:11"])
    #[serde(default)]
    pub tags: HashSet<String>,
    /// Labels for querying/filtering (e.g., ["project:cauldron", "phase:bayesian-optimization"])
    #[serde(default)]
    pub labels: HashSet<String>,
    /// Suite scheduling priority (higher = more important)
    #[serde(default)]
    pub priority: i32,
    /// Worker allocation plan
    pub worker_schedule: WorkerSchedulePlan,
    /// Execution hooks for environment setup/teardown
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec_hooks: Option<ExecHooks>,
}

/// Response after creating a task suite
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateTaskSuiteResp {
    /// Unique UUID for this suite
    pub uuid: Uuid,
}

/// Worker scheduling policy for the suite
/// This enum allows for future extension with different scheduling strategies
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "PascalCase")]
pub enum WorkerSchedulePlan {
    /// Fixed number of workers with optional CPU binding
    /// This is the basic scheduling policy where a fixed number of workers
    /// are spawned to process tasks from the suite
    // TODO: should update it to our final design
    FixedWorkers {
        /// Number of workers to spawn (1-256)
        worker_count: u32,
        /// Optional CPU core binding strategy
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cpu_binding: Option<CpuBinding>,
        /// How many tasks to prefetch locally per worker (default: 16)
        #[serde(default = "default_prefetch_count")]
        task_prefetch_count: u32,
    },
    // Future extensions:
    // AutoScale { min_workers, max_workers, scale_up_threshold, scale_down_threshold, ... }
    // LoadBalanced { target_utilization, ... }
    // Priority { high_priority_workers, low_priority_workers, ... }
}

fn default_prefetch_count() -> u32 {
    16
}

/// CPU core binding configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CpuBinding {
    /// List of CPU core IDs to bind to
    pub cores: Vec<usize>,
    /// Binding strategy
    pub strategy: CpuBindingStrategy,
}

/// CPU binding strategies
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum CpuBindingStrategy {
    /// Distribute workers across cores in round-robin fashion
    RoundRobin,
    /// Each worker gets exclusive access to dedicated core(s)
    Exclusive,
    /// All workers share all specified cores
    Shared,
}

/// Query parameters for listing suites
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSuitesQueryReq {
    pub name: Option<String>,
    pub description: Option<String>,
    pub creator_usernames: Option<HashSet<String>>,
    pub group_name: Option<String>,
    pub tags: Option<HashSet<String>>,
    pub labels: Option<HashSet<String>>,
    pub states: Option<HashSet<crate::entity::state::TaskSuiteState>>,
    pub priority: Option<String>,
    pub limit: Option<u64>,
    pub offset: Option<u64>,
    pub count: bool,
}

/// Response for suite query
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSuitesQueryResp {
    pub count: u64,
    pub suites: Vec<TaskSuiteInfo>,
    pub group_name: String,
}

/// Information about a task suite
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedTaskSuiteInfo {
    pub uuid: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub group_name: String,
    pub creator_username: String,
    pub tags: Vec<String>,
    pub labels: Vec<String>,
    pub priority: i32,
    pub worker_schedule: WorkerSchedulePlan,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exec_hooks: Option<ExecHooks>,
    pub state: crate::entity::state::TaskSuiteState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_task_submitted_at: Option<OffsetDateTime>,
    pub total_tasks: i32,
    pub pending_tasks: i32,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSuiteQueryResp {
    pub info: ParsedTaskSuiteInfo,
    pub assigned_agents: Vec<Uuid>,
}

/// Information about a task suite
#[derive(Debug, Clone, Serialize, Deserialize, FromQueryResult)]
pub struct TaskSuiteInfo {
    pub uuid: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub group_name: String,
    pub creator_username: String,
    pub tags: Vec<String>,
    pub labels: Vec<String>,
    pub priority: i32,
    pub worker_schedule: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exec_hooks: Option<serde_json::Value>,
    pub state: crate::entity::state::TaskSuiteState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_task_submitted_at: Option<OffsetDateTime>,
    pub total_tasks: i32,
    pub pending_tasks: i32,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<OffsetDateTime>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CancelTaskSuiteParam {
    pub op: Option<CancelTaskSuiteOp>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub enum CancelTaskSuiteOp {
    #[default]
    #[serde(alias = "graceful")]
    Graceful,
    #[serde(alias = "force")]
    Force,
}

/// Response after canceling a suite
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelSuiteResp {
    /// Number of tasks that were cancelled
    pub cancelled_task_count: u64,
}

/// Query parameters for listing suite tasks
#[derive(serde::Deserialize)]
pub struct TaskSuiteTasksQueryParams {
    pub limit: Option<u64>,
    pub offset: Option<u64>,
}

/// Request to refresh tag-matched agents for a suite
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefreshSuiteAgentsResp {
    /// Agents that were added during this refresh
    pub added_agents: Vec<AgentAssignmentInfo>,
    /// Agents that were removed during this refresh (no longer match tags)
    pub removed_agents: Vec<Uuid>,
    /// Total number of agents now assigned to the suite
    pub total_assigned: u64,
}

/// Information about an agent assignment
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentAssignmentInfo {
    pub agent_uuid: Uuid,
    pub matched_tags: Vec<String>,
    pub selection_type: crate::entity::state::SelectionType,
}

/// Request to manually add agents to a suite
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddSuiteAgentsReq {
    pub agent_uuids: Vec<Uuid>,
}

/// Response after adding agents
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddSuiteAgentsResp {
    /// Agents that were successfully added
    pub added_agents: Vec<Uuid>,
    /// Agents that could not be added (permission denied or not found)
    pub rejected_agents: Vec<Uuid>,
    /// Reason for rejection (if any)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Request to remove agents from a suite
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoveSuiteAgentsReq {
    pub agent_uuids: Vec<Uuid>,
}

/// Response after removing agents
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoveSuiteAgentsResp {
    pub removed_count: u64,
}
