use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::entity::{content::ArtifactContentType, state::UserState};

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

impl TaskSpec {
    pub fn new<T, I>(command: TaskCommand, args: I, terminal_output: bool) -> Self
    where
        I: IntoIterator<Item = T>,
        T: Into<String>,
    {
        Self {
            command,
            args: args.into_iter().map(Into::into).collect(),
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
