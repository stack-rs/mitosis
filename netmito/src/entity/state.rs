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
    EnumIter,
    DeriveActiveEnum,
    Clone,
    Debug,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    Copy,
    ValueEnum,
    Hash,
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
                        (target_state, msg); TaskResultMessage::FetchResourceTimeout, TaskResultMessage::ExecTimeout, TaskResultMessage::UploadResultTimeout, TaskResultMessage::ResourceNotFound, TaskResultMessage::ResourceForbidden, TaskResultMessage::WatchTimeout, TaskResultMessage::UserCancellation, TaskResultMessage::SubmitNewTaskFailed =>
                        TaskExecState::WorkerExited             => false, false, false, false, false, false, false, false;
                        TaskExecState::FetchResource            => false, false, false, false, false, false, false, true;
                        TaskExecState::FetchResourceFinished    => false, false, false, false, false, false, false, true;
                        TaskExecState::FetchResourceError       => false, false, false, false, false, false, false, false;
                        TaskExecState::FetchResourceTimeout     => true,  false, false, false, false, false, false, false;
                        TaskExecState::FetchResourceNotFound    => false, false, false, true,  false, false, false, false;
                        TaskExecState::FetchResourceForbidden   => false, false, false, false, true,  false, false, false;
                        TaskExecState::Watch                    => false, false, false, false, false, false, false, true;
                        TaskExecState::WatchFinished            => false, false, false, false, false, false, false, true;
                        TaskExecState::WatchTimeout             => false, false, false, false, false, true,  false, false;
                        TaskExecState::ExecPending              => false, false, false, false, false, false, false, true;
                        TaskExecState::ExecSpawned              => false, false, false, false, false, false, false, true;
                        TaskExecState::ExecFinished             => false, false, false, false, false, false, false, true;
                        TaskExecState::ExecTimeout              => false, true,  false, false, false, false, false, false;
                        TaskExecState::UploadResult             => true,  true,  true,  true,  true,  true,  false, true;
                        TaskExecState::UploadFinishedResult     => false, false, false, false, false, false, false, false;
                        TaskExecState::UploadCancelledResult    => true,  true,  true,  true,  true,  true,  false, false;
                        TaskExecState::UploadResultFinished     => true,  true,  true,  true,  true,  true,  false, true;
                        TaskExecState::UploadResultTimeout      => false, false, true,  false, false, false, false, false;
                        TaskExecState::TaskCommitted            => true,  true,  true,  true,  true,  true,  true,  true;
                        TaskExecState::Unknown                  => false, false, false, false, false, false, false, false;
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

#[derive(
    EnumIter,
    DeriveActiveEnum,
    Clone,
    Debug,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    Copy,
    Hash,
    ValueEnum,
)]
#[sea_orm(rs_type = "i32", db_type = "Integer")]
pub enum TaskSuiteState {
    /// Suite is accepting new tasks
    Open = 0,
    /// No new tasks has been added to the suite in a recent time but tasks can still be executed;
    /// Adding a new task will transit the suite back to `open` state
    Closed = 1,
    /// All tasks in the suite have completed
    Complete = 2,
    /// Suite has been cancelled
    Cancelled = 3,
}

impl Display for TaskSuiteState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TaskSuiteState::Open => write!(f, "Open"),
            TaskSuiteState::Closed => write!(f, "Closed"),
            TaskSuiteState::Complete => write!(f, "Complete"),
            TaskSuiteState::Cancelled => write!(f, "Cancelled"),
        }
    }
}

impl TaskSuiteState {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Complete | Self::Cancelled)
    }

    /// Returns true if the suite can accept new tasks.
    /// - Open: directly accepts tasks
    /// - Complete: can accept tasks (will be reopened)
    /// - Closed: can accept tasks (will be reopened)
    /// - Cancelled: terminal state, cannot accept tasks
    pub fn can_accept_tasks(&self) -> bool {
        !matches!(self, Self::Cancelled)
    }

    /// Returns true if the suite allows its tasks to be executed.
    /// - Open, Closed: tasks keep running
    /// - Complete: nothing is left to run, but asking is not an error
    /// - Cancelled: terminal state, no further execution
    pub fn allows_task_execution(&self) -> bool {
        !matches!(self, Self::Cancelled)
    }

    // TODO: this method might be removed as we should do an idempotent update to state
    /// Returns true if the suite needs to be reopened before accepting tasks.
    /// This is true for Closed and Complete states.
    pub fn needs_reopen(&self) -> bool {
        matches!(self, Self::Closed | Self::Complete)
    }

    pub fn is_closed(&self) -> bool {
        matches!(self, Self::Closed | Self::Complete | Self::Cancelled)
    }
}

