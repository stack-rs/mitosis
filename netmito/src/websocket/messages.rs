use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use uuid::Uuid;

use crate::entity::state::NodeManagerState;
use crate::schema::{ReportTaskOp, WorkerTaskResp};

// Manager → Coordinator Messages (6 types)
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

// Coordinator → Manager Messages (7 types)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_heartbeat_serialization() {
        let metrics = ManagerMetrics {
            active_workers: 4,
            total_tasks_completed: 100,
            total_tasks_failed: 5,
            current_suite_tasks_completed: 50,
            current_suite_tasks_failed: 2,
            uptime_seconds: 3600,
            cpu_usage_percent: 45.5,
            memory_usage_mb: 1024,
        };

        let msg = ManagerMessage::Heartbeat {
            manager_uuid: Uuid::new_v4(),
            state: NodeManagerState::Executing,
            metrics,
        };

        // Test serialization
        let json = serde_json::to_string(&msg).expect("Failed to serialize");
        assert!(json.contains("\"type\":\"heartbeat\""));

        // Test deserialization
        let _deserialized: ManagerMessage =
            serde_json::from_str(&json).expect("Failed to deserialize");
    }

    #[test]
    fn test_fetch_task_serialization() {
        let msg = ManagerMessage::FetchTask {
            request_id: 42,
            worker_local_id: 1,
        };

        let json = serde_json::to_string(&msg).expect("Failed to serialize");
        assert!(json.contains("\"type\":\"fetch_task\""));
        assert!(json.contains("\"request_id\":42"));

        let _deserialized: ManagerMessage =
            serde_json::from_str(&json).expect("Failed to deserialize");
    }

    #[test]
    fn test_task_available_serialization() {
        let msg = CoordinatorMessage::TaskAvailable {
            request_id: 42,
            task: None,
        };

        let json = serde_json::to_string(&msg).expect("Failed to serialize");
        assert!(json.contains("\"type\":\"task_available\""));
        assert!(json.contains("\"request_id\":42"));

        let _deserialized: CoordinatorMessage =
            serde_json::from_str(&json).expect("Failed to deserialize");
    }

    #[test]
    fn test_suite_completed_serialization() {
        let msg = ManagerMessage::SuiteCompleted {
            suite_uuid: Uuid::new_v4(),
            tasks_completed: 100,
            tasks_failed: 5,
        };

        let json = serde_json::to_string(&msg).expect("Failed to serialize");
        assert!(json.contains("\"type\":\"suite_completed\""));

        let _deserialized: ManagerMessage =
            serde_json::from_str(&json).expect("Failed to deserialize");
    }

    #[test]
    fn test_invalid_json_returns_error() {
        let invalid_json = "{\"type\":\"invalid_type\"}";
        let result: Result<ManagerMessage, _> = serde_json::from_str(invalid_json);
        assert!(result.is_err());
    }
}
