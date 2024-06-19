use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::entity::{
    content::ArtifactContentType,
    state::{TaskState, UserState},
};

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateUserReq {
    pub username: String,
    pub md5_password: [u8; 16],
    pub admin: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UserLoginReq {
    pub username: String,
    pub md5_password: [u8; 16],
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UserLoginResp {
    pub token: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DeleteUserReq {
    pub username: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ChangeUserStateReq {
    pub username: String,
    pub state: UserState,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UserStateResp {
    pub state: UserState,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateGroupReq {
    pub group_name: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RegisterWorkerReq {
    pub tags: HashSet<String>,
    pub groups: HashSet<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RegisterWorkerResp {
    pub worker_id: Uuid,
    pub token: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TaskSpec {
    pub args: Vec<String>,
    #[serde(default)]
    pub envs: HashMap<String, String>,
    #[serde(default)]
    pub resources: Vec<RemoteResourceDownload>,
    #[serde(default)]
    pub terminal_output: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WorkerTaskResp {
    pub id: i64,
    #[serde(with = "humantime_serde")]
    pub timeout: std::time::Duration,
    pub spec: TaskSpec,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReportTaskReq {
    pub id: i64,
    pub op: ReportTaskOp,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum ReportTaskOp {
    Finish,
    Cancel,
    Commit(TaskResultSpec),
    Upload {
        content_type: ArtifactContentType,
        content_length: i64,
    },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReportTaskResp {
    pub url: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SubmitTaskReq {
    pub group_name: String,
    pub tags: HashSet<String>,
    #[serde(with = "humantime_serde")]
    pub timeout: std::time::Duration,
    pub priority: i32,
    pub task_spec: TaskSpec,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SubmitTaskResp {
    pub task_id: i64,
    pub uuid: Uuid,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TaskResultSpec {
    pub exit_status: i32,
    pub msg: Option<TaskResultMessage>,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum TaskResultMessage {
    FetchResourceTimeout,
    ExecTimeout,
    UploadResultTimeout,
    ResourceNotFound,
    ResourceForbidden,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ArtifactQueryResp {
    pub content_type: ArtifactContentType,
    pub size: i64,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TaskQueryResp {
    pub uuid: Uuid,
    pub creator_username: String,
    pub group_name: String,
    pub task_id: i64,
    pub tags: Vec<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    pub state: TaskState,
    pub timeout: i64,
    pub priority: i32,
    pub spec: TaskSpec,
    pub result: Option<TaskResultSpec>,
    pub artifacts: Vec<ArtifactQueryResp>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RemoteResourceDownloadResp {
    pub url: String,
    pub size: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ResourceDownloadInfo {
    pub size: i64,
    pub local_path: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum RemoteResource {
    Artifact {
        uuid: Uuid,
        content_type: ArtifactContentType,
    },
    Attachment {
        key: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RemoteResourceDownload {
    pub remote_file: RemoteResource,
    /// The relative local file path of the resource downloaded to at the cache directory.
    /// Will append the path to the worker's working directory.
    pub local_path: PathBuf,
}

impl TaskSpec {
    pub fn new<T, I, P, Q, V>(args: I, envs: P, files: V, terminal_output: bool) -> Self
    where
        I: IntoIterator<Item = T>,
        T: Into<String>,
        P: IntoIterator<Item = Q>,
        Q: Into<(String, String)>,
        V: IntoIterator<Item = RemoteResourceDownload>,
    {
        Self {
            args: args.into_iter().map(Into::into).collect(),
            envs: envs.into_iter().map(Into::into).collect(),
            resources: files.into_iter().collect(),
            terminal_output,
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
