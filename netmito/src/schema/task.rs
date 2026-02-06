use std::collections::HashSet;

use sea_orm::FromQueryResult;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::entity::{content::ArtifactContentType, state::TaskState};

use super::artifact::ArtifactQueryResp;
use super::exec::{ExecSpec, TaskExecOptions};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct WorkerTaskResp {
    pub id: i64,
    pub uuid: Uuid,
    pub upstream_task_uuid: Option<Uuid>,
    pub spec: ExecSpec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec_options: Option<TaskExecOptions>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ReportTaskReq {
    pub id: i64,
    pub op: ReportTaskOp,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum ReportTaskOp {
    Finish,
    Cancel,
    Submit(Box<SubmitTaskReq>),
    Commit(TaskResultSpec),
    Upload {
        content_type: ArtifactContentType,
        content_length: u64,
    },
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ReportTaskResp {
    pub url: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SubmitTaskReq {
    pub group_name: String,
    /// Optional suite UUID to assign task to
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suite_uuid: Option<Uuid>,
    pub tags: HashSet<String>,
    pub labels: HashSet<String>,
    pub priority: i32,
    /// Core execution specification
    pub spec: ExecSpec,
    /// Task-specific execution options
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec_options: Option<TaskExecOptions>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SubmitTaskResp {
    pub task_id: i64,
    pub uuid: Uuid,
    /// Suite UUID (echoed back if provided in request)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suite_uuid: Option<Uuid>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ChangeTaskReq {
    pub tags: Option<HashSet<String>>,
    pub priority: Option<i32>,
    pub spec: Option<ExecSpec>,
    pub exec_options: Option<TaskExecOptions>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UpdateTaskLabelsReq {
    pub labels: HashSet<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TaskResultSpec {
    pub exit_status: i32,
    pub msg: Option<TaskResultMessage>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum TaskResultMessage {
    FetchResourceTimeout,
    ExecTimeout,
    UploadResultTimeout,
    ResourceNotFound,
    ResourceForbidden,
    WatchTimeout,
    // May record the user name who cancels the task.
    UserCancellation,
    SubmitNewTaskFailed,
}

#[derive(Debug, Serialize, Deserialize, FromQueryResult, Clone)]
pub struct TaskQueryInfo {
    pub uuid: Uuid,
    pub creator_username: String,
    pub group_name: String,
    pub task_id: i64,
    pub tags: Vec<String>,
    pub labels: Vec<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    pub state: TaskState,
    pub priority: i32,
    pub spec: serde_json::Value,
    pub exec_options: Option<serde_json::Value>,
    pub result: Option<serde_json::Value>,
    pub upstream_task_uuid: Option<Uuid>,
    pub downstream_task_uuid: Option<Uuid>,
    pub reporter_uuid: Option<Uuid>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ParsedTaskQueryInfo {
    pub uuid: Uuid,
    pub creator_username: String,
    pub group_name: String,
    pub task_id: i64,
    pub tags: Vec<String>,
    pub labels: Vec<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    pub state: TaskState,
    pub priority: i32,
    pub spec: ExecSpec,
    pub exec_options: Option<TaskExecOptions>,
    pub result: Option<TaskResultSpec>,
    pub upstream_task_uuid: Option<Uuid>,
    pub downstream_task_uuid: Option<Uuid>,
    pub reporter_uuid: Option<Uuid>,
}

/// Each field in the query request is optional, and the server will return all tasks if no field is specified.
///
/// The relationship between the fields is AND.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TasksQueryReq {
    pub creator_usernames: Option<HashSet<String>>,
    pub group_name: Option<String>,
    pub tags: Option<HashSet<String>>,
    pub labels: Option<HashSet<String>>,
    pub states: Option<HashSet<TaskState>>,
    pub exit_status: Option<String>,
    pub priority: Option<String>,
    /// Set reporter_uuid will automatically exclude all non-completed tasks.
    pub reporter_uuid: Option<Uuid>,
    /// Filter tasks by suite UUID
    pub suite_uuid: Option<Uuid>,
    pub limit: Option<u64>,
    pub offset: Option<u64>,
    pub count: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TaskQueryResp {
    pub info: ParsedTaskQueryInfo,
    pub artifacts: Vec<ArtifactQueryResp>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TasksQueryResp {
    pub count: u64,
    pub tasks: Vec<TaskQueryInfo>,
    pub group_name: String,
}

/// Request to cancel multiple tasks by filter criteria.
/// Uses the same filter fields as TasksQueryReq but without pagination.
/// Only tasks in Ready state will be cancelled.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TasksCancelByFilterReq {
    pub creator_usernames: Option<HashSet<String>>,
    pub group_name: Option<String>,
    pub tags: Option<HashSet<String>>,
    pub labels: Option<HashSet<String>>,
    pub states: Option<HashSet<TaskState>>,
    pub exit_status: Option<String>,
    pub priority: Option<String>,
    /// Filter tasks by suite UUID
    pub suite_uuid: Option<Uuid>,
}

/// Response for batch cancel operation
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TasksCancelByFilterResp {
    pub cancelled_count: u64,
    pub group_name: String,
}

/// Request to cancel multiple tasks by UUIDs
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TasksCancelByUuidsReq {
    pub uuids: Vec<Uuid>,
}

/// Response for batch cancel by UUIDs operation
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TasksCancelByUuidsResp {
    pub cancelled_count: u64,
    pub failed_uuids: Vec<Uuid>,
}

/// Request to batch submit tasks
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TasksSubmitReq {
    pub tasks: Vec<SubmitTaskReq>,
}

/// Response for batch task submission
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TasksSubmitResp {
    pub results: Vec<Result<SubmitTaskResp, crate::error::ErrorMsg>>,
}
