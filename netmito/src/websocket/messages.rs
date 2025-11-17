use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use uuid::Uuid;

use crate::entity::state::NodeManagerState;
use crate::schema::{ReportTaskOp, WorkerTaskResp};

// Manager → Coordinator Messages
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ManagerMessage {
    Heartbeat {
        manager_uuid: Uuid,
        state: NodeManagerState,
        metrics: ManagerMetrics,
    },
    FetchTask {
        request_id: u64,
        worker_local_id: u32,
    },
    ReportTask {
        request_id: u64,
        task_id: i64,
        op: ReportTaskOp,
    },
    ReportFailure {
        task_uuid: Uuid,
        failure_count: u32,
        error_message: String,
        worker_local_id: u32,
    },
    AbortTask {
        task_uuid: Uuid,
        reason: String,
    },
    SuiteCompleted {
        suite_uuid: Uuid,
        tasks_completed: u64,
        tasks_failed: u64,
    },
}

// Coordinator → Manager Messages
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CoordinatorMessage {
    SuiteAssigned {
        suite_uuid: Uuid,
        suite_spec: TaskSuiteSpec,
    },
    TaskAvailable {
        request_id: u64,
        task: Option<WorkerTaskResp>,
    },
    TaskReportAck {
        request_id: u64,
        success: bool,
        url: Option<String>,
    },
    CancelTask {
        task_uuid: Uuid,
        reason: String,
    },
    CancelSuite {
        suite_uuid: Uuid,
        reason: String,
        cancel_running_tasks: bool,
    },
    ConfigUpdate {
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            with = "humantime_serde"
        )]
        lease_duration: Option<Duration>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            with = "humantime_serde"
        )]
        heartbeat_interval: Option<Duration>,
    },
    Shutdown {
        graceful: bool,
    },
}

// Supporting Types
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagerMetrics {
    pub active_workers: u32,
    pub total_tasks_completed: u64,
    pub total_tasks_failed: u64,
    pub current_suite_tasks_completed: u64,
    pub current_suite_tasks_failed: u64,
    pub uptime_seconds: u64,
    pub cpu_usage_percent: f32,
    pub memory_usage_mb: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSuiteSpec {
    pub uuid: Uuid,
    pub name: String,
    pub description: String,
    pub group_id: i64,
    pub tags: HashSet<String>,
    pub labels: HashSet<String>,
    pub priority: i32,
    pub worker_schedule: WorkerSchedulePlan,
    pub env_preparation: Option<EnvHookSpec>,
    pub env_cleanup: Option<EnvHookSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerSchedulePlan {
    pub worker_count: u32,
    pub cpu_binding: Option<CpuBindingStrategy>,
    pub task_prefetch_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CpuBindingStrategy {
    pub cores: Vec<u32>,
    pub strategy: BindingMode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingMode {
    RoundRobin,
    Exclusive,
    Shared,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvHookSpec {
    pub args: Vec<String>,
    pub envs: HashMap<String, String>,
    pub resources: Vec<ResourceSpec>,
    #[serde(with = "humantime_serde")]
    pub timeout: Duration,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceSpec {
    pub url: String,
    pub path: String,
}
