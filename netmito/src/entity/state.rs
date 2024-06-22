use std::fmt::Display;

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(EnumIter, DeriveActiveEnum, Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[sea_orm(rs_type = "i32", db_type = "Integer")]
pub enum UserState {
    Active = 0,
    Locked = 1,
    Deleted = 2,
}

#[derive(EnumIter, DeriveActiveEnum, Clone, Debug, PartialEq, Eq)]
#[sea_orm(rs_type = "i32", db_type = "Integer")]
pub enum GroupState {
    Active = 0,
    Locked = 1,
    Deleted = 2,
}

#[derive(EnumIter, DeriveActiveEnum, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[sea_orm(rs_type = "i32", db_type = "Integer")]
pub enum TaskState {
    /// Reserved for future use
    Pending = 0,
    /// Task is ready to be fetched and executed
    Ready = 1,
    /// Task is being executed by some worker
    Running = 2,
    /// Task has been successfully executed, but not sure if it succeeded or not
    Finished = 3,
    /// Task is canceled by the worker due to timeout
    Cancelled = 4,
    Unknown = 5,
}

impl Display for TaskState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TaskState::Pending => write!(f, "Pending"),
            TaskState::Ready => write!(f, "Ready"),
            TaskState::Running => write!(f, "Running"),
            TaskState::Finished => write!(f, "Finished"),
            TaskState::Cancelled => write!(f, "Cancelled"),
            TaskState::Unknown => write!(f, "Unknown"),
        }
    }
}

// This is specific to the task execution state (the lifetime of its execution in a worker)
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskExecState {
    WorkerExited = -1,
    FetchResource = 1,
    FetchResourceError = 2,
    FetchResourceTimeout = 3,
    FetchResourceNotFound = 4,
    FetchResourceForbidden = 5,
    ExecPending = 6,
    ExecSpawned = 7,
    ExecTimeout = 8,
    UploadFinishResult = 9,
    UploadCancelResult = 10,
    UploadResultTimeout = 11,
    TaskCommitted = 12,
    Unknown = -99,
}

impl From<i32> for TaskExecState {
    fn from(v: i32) -> Self {
        match v {
            -1 => TaskExecState::WorkerExited,
            1 => TaskExecState::FetchResource,
            2 => TaskExecState::FetchResourceError,
            3 => TaskExecState::FetchResourceTimeout,
            4 => TaskExecState::FetchResourceNotFound,
            5 => TaskExecState::FetchResourceForbidden,
            6 => TaskExecState::ExecPending,
            7 => TaskExecState::ExecSpawned,
            8 => TaskExecState::ExecTimeout,
            9 => TaskExecState::UploadFinishResult,
            10 => TaskExecState::UploadCancelResult,
            11 => TaskExecState::UploadResultTimeout,
            12 => TaskExecState::TaskCommitted,
            _ => TaskExecState::Unknown,
        }
    }
}

#[derive(EnumIter, DeriveActiveEnum, Clone, Debug, PartialEq, Eq)]
#[sea_orm(rs_type = "i32", db_type = "Integer")]
pub enum WorkerState {
    Normal = 0,
    /// Worker is being shutdown gracefully. It should only be shutdown when fetching new task
    GracefulShutdown = 1,
}
