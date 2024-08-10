use std::{convert::Infallible, fmt::Display, str::FromStr};

use clap::ValueEnum;
use matrix_match::matrix_match;
use redis::FromRedisValue;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::schema::{TaskResultMessage, TaskResultSpec};

#[derive(EnumIter, DeriveActiveEnum, Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[sea_orm(rs_type = "i32", db_type = "Integer")]
pub enum UserState {
    Active = 0,
    Locked = 1,
    Deleted = 2,
}

impl Display for UserState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UserState::Active => write!(f, "Active"),
            UserState::Locked => write!(f, "Locked"),
            UserState::Deleted => write!(f, "Deleted"),
        }
    }
}

#[derive(EnumIter, DeriveActiveEnum, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[sea_orm(rs_type = "i32", db_type = "Integer")]
pub enum GroupState {
    Active = 0,
    Locked = 1,
    Deleted = 2,
}

impl Display for GroupState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GroupState::Active => write!(f, "Active"),
            GroupState::Locked => write!(f, "Locked"),
            GroupState::Deleted => write!(f, "Deleted"),
        }
    }
}

#[derive(
    EnumIter, DeriveActiveEnum, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Copy, ValueEnum,
)]
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

impl TaskState {
    pub fn is_reach(&self, target_state: &TaskExecState, result: Option<TaskResultSpec>) -> bool {
        match self {
            TaskState::Pending | TaskState::Ready | TaskState::Unknown => false,
            TaskState::Running => matches!(target_state, TaskExecState::FetchResource),
            TaskState::Finished => match target_state {
                TaskExecState::WorkerExited => false,
                TaskExecState::FetchResource => true,
                TaskExecState::FetchResourceFinished => true,
                TaskExecState::FetchResourceError => false,
                TaskExecState::FetchResourceTimeout => false,
                TaskExecState::FetchResourceNotFound => false,
                TaskExecState::FetchResourceForbidden => false,
                TaskExecState::Watch => true,
                TaskExecState::WatchFinished => true,
                TaskExecState::WatchTimeout => false,
                TaskExecState::ExecPending => true,
                TaskExecState::ExecSpawned => true,
                TaskExecState::ExecFinished => true,
                TaskExecState::ExecTimeout => false,
                TaskExecState::UploadResult => true,
                TaskExecState::UploadFinishedResult => true,
                TaskExecState::UploadCancelledResult => false,
                TaskExecState::UploadResultFinished => result.is_some(),
                TaskExecState::UploadResultTimeout => false,
                TaskExecState::TaskCommitted => result.is_some(),
                TaskExecState::Unknown => false,
            },
            TaskState::Cancelled => match result {
                Some(result_spec) => match result_spec.msg {
                    Some(msg) => matrix_match!(
                        (target_state, msg); TaskResultMessage::FetchResourceTimeout, TaskResultMessage::ExecTimeout, TaskResultMessage::UploadResultTimeout, TaskResultMessage::ResourceNotFound, TaskResultMessage::ResourceForbidden, TaskResultMessage::WatchTimeout, TaskResultMessage::UserCancellation =>
                        TaskExecState::WorkerExited             => false, false, false, false, false, false, false;
                        TaskExecState::FetchResource            => false, false, false, false, false, false, false;
                        TaskExecState::FetchResourceFinished    => false, false, false, false, false, false, false;
                        TaskExecState::FetchResourceError       => false, false, false, false, false, false, false;
                        TaskExecState::FetchResourceTimeout     => true,  false, false, false, false, false, false;
                        TaskExecState::FetchResourceNotFound    => false, false, false, true,  false, false, false;
                        TaskExecState::FetchResourceForbidden   => false, false, false, false, true,  false, false;
                        TaskExecState::Watch                    => false, false, false, false, false, false, false;
                        TaskExecState::WatchFinished            => false, false, false, false, false, false, false;
                        TaskExecState::WatchTimeout             => false, false, false, false, false, true,  false;
                        TaskExecState::ExecPending              => false, false, false, false, false, false, false;
                        TaskExecState::ExecSpawned              => false, false, false, false, false, false, false;
                        TaskExecState::ExecFinished             => false, false, false, false, false, false, false;
                        TaskExecState::ExecTimeout              => false, true,  false, false, false, false, false;
                        TaskExecState::UploadResult             => true,  true,  true,  true,  true,  true,  false;
                        TaskExecState::UploadFinishedResult     => false, false, false, false, false, false, false;
                        TaskExecState::UploadCancelledResult    => true,  true,  true,  true,  true,  true,  false;
                        TaskExecState::UploadResultFinished     => true,  true,  true,  true,  true,  true,  false;
                        TaskExecState::UploadResultTimeout      => false, false, true,  false, false, false, false;
                        TaskExecState::TaskCommitted            => true,  true,  true,  true,  true,  true,  true;
                        TaskExecState::Unknown                  => false, false, false, false, false, false, false;
                    ),
                    None => false,
                },
                None => false,
            },
        }
    }
}

