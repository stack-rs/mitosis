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
pub struct DeleteUserReq {
    pub username: String,
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
pub struct GroupStorageQuotaResp {
    pub storage_quota: i64,
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

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AttachmentMetadata {
    pub content_type: AttachmentContentType,
    pub size: i64,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AttachmentsQueryReq {
    pub key_prefix: Option<String>,
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
    pub creator_username: Option<String>,
    pub count: bool,
}

#[derive(Debug, Serialize, Deserialize, FromQueryResult, Clone)]
pub(crate) struct RawWorkerQueryInfo {
    pub(crate) id: i64,
    pub(crate) worker_id: Uuid,
    pub(crate) creator_username: String,
    pub(crate) tags: Vec<String>,
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
pub struct UpdateGroupWorkerRoleReq {
    pub relations: HashMap<String, GroupWorkerRole>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RemoveGroupWorkerRoleReq {
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
pub struct ShutdownReq {
    pub secret: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct WorkerShutdown {
    pub op: Option<WorkerShutdownOp>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum WorkerShutdownOp {
    #[serde(alias = "graceful")]
    Graceful,
    #[serde(alias = "force")]
    Force,
}

impl Default for WorkerShutdownOp {
    fn default() -> Self {
        Self::Graceful
    }
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
            created_at: model.created_at,
            updated_at: model.updated_at,
            state: model.state,
            last_heartbeat: model.last_heartbeat,
            assigned_task_id: model.assigned_task_id,
        }
    }
}
