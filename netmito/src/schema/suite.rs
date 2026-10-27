use std::collections::{HashMap, HashSet};

use sea_orm::FromQueryResult;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::entity::state::TaskSuiteState;

use super::exec::{ExecHooks, ExecSpec};

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

/// A change to a field that has a nullable column. The request puts the field in an `Option`,
/// and an absent field does not change.
/// A present field has one of these two values: set a new value, or set the column to null.
///
/// A field that has a `NOT NULL` column uses a plain `Option`. This column cannot be null, so its
/// 'cleared' value is a value of its type, for example an empty array.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", content = "value", rename_all = "snake_case")]
pub enum UpdateOp<T> {
    SetTo(T),
    SetNull,
}

impl<T> UpdateOp<T> {
    /// Returns the value to store. `SetNull` returns `None`.
    pub fn into_value(self) -> Option<T> {
        match self {
            Self::SetTo(value) => Some(value),
            Self::SetNull => None,
        }
    }
}

/// The changes to the `exec_hooks` of a suite, one for each hook. You can set a hook, clear a
/// hook, or leave a hook out to keep it. The request does not replace the full object, because
/// that would remove each hook that the request does not mention. If you clear all three hooks,
/// the suite stores NULL, the same as a suite that you create without hooks.
///
/// The database merges the changes into the current value of the column. Because of this, two
/// changes at the same time cannot overwrite each other.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExecHooksUpdate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provision: Option<UpdateOp<ExecSpec>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleanup: Option<UpdateOp<ExecSpec>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<UpdateOp<ExecSpec>>,
}

impl ExecHooksUpdate {
    pub fn is_empty(&self) -> bool {
        self.provision.is_none() && self.cleanup.is_none() && self.background.is_none()
    }
}

/// A request to change the properties of a suite. An absent field means no change.
/// The server rejects an empty request.
///
/// The change applies to the jobs that start after it, because a job copies the worker schedule
/// and the hooks when it starts.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChangeTaskSuiteReq {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<UpdateOp<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<UpdateOp<String>>,
    /// The tags that an agent must have to run the suite. An empty set matches all agents that
    /// the group of the suite can write to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<HashSet<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub labels: Option<HashSet<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<i32>,
    /// Replaces the full plan. A partial change cannot change the plan to a different scheduling policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_schedule: Option<WorkerSchedulePlan>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec_hooks: Option<ExecHooksUpdate>,
}

impl ChangeTaskSuiteReq {
    pub fn is_empty(&self) -> bool {
        self.name.is_none()
            && self.description.is_none()
            && self.tags.is_none()
            && self.labels.is_none()
            && self.priority.is_none()
            && self.worker_schedule.is_none()
            && self.exec_hooks.as_ref().map_or(true, |h| h.is_empty())
    }

    /// Returns true if the change can change the result of the scheduler: which agents can run the
    /// suite, or the position of the suite in the pick order.
    pub fn affects_scheduling(&self) -> bool {
        self.tags.is_some() || self.priority.is_some()
    }
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
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CpuBinding {
    /// List of CPU core IDs to bind to, an empty vectors means using all cores
    pub cores: Vec<usize>,
    /// Binding strategy
    pub strategy: CpuBindingStrategy,
}

/// CPU binding strategies
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
pub enum CpuBindingStrategy {
    /// Distribute workers across cores in round-robin fashion
    RoundRobin,
    /// Each worker gets exclusive access to dedicated core(s)
    Exclusive,
    /// All workers share all specified cores
    #[default]
    Shared,
}

/// Filter for querying task suites
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSuitesQueryReq {
    pub name: Option<String>,
    pub description: Option<String>,
    pub creator_usernames: Option<HashSet<String>>,
    pub group_name: Option<String>,
    pub tags: Option<HashSet<String>>,
    pub labels: Option<HashSet<String>>,
    pub states: Option<HashSet<TaskSuiteState>>,
    pub priority: Option<String>,
    pub limit: Option<u64>,
    pub offset: Option<u64>,
    pub count: bool,
}

/// Suite information with raw JSON worker_schedule/exec_hooks, straight from a DB query
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
    pub state: TaskSuiteState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_task_submitted_at: Option<OffsetDateTime>,
    pub total_tasks: i32,
    pub incomplete_tasks: i32,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<OffsetDateTime>,
}

/// Response for suite query
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSuitesQueryResp {
    pub count: u64,
    pub suites: Vec<TaskSuiteInfo>,
    pub group_name: String,
}

/// Suite information with parsed (typed) worker_schedule/exec_hooks, for detail views
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
    pub state: TaskSuiteState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_task_submitted_at: Option<OffsetDateTime>,
    pub total_tasks: i32,
    pub incomplete_tasks: i32,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<OffsetDateTime>,
}

/// Detailed suite response: the suite plus the UUIDs of its assigned agents
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSuiteQueryResp {
    pub info: ParsedTaskSuiteInfo,
    pub eligible_agents: Vec<Uuid>,
}

/// Query parameter for `DELETE /suites/{uuid}` selecting the cancellation mode
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CancelTaskSuiteParam {
    pub op: Option<CancelTaskSuiteOp>,
}

/// Cancellation mode for a suite
#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub enum CancelTaskSuiteOp {
    #[default]
    #[serde(alias = "graceful")]
    Graceful,
    #[serde(alias = "force")]
    Force,
}

/// One entry's desired outcome in a batch agent-selection request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuiteAgentOverrideAction {
    /// Pin the agent to the suite even if it does not tag-match (manual include).
    Include,
    /// Block the agent from the suite even if it tag-matches (manual exclude).
    Exclude,
    /// Clear any manual override, falling back to tag-matching.
    Clear,
}

impl SuiteAgentOverrideAction {
    /// The persisted override type this action maps to, or `None` for `Clear`
    /// (which clears the override).
    pub fn override_type(self) -> Option<crate::entity::task_suite_agent::SuiteAgentOverrideType> {
        match self {
            Self::Include => {
                Some(crate::entity::task_suite_agent::SuiteAgentOverrideType::UserIncluded)
            }
            Self::Exclude => {
                Some(crate::entity::task_suite_agent::SuiteAgentOverrideType::UserExcluded)
            }
            Self::Clear => None,
        }
    }
}

/// Batch request to set agent-suite matching overrides. Keyed by the "other" entity's UUID
/// (agent UUIDs for a fixed suite; suite UUIDs for a fixed agent in the reverse endpoint).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuiteAgentOverrideReq {
    pub overrides: HashMap<Uuid, SuiteAgentOverrideAction>,
}

/// Batch response: entries that could not be applied, keyed by the same UUID as the
/// request. An empty map means every entry succeeded.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuiteAgentOverrideResp {
    pub errors: HashMap<Uuid, crate::error::ErrorMsg>,
}