// This is specific to the task execution state (the lifetime of its execution in a worker)
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Copy)]
pub enum TaskExecState {
    WorkerExited = -1,
    FetchResource = 1,
    FetchResourceFinished = 2,
    FetchResourceError = 3,
    FetchResourceTimeout = 4,
    FetchResourceNotFound = 5,
    FetchResourceForbidden = 6,
    Watch = 7,
    WatchFinished = 8,
    WatchTimeout = 9,
    ExecPending = 10,
    ExecSpawned = 11,
    ExecFinished = 12,
    ExecTimeout = 13,
    UploadResult = 14,
    UploadFinishedResult = 15,
    UploadCancelledResult = 16,
    UploadResultFinished = 17,
    UploadResultTimeout = 18,
    TaskCommitted = 19,
    Unknown = -99,
}

impl From<i32> for TaskExecState {
    fn from(v: i32) -> Self {
        match v {
            -1 => TaskExecState::WorkerExited,
            1 => TaskExecState::FetchResource,
            2 => TaskExecState::FetchResourceFinished,
            3 => TaskExecState::FetchResourceError,
            4 => TaskExecState::FetchResourceTimeout,
            5 => TaskExecState::FetchResourceNotFound,
            6 => TaskExecState::FetchResourceForbidden,
            7 => TaskExecState::Watch,
            8 => TaskExecState::WatchFinished,
            9 => TaskExecState::WatchTimeout,
            10 => TaskExecState::ExecPending,
            11 => TaskExecState::ExecSpawned,
            12 => TaskExecState::ExecFinished,
            13 => TaskExecState::ExecTimeout,
            14 => TaskExecState::UploadResult,
            15 => TaskExecState::UploadFinishedResult,
            16 => TaskExecState::UploadCancelledResult,
            17 => TaskExecState::UploadResultFinished,
            18 => TaskExecState::UploadResultTimeout,
            19 => TaskExecState::TaskCommitted,
            _ => TaskExecState::Unknown,
        }
    }
}

impl FromStr for TaskExecState {
    type Err = Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "WorkerExited" => Ok(TaskExecState::WorkerExited),
            "FetchResource" => Ok(TaskExecState::FetchResource),
            "FetchResourceFinished" => Ok(TaskExecState::FetchResourceFinished),
            "FetchResourceError" => Ok(TaskExecState::FetchResourceError),
            "FetchResourceTimeout" => Ok(TaskExecState::FetchResourceTimeout),
            "FetchResourceNotFound" => Ok(TaskExecState::FetchResourceNotFound),
            "FetchResourceForbidden" => Ok(TaskExecState::FetchResourceForbidden),
            "Watch" => Ok(TaskExecState::Watch),
            "WatchFinished" => Ok(TaskExecState::WatchFinished),
            "WatchTimeout" => Ok(TaskExecState::WatchTimeout),
            "ExecPending" => Ok(TaskExecState::ExecPending),
            "ExecSpawned" => Ok(TaskExecState::ExecSpawned),
            "ExecFinished" => Ok(TaskExecState::ExecFinished),
            "ExecTimeout" => Ok(TaskExecState::ExecTimeout),
            "UploadResult" => Ok(TaskExecState::UploadResult),
            "UploadFinishedResult" => Ok(TaskExecState::UploadFinishedResult),
            "UploadCancelledResult" => Ok(TaskExecState::UploadCancelledResult),
            "UploadResultFinished" => Ok(TaskExecState::UploadResultFinished),
            "UploadResultTimeout" => Ok(TaskExecState::UploadResultTimeout),
            "TaskCommitted" => Ok(TaskExecState::TaskCommitted),
            _ => Ok(TaskExecState::Unknown),
        }
    }
}

