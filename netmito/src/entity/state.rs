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

#[derive(EnumIter, DeriveActiveEnum, Clone, Debug, PartialEq, Eq)]
#[sea_orm(rs_type = "i32", db_type = "Integer")]
pub enum WorkerState {
    Normal = 0,
    /// Worker is being shutdown gracefully. It should only be shutdown when fetching new task
    GracefulShutdown = 1,
}