// TODO: should get further check on what states are needed
/// Runtime phase of an agent.
#[derive(
    EnumIter,
    DeriveActiveEnum,
    Clone,
    Debug,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    Copy,
    ValueEnum,
)]
#[sea_orm(rs_type = "i32", db_type = "Integer")]
pub enum AgentState {
    /// Agent is idle and available for assignment
    Idle = 0,
    /// Agent is provisioning environment for task suite
    Provisioning = 1,
    /// Agent is executing tasks from a suite
    Executing = 2,
    /// Agent is cleaning up after task suite completion or asked to gracefully shut down/handover
    Cleaning = 3,
    /// Agent is offline
    Offline = 4,
}

impl Display for AgentState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentState::Idle => write!(f, "Idle"),
            AgentState::Provisioning => write!(f, "Provisioning"),
            AgentState::Executing => write!(f, "Executing"),
            AgentState::Cleaning => write!(f, "Cleaning"),
            AgentState::Offline => write!(f, "Offline"),
        }
    }
}

impl AgentState {
    pub fn is_available(&self) -> bool {
        matches!(self, Self::Idle)
    }

    pub fn is_busy(&self) -> bool {
        matches!(self, Self::Provisioning | Self::Executing | Self::Cleaning)
    }
}

/// Lifecycle state of a hook task (provision / cleanup / background).
/// A hook task is reported on completion, so There is no `Running` state
#[derive(EnumIter, DeriveActiveEnum, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Copy)]
#[sea_orm(rs_type = "i32", db_type = "Integer")]
pub enum HookExecState {
    /// Hook completed successfully
    Completed = 0,
    /// Hook failed (the return value of the program is none-zero)
    Failed = 1,
    /// Hook was cancelled
    Cancelled = 2,
}

impl Display for HookExecState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HookExecState::Completed => write!(f, "Completed"),
            HookExecState::Failed => write!(f, "Failed"),
            HookExecState::Cancelled => write!(f, "Cancelled"),
        }
    }
}

/// Lifecycle state of one job: an attempt of an agent running a task suite.
///
/// A job that is gracefully stopped will not execute any new task,
/// will ask tasks still executing to gracefully shut down, and will still
/// run the cleanup hook and report the hook execution result.
///
/// A job that is forcefully stopped will not execute any new task,
/// will ask tasks still executing to forcefully shut down, and will NOT run
/// any cleanup hook.
///
/// A task's execution result is only accepted while the job is `Executing`
/// (see `accepts_task_result`); the agent must drain all pending task
/// reports before the job leaves that state. Reports against any other
/// state are rejected.
///
#[derive(
    EnumIter,
    DeriveActiveEnum,
    Clone,
    Debug,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    Copy,
    ValueEnum,
)]
#[sea_orm(rs_type = "i32", db_type = "Integer")]
pub enum SuiteJobState {
    /// Job accepted, provision hook running
    Provisioning = 0,
    /// Tasks being executed
    Executing = 1,
    /// Cleanup hook running
    Cleanup = 2,
    /// Terminal: job finished successfully,
    /// It doesn't matter if the job finished all the available tasks in the suite. As long as
    /// all hooks completed successfully, the job is marked completed.
    Completed = 3,
    /// Terminal: a hook execution failed. Job terminated, get another suite to execute after.
    Failed = 4,
    /// Terminal: the agent was lost while executing this job.
    Lost = 5,
    /// Terminal: job was force stopped, no cleanup
    Killed = 6,
}

impl Display for SuiteJobState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SuiteJobState::Provisioning => write!(f, "Provision"),
            SuiteJobState::Executing => write!(f, "Executing"),
            SuiteJobState::Cleanup => write!(f, "Cleanup"),
            SuiteJobState::Completed => write!(f, "Completed"),
            SuiteJobState::Failed => write!(f, "Failed"),
            SuiteJobState::Lost => write!(f, "Lost"),
            SuiteJobState::Killed => write!(f, "Killed"),
        }
    }
}

impl SuiteJobState {
    /// Whether the job is still running one of its lifecycle phases
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Provisioning | Self::Executing | Self::Cleanup)
    }

    pub fn is_terminal(&self) -> bool {
        !self.is_active()
    }

    /// Whether a task's execution result may be recorded in this job state.
    pub fn accepts_task_result(&self) -> bool {
        matches!(self, Self::Executing)
    }
}