impl TaskExecState {
    pub fn is_reach(&self, target_state: &TaskExecState) -> bool {
        matrix_match!(
            (target_state, self) ; TaskExecState::WorkerExited, TaskExecState::FetchResource, TaskExecState::FetchResourceFinished, TaskExecState::FetchResourceError, TaskExecState::FetchResourceTimeout, TaskExecState::FetchResourceNotFound, TaskExecState::FetchResourceForbidden, TaskExecState::Watch, TaskExecState::WatchFinished, TaskExecState::WatchTimeout, TaskExecState::ExecPending, TaskExecState::ExecSpawned, TaskExecState::ExecFinished, TaskExecState::ExecTimeout, TaskExecState::UploadResult, TaskExecState::UploadFinishedResult, TaskExecState::UploadCancelledResult, TaskExecState::UploadResultFinished, TaskExecState::UploadResultTimeout, TaskExecState::TaskCommitted, TaskExecState::Unknown =>
            TaskExecState::WorkerExited             => false,   false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false, false,   false,  false,  false,  false,  false,  false,  false,  false;
            TaskExecState::FetchResource            => false,   true,   true,   true,   true,   true,   true,   true,   true,   true,   true,   true,   true,   true,   true,   true,   true,   true,   true,   true,   false;
            TaskExecState::FetchResourceFinished    => false,   false,  true,   false,  false,  false,  false,  true,   true,   true,   true,   true,   true,   true,   true,   true,   true,   true,   true,   true,   false;
            TaskExecState::FetchResourceError       => false,   false,  false,  true,   false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false;
            TaskExecState::FetchResourceTimeout     => false,   false,  false,  false,  true,   false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false;
            TaskExecState::FetchResourceNotFound    => false,   false,  false,  false,  false,  true,   false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false;
            TaskExecState::FetchResourceForbidden   => false,   false,  false,  false,  false,  false,  true,   false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false;
            TaskExecState::Watch                    => false,   false,  false,  false,  false,  false,  false,  true,   true,   true,   true,   true,   true,   true,   true,   true,   true,   true,   true,   true,   false;
            TaskExecState::WatchFinished            => false,   false,  false,  false,  false,  false,  false,  false,  true,   false,  true,   true,   true,   true,   true,   true,   true,   true,   true,   true,   false;
            TaskExecState::WatchTimeout             => false,   false,  false,  false,  false,  false,  false,  false,  false,  true,   false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false;
            TaskExecState::ExecPending              => false,   false,  false,  false,  false,  false,  false,  false,  false,  false,  true,   true,   true,   true,   true,   true,   true,   true,   true,   true,   false;
            TaskExecState::ExecSpawned              => false,   false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  true,   true,   true,   true,   true,   true,   true,   true,   true,   false;
            TaskExecState::ExecFinished             => false,   false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  true,   false,  false,  true,   false,  false,  false,  false,  false;
            TaskExecState::ExecTimeout              => false,   false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  true,   false,  false,  true,   false,  false,  false,  false;
            TaskExecState::UploadResult             => false,   false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  true,   true,   true,   true,   true,   true,   false;
            TaskExecState::UploadFinishedResult     => false,   false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  true,   false,  false,  false,  false,  false;
            TaskExecState::UploadCancelledResult    => false,   false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  true,   false,  false,  false,  false;
            TaskExecState::UploadResultFinished     => false,   false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  true,   false,  false,  false;
            TaskExecState::UploadResultTimeout      => false,   false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  true,   false,  false;
            TaskExecState::TaskCommitted            => false,   false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  true,   false;
            TaskExecState::Unknown                  => false,   false,  false,  false,  false,  false,  false,  false,  false,  false,  false,  false, false,   false,  false,  false,  false,  false,  false,  false,  false;
        )
    }

    pub fn is_end(&self) -> bool {
        matches!(
            self,
            TaskExecState::FetchResourceError | TaskExecState::TaskCommitted
        )
    }
}

impl FromRedisValue for TaskExecState {
    fn from_redis_value(v: &redis::Value) -> redis::RedisResult<Self> {
        let i = i32::from_redis_value(v)?;
        Ok(i.into())
    }

    fn from_owned_redis_value(v: redis::Value) -> redis::RedisResult<Self> {
        let i = i32::from_owned_redis_value(v)?;
        Ok(i.into())
    }
}

#[derive(EnumIter, DeriveActiveEnum, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Copy)]
#[sea_orm(rs_type = "i32", db_type = "Integer")]
pub enum WorkerState {
    Normal = 0,
    /// Worker is being shutdown gracefully. It should only be shutdown when fetching new task
    GracefulShutdown = 1,
}

impl Display for WorkerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorkerState::Normal => write!(f, "Normal"),
            WorkerState::GracefulShutdown => write!(f, "GracefulShutdown"),
        }
    }
}
