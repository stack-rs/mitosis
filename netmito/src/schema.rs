use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};

use sea_orm::FromQueryResult;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::entity::{
    content::{ArtifactContentType, AttachmentContentType},
    role::{GroupWorkerRole, UserGroupRole},
    state::{GroupState, TaskExecState, TaskState, UserState, WorkerState},
};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CreateUserReq {
    pub username: String,
    pub md5_password: [u8; 16],
    pub admin: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UserLoginReq {
    pub username: String,
    pub md5_password: [u8; 16],
    #[serde(default)]
    pub retain: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UserLoginResp {
    pub token: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UserChangePasswordReq {
    pub old_md5_password: [u8; 16],
    pub new_md5_password: [u8; 16],
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UserChangePasswordResp {
    pub token: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AdminChangePasswordReq {
    pub new_md5_password: [u8; 16],
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ChangeUserStateReq {
    pub state: UserState,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UserStateResp {
    pub state: UserState,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ChangeGroupStorageQuotaReq {
    pub storage_quota: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ChangeUserGroupQuota {
    pub group_quota: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GroupStorageQuotaResp {
    pub storage_quota: i64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UserGroupQuotaResp {
    pub group_quota: i32,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CreateGroupReq {
    pub group_name: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GroupQueryInfo {
    pub group_name: String,
    pub creator_username: String,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    pub state: GroupState,
    pub task_count: i64,
    pub storage_quota: i64,
    pub storage_used: i64,
    pub worker_count: i64,
    pub users_in_group: Option<HashMap<String, UserGroupRole>>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RegisterWorkerReq {
    pub tags: HashSet<String>,
    pub labels: HashSet<String>,
    pub groups: HashSet<String>,
    #[serde(default)]
    #[serde(with = "humantime_serde")]
    pub lifetime: Option<std::time::Duration>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RegisterWorkerResp {
    pub worker_id: Uuid,
    pub token: String,
    pub redis_url: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TaskSpec {
    pub args: Vec<String>,
    #[serde(default)]
    pub envs: HashMap<String, String>,
    #[serde(default)]
    pub resources: Vec<RemoteResourceDownload>,
    #[serde(default)]
    pub terminal_output: bool,
    pub watch: Option<(Uuid, TaskExecState)>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct WorkerTaskResp {
    pub id: i64,
    pub uuid: Uuid,
    #[serde(with = "humantime_serde")]
    pub timeout: std::time::Duration,
    pub upstream_task_uuid: Option<Uuid>,
    pub spec: TaskSpec,
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
pub struct UploadAttachmentReq {
    pub key: String,
    pub content_length: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UploadAttachmentResp {
    pub url: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UploadArtifactReq {
    pub content_type: ArtifactContentType,
    pub content_length: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UploadArtifactResp {
    pub url: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SubmitTaskReq {
    pub group_name: String,
    /// Optional suite UUID to assign task to
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suite_uuid: Option<Uuid>,
    pub tags: HashSet<String>,
    pub labels: HashSet<String>,
    #[serde(with = "humantime_serde")]
    pub timeout: std::time::Duration,
    pub priority: i32,
    pub task_spec: TaskSpec,
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
    #[serde(with = "humantime_serde")]
    pub timeout: Option<std::time::Duration>,
    pub priority: Option<i32>,
    pub task_spec: Option<TaskSpec>,
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

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ArtifactQueryResp {
    pub content_type: ArtifactContentType,
    pub size: i64,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
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
    pub timeout: i64,
    pub priority: i32,
    pub spec: serde_json::Value,
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
    pub timeout: i64,
    pub priority: i32,
    pub spec: TaskSpec,
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

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AttachmentMetadata {
    pub content_type: AttachmentContentType,
    pub size: i64,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AttachmentsQueryReq {
    pub key: Option<String>,
    pub limit: Option<u64>,
    pub offset: Option<u64>,
    pub count: bool,
}

#[derive(Debug, Serialize, Deserialize, FromQueryResult, Clone)]
pub struct AttachmentQueryInfo {
    pub key: String,
    pub content_type: AttachmentContentType,
    pub size: i64,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AttachmentsQueryResp {
    pub count: u64,
    pub attachments: Vec<AttachmentQueryInfo>,
    pub group_name: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct WorkersQueryReq {
    pub group_name: Option<String>,
    pub role: Option<HashSet<GroupWorkerRole>>,
    pub tags: Option<HashSet<String>>,
    pub labels: Option<HashSet<String>>,
    pub creator_username: Option<String>,
    pub count: bool,
}

/// Request to shutdown multiple workers by filter criteria.
/// Uses the same filter fields as WorkersQueryReq.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct WorkersShutdownByFilterReq {
    pub group_name: Option<String>,
    pub role: Option<HashSet<GroupWorkerRole>>,
    pub tags: Option<HashSet<String>>,
    pub labels: Option<HashSet<String>>,
    pub creator_username: Option<String>,
    pub op: WorkerShutdownOp,
}

/// Response for batch worker shutdown operation
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct WorkersShutdownByFilterResp {
    pub shutdown_count: u64,
    pub group_name: String,
}

/// Request to shutdown multiple workers by UUIDs
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct WorkersShutdownByUuidsReq {
    pub uuids: Vec<Uuid>,
    pub op: WorkerShutdownOp,
}

/// Response for batch worker shutdown by UUIDs operation
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct WorkersShutdownByUuidsResp {
    pub shutdown_count: u64,
    pub failed_uuids: Vec<Uuid>,
}

#[derive(Debug, Serialize, Deserialize, FromQueryResult, Clone)]
pub(crate) struct RawWorkerQueryInfo {
    pub(crate) id: i64,
    pub(crate) worker_id: Uuid,
    pub(crate) creator_username: String,
    pub(crate) tags: Vec<String>,
    pub(crate) labels: Vec<String>,
    pub(crate) created_at: OffsetDateTime,
    pub(crate) updated_at: OffsetDateTime,
    pub(crate) state: WorkerState,
    pub(crate) last_heartbeat: OffsetDateTime,
    pub(crate) assigned_task_id: Option<Uuid>,
}

#[derive(Debug, Serialize, Deserialize, FromQueryResult, Clone)]
pub struct CountQuery {
    pub count: i64,
}

#[derive(Debug, Serialize, Deserialize, FromQueryResult, Clone)]
pub struct WorkerQueryInfo {
    pub worker_id: Uuid,
    pub creator_username: String,
    pub tags: Vec<String>,
    pub labels: Vec<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    pub state: WorkerState,
    pub last_heartbeat: OffsetDateTime,
    pub assigned_task_id: Option<Uuid>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct WorkerQueryResp {
    pub info: WorkerQueryInfo,
    pub groups: HashMap<String, GroupWorkerRole>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct WorkersQueryResp {
    pub count: u64,
    pub workers: Vec<WorkerQueryInfo>,
    pub group_name: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RedisConnectionInfo {
    pub url: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RemoteResourceDownloadResp {
    pub url: String,
    pub size: i64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ResourceDownloadInfo {
    pub size: i64,
    pub local_path: PathBuf,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum RemoteResource {
    Artifact {
        uuid: Uuid,
        content_type: ArtifactContentType,
    },
    Attachment {
        key: String,
    },
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RemoteResourceDownload {
    pub remote_file: RemoteResource,
    /// The relative local file path of the resource downloaded to at the cache directory.
    /// Will append the path to the worker's working directory.
    pub local_path: PathBuf,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GroupsQueryResp {
    pub groups: HashMap<String, UserGroupRole>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ReplaceWorkerTagsReq {
    pub tags: HashSet<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ReplaceWorkerLabelsReq {
    pub labels: HashSet<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UpdateGroupWorkerRoleReq {
    pub relations: HashMap<String, GroupWorkerRole>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RemoveGroupWorkerRoleReq {
    pub groups: HashSet<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RemoveGroupWorkerRoleParams {
    #[serde(default)]
    pub groups: HashSet<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UpdateUserGroupRoleReq {
    pub relations: HashMap<String, UserGroupRole>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RemoveUserGroupRoleReq {
    pub users: HashSet<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RemoveUserGroupRoleParams {
    #[serde(default)]
    pub users: HashSet<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ShutdownReq {
    pub secret: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct WorkerShutdown {
    pub op: Option<WorkerShutdownOp>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub enum WorkerShutdownOp {
    #[default]
    #[serde(alias = "graceful")]
    Graceful,
    #[serde(alias = "force")]
    Force,
}

/// Request to batch download artifacts by filter criteria.
/// Uses the same filter fields as TasksQueryReq to find tasks, then downloads artifacts of the specified type.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ArtifactsDownloadByFilterReq {
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
    pub content_type: ArtifactContentType,
}

/// Request to batch download artifacts by task UUIDs.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ArtifactsDownloadByUuidsReq {
    pub uuids: Vec<Uuid>,
    pub content_type: ArtifactContentType,
}

/// Single artifact download item in batch response
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ArtifactDownloadItem {
    pub uuid: Uuid,
    pub url: String,
    pub size: i64,
}

/// Response for batch artifact download operations
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ArtifactsDownloadListResp {
    pub downloads: Vec<ArtifactDownloadItem>,
}

/// Request to batch download attachments by filter criteria.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AttachmentsDownloadByFilterReq {
    pub key: Option<String>,
    pub limit: Option<u64>,
    pub offset: Option<u64>,
}

/// Request to batch download attachments by keys.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AttachmentsDownloadByKeysReq {
    pub keys: Vec<String>,
}

/// Single attachment download item in batch response
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AttachmentDownloadItem {
    pub key: String,
    pub url: String,
    pub size: i64,
}

/// Response for batch attachment download operations
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AttachmentsDownloadListResp {
    pub downloads: Vec<AttachmentDownloadItem>,
    pub group_name: String,
}

/// Request to batch delete artifacts by filter criteria.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ArtifactsDeleteByFilterReq {
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
    pub content_type: ArtifactContentType,
}

/// Request to batch delete artifacts by task UUIDs.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ArtifactsDeleteByUuidsReq {
    pub uuids: Vec<Uuid>,
    pub content_type: ArtifactContentType,
}

/// Response for batch artifact deletion by filter
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ArtifactsDeleteByFilterResp {
    pub deleted_count: u64,
}

/// Response for batch artifact deletion by UUIDs
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ArtifactsDeleteByUuidsResp {
    pub deleted_count: u64,
    pub failed_uuids: Vec<Uuid>,
}

/// Request to batch delete attachments by filter criteria.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AttachmentsDeleteByFilterReq {
    pub key: Option<String>,
    pub limit: Option<u64>,
    pub offset: Option<u64>,
}

/// Request to batch delete attachments by keys.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AttachmentsDeleteByKeysReq {
    pub keys: Vec<String>,
}

/// Response for batch attachment deletion by filter
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AttachmentsDeleteByFilterResp {
    pub deleted_count: u64,
    pub group_name: String,
}

/// Response for batch attachment deletion by keys
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AttachmentsDeleteByKeysResp {
    pub deleted_count: u64,
    pub failed_keys: Vec<String>,
    pub group_name: String,
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

impl TaskSpec {
    pub fn new<T, I, P, Q, V, U>(
        args: I,
        envs: P,
        files: V,
        terminal_output: bool,
        watch: U,
    ) -> Self
    where
        I: IntoIterator<Item = T>,
        T: Into<String>,
        P: IntoIterator<Item = Q>,
        Q: Into<(String, String)>,
        V: IntoIterator<Item = RemoteResourceDownload>,
        U: Into<Option<(Uuid, TaskExecState)>>,
    {
        Self {
            args: args.into_iter().map(Into::into).collect(),
            envs: envs.into_iter().map(Into::into).collect(),
            resources: files.into_iter().collect(),
            terminal_output,
            watch: watch.into(),
        }
    }
}

impl From<crate::entity::artifacts::Model> for ArtifactQueryResp {
    fn from(model: crate::entity::artifacts::Model) -> Self {
        Self {
            content_type: model.content_type,
            size: model.size,
            created_at: model.created_at,
            updated_at: model.updated_at,
        }
    }
}

impl From<RawWorkerQueryInfo> for WorkerQueryInfo {
    fn from(model: RawWorkerQueryInfo) -> Self {
        Self {
            worker_id: model.worker_id,
            creator_username: model.creator_username,
            tags: model.tags,
            labels: model.labels,
            created_at: model.created_at,
            updated_at: model.updated_at,
            state: model.state,
            last_heartbeat: model.last_heartbeat,
            assigned_task_id: model.assigned_task_id,
        }
    }
}

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
    /// Tags for manager matching (e.g., ["wireless", "linux", "cuda:11"])
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
    /// Optional environment preparation hook
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_preparation: Option<EnvHookSpec>,
    /// Optional environment cleanup hook
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_cleanup: Option<EnvHookSpec>,
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

/// Environment hook specification (preparation or cleanup)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvHookSpec {
    /// Command and arguments to execute
    pub args: Vec<String>,
    /// Environment variables to set
    #[serde(default)]
    pub envs: HashMap<String, String>,
    /// Remote resources to download before execution
    #[serde(default)]
    pub resources: Vec<RemoteResourceDownload>,
    /// Execution timeout (e.g., "5m", "1h")
    #[serde(with = "humantime_serde")]
    pub timeout: std::time::Duration,
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
    pub env_preparation: Option<EnvHookSpec>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env_cleanup: Option<EnvHookSpec>,
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
    pub assigned_managers: Vec<Uuid>,
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
    pub env_preparation: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env_cleanup: Option<serde_json::Value>,
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

/// Response after cancelling a suite
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

// ============================================================================
// Node Manager WebSocket Messages
// ============================================================================

/// Messages sent from Manager → Coordinator
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ManagerMessage {
    Heartbeat {
        manager_uuid: Uuid,
        state: crate::entity::state::NodeManagerState,
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

/// Manager metrics sent with heartbeat
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

/// Messages sent from Coordinator → Manager
#[derive(Debug, Serialize, Deserialize, Clone)]
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
        #[serde(default, with = "humantime_serde")]
        lease_duration: Option<std::time::Duration>,
        #[serde(default, with = "humantime_serde")]
        heartbeat_interval: Option<std::time::Duration>,
    },
    Shutdown {
        graceful: bool,
    },
}

/// Task suite specification for manager (uses existing types from above)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSuiteSpec {
    pub uuid: Uuid,
    pub name: Option<String>,
    pub description: Option<String>,
    pub group_id: i64,
    pub tags: Vec<String>,
    pub labels: Vec<String>,
    pub priority: i32,
    pub worker_schedule: WorkerSchedulePlan,
    pub env_preparation: Option<EnvHookSpec>,
    pub env_cleanup: Option<EnvHookSpec>,
}

// ============================================================================
// Node Manager Registration API
// ============================================================================

/// Request to register a new node manager
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RegisterManagerReq {
    pub tags: Vec<String>,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub groups: Vec<String>,
    #[serde(default, with = "humantime_serde")]
    pub lifetime: Option<std::time::Duration>,
}

/// Response after registering a manager
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RegisterManagerResp {
    pub manager_uuid: Uuid,
    pub token: String,
}

/// Request for manager heartbeat (HTTP fallback)
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ManagerHeartbeatReq {
    pub state: crate::entity::state::NodeManagerState,
    pub metrics: ManagerMetrics,
}
