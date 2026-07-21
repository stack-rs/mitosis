use std::collections::HashSet;

use serde::{Deserialize, Serialize};
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
