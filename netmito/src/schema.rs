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
    pub command: TaskCommand,
    pub args: Vec<String>,
    #[serde(default)]
    pub envs: HashMap<String, String>,
    #[serde(default)]
    pub terminal_output: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone, clap::ValueEnum)]
pub enum TaskCommand {
    Sh,
    Bash,
    Zsh,
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
pub struct ArtifactDownloadResp {
    pub url: String,
    pub size: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ArtifactDownloadInfo {
    pub size: i64,
    pub file_path: PathBuf,
}

impl TaskSpec {
    pub fn new<T, I, P, Q>(command: TaskCommand, args: I, envs: P, terminal_output: bool) -> Self
    where
        I: IntoIterator<Item = T>,
        T: Into<String>,
        P: IntoIterator<Item = Q>,
        Q: Into<(String, String)>,
    {
        Self {
            command,
            args: args.into_iter().map(Into::into).collect(),
            envs: envs.into_iter().map(Into::into).collect(),
            terminal_output,
        }
    }
}

impl AsRef<str> for TaskCommand {
    fn as_ref(&self) -> &str {
        match self {
            TaskCommand::Sh => "sh",
            TaskCommand::Bash => "bash",
            TaskCommand::Zsh => "zsh",
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
