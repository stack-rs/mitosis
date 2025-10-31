use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    config::client::parse_resources,
    entity::state::{TaskExecState, TaskState},
    schema::{
        ChangeTaskReq, RemoteResourceDownload, TaskSpec, TasksCancelByFilterReq,
        TasksCancelByUuidsReq, TasksQueryReq, UpdateTaskLabelsReq,
    },
};

use super::{parse_key_val, parse_watch_task, ArtifactsArgs};

#[derive(Serialize, Debug, Deserialize, Args, derive_more::From, Clone)]
pub struct TasksArgs {
    #[command(subcommand)]
    pub command: TasksCommands,
}

#[derive(Subcommand, Serialize, Debug, Deserialize, derive_more::From, Clone)]
pub enum TasksCommands {
    /// Submit a task
    Submit(SubmitTaskArgs),
    /// Get the info of a task
    Get(GetTaskArgs),
    /// Query tasks subject to the filter
    Query(QueryTasksArgs),
    /// Cancel a task
    Cancel(CancelTaskArgs),
    /// Cancel multiple tasks subject to the filter
    CancelMany(CancelTasksArgs),
    /// Cancel multiple tasks by UUIDs
    CancelList(CancelTasksByUuidsArgs),
    /// Replace labels of a task
    UpdateLabels(UpdateTaskLabelsArgs),
    /// Update the spec of a task
    Change(ChangeTaskArgs),
    /// Query, upload, download or delete an artifact
    Artifacts(ArtifactsArgs),
}

#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct SubmitTaskArgs {
    /// The name of the group this task is submitted to
    #[arg(short = 'g', long = "group")]
    pub group_name: Option<String>,
    /// The tags of the task, used to filter workers to execute the task
    #[arg(short, long, num_args = 0.., value_delimiter = ',')]
    pub tags: Vec<String>,
    /// The labels of the task, used for querying tasks
    #[arg(short, long, num_args = 0.., value_delimiter = ',')]
    pub labels: Vec<String>,
    /// The timeout of the task.
    #[arg(long, value_parser = humantime_serde::re::humantime::parse_duration, default_value="10min")]
    pub timeout: std::time::Duration,
    /// The priority of the task.
    #[arg(short, long, default_value_t = 0)]
    pub priority: i32,
    /// The environment variables to set
    #[arg(short, long, num_args = 0.., value_delimiter = ',', value_parser = parse_key_val::<String, String>)]
    pub envs: Vec<(String, String)>,
    /// Whether to collect the terminal standard output and error of the executed task.
    #[arg(long = "terminal")]
    pub terminal_output: bool,
    /// The command to run
    #[arg(last = true)]
    pub command: Vec<String>,
    #[arg(short, long, num_args = 0.., value_delimiter = ',', value_parser = parse_resources)]
    pub resources: Vec<RemoteResourceDownload>,
    /// The UUID and the state of the task to watch before triggering this task.
    /// Should specify it as `UUID,STATE`, e.g. `123e4567-e89b-12d3-a456-426614174000,ExecSpawned`.
    #[arg(long, value_parser = parse_watch_task::<Uuid, TaskExecState>)]
    pub watch: Option<(Uuid, TaskExecState)>,
}

#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct GetTaskArgs {
    /// The UUID of the task
    pub uuid: Uuid,
}

#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct QueryTasksArgs {
    /// The username of the creator who submitted the tasks
    #[arg(short, long, num_args = 0.., value_delimiter = ',')]
    pub creators: Vec<String>,
    /// The name of the group the tasks belong to
    #[arg(short, long)]
    pub group: Option<String>,
    /// The tags of the tasks
    #[arg(short, long, num_args = 0.., value_delimiter = ',')]
    pub tags: Vec<String>,
    /// The labels of the tasks
    #[arg(short, long, num_args = 0.., value_delimiter = ',')]
    pub labels: Vec<String>,
    /// The state of the tasks
    #[arg(short, long, num_args = 0.., value_delimiter = ',')]
    pub state: Vec<TaskState>,
    /// The exit status of the tasks, support operators like `=`(default), `!=`, `<`, `<=`, `>`, `>=`
    #[arg(short, long)]
    pub exit_status: Option<String>,
    /// The priority of the tasks, support operators like `=`(default), `!=`, `<`, `<=`, `>`, `>=`
    #[arg(short, long)]
    pub priority: Option<String>,
    /// The limit of the tasks to query
    #[arg(long)]
    pub limit: Option<u64>,
    /// The offset of the tasks to query
    #[arg(long)]
    pub offset: Option<u64>,
    /// Whether to output the verbose information of workers
    #[arg(short, long)]
    pub verbose: bool,
    /// Only count the number of workers
    #[arg(long)]
    pub count: bool,
}

#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct CancelTaskArgs {
    /// The UUID of the task
    pub uuid: Uuid,
}

#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct CancelTasksArgs {
    /// The username of the creator who submitted the tasks
    #[arg(short, long, num_args = 0.., value_delimiter = ',')]
    pub creators: Vec<String>,
    /// The name of the group the tasks belong to
    #[arg(short, long)]
    pub group: Option<String>,
    /// The tags of the tasks
    #[arg(short, long, num_args = 0.., value_delimiter = ',')]
    pub tags: Vec<String>,
    /// The labels of the tasks
    #[arg(short, long, num_args = 0.., value_delimiter = ',')]
    pub labels: Vec<String>,
    /// The state of the tasks
    #[arg(short, long, num_args = 0.., value_delimiter = ',')]
    pub state: Vec<TaskState>,
    /// The exit status of the tasks, support operators like `=`(default), `!=`, `<`, `<=`, `>`, `>=`
    #[arg(short, long)]
    pub exit_status: Option<String>,
    /// The priority of the tasks, support operators like `=`(default), `!=`, `<`, `<=`, `>`, `>=`
    #[arg(short, long)]
    pub priority: Option<String>,
}

#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct CancelTasksByUuidsArgs {
    /// The UUIDs of the tasks to cancel
    #[arg(num_args = 1.., value_delimiter = ',')]
    pub uuids: Vec<Uuid>,
}

#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct UpdateTaskLabelsArgs {
    /// The UUID of the task
    pub uuid: Uuid,
    /// The labels to replace
    #[arg(num_args = 0..)]
    pub labels: Vec<String>,
}

#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct ChangeTaskArgs {
    /// The UUID of the task
    pub uuid: Uuid,
    /// The tags of the task, used to filter workers to execute the task
    #[arg(short, long, num_args = 0.., value_delimiter = ',')]
    pub tags: Vec<String>,
    /// The timeout of the task.
    #[arg(long, value_parser = humantime_serde::re::humantime::parse_duration)]
    pub timeout: Option<std::time::Duration>,
    /// The priority of the task.
    #[arg(short, long)]
    pub priority: Option<i32>,
    /// The environment variables to set
    #[arg(short, long, num_args = 0.., value_delimiter = ',', value_parser = parse_key_val::<String, String>)]
    pub envs: Vec<(String, String)>,
    /// Whether to collect the terminal standard output and error of the executed task.
    #[arg(long = "terminal")]
    pub terminal_output: bool,
    /// The command to run
    #[arg(last = true)]
    pub command: Vec<String>,
    #[arg(skip)]
    pub resources: Vec<RemoteResourceDownload>,
    /// The UUID and the state of the task to watch before triggering this task.
    /// Should specify it as `UUID,STATE`, e.g. `123e4567-e89b-12d3-a456-426614174000,ExecSpawned`.
    #[arg(long, value_parser = parse_watch_task::<Uuid, TaskExecState>)]
    pub watch: Option<(Uuid, TaskExecState)>,
}

impl From<QueryTasksArgs> for TasksQueryReq {
    fn from(args: QueryTasksArgs) -> Self {
        Self {
            creator_usernames: if args.creators.is_empty() {
                None
            } else {
                Some(args.creators.into_iter().collect())
            },
            group_name: args.group,
            tags: if args.tags.is_empty() {
                None
            } else {
                Some(args.tags.into_iter().collect())
            },
            labels: if args.labels.is_empty() {
                None
            } else {
                Some(args.labels.into_iter().collect())
            },
            states: if args.state.is_empty() {
                None
            } else {
                Some(args.state.into_iter().collect())
            },
            exit_status: args.exit_status,
            priority: args.priority,
            limit: args.limit,
            offset: args.offset,
            count: args.count,
        }
    }
}

impl From<ChangeTaskArgs> for ChangeTaskReq {
    fn from(args: ChangeTaskArgs) -> Self {
        let task_spec = if args.command.is_empty() {
            None
        } else {
            Some(TaskSpec::new(
                args.command,
                args.envs,
                args.resources,
                args.terminal_output,
                args.watch,
            ))
        };
        Self {
            tags: if args.tags.is_empty() {
                None
            } else {
                Some(args.tags.into_iter().collect())
            },
            timeout: args.timeout,
            priority: args.priority,
            task_spec,
        }
    }
}

impl From<UpdateTaskLabelsArgs> for UpdateTaskLabelsReq {
    fn from(args: UpdateTaskLabelsArgs) -> Self {
        Self {
            labels: args.labels.into_iter().collect(),
        }
    }
}

impl From<CancelTasksArgs> for TasksCancelByFilterReq {
    fn from(args: CancelTasksArgs) -> Self {
        Self {
            creator_usernames: if args.creators.is_empty() {
                None
            } else {
                Some(args.creators.into_iter().collect())
            },
            group_name: args.group,
            tags: if args.tags.is_empty() {
                None
            } else {
                Some(args.tags.into_iter().collect())
            },
            labels: if args.labels.is_empty() {
                None
            } else {
                Some(args.labels.into_iter().collect())
            },
            states: if args.state.is_empty() {
                None
            } else {
                Some(args.state.into_iter().collect())
            },
            exit_status: args.exit_status,
            priority: args.priority,
        }
    }
}

impl From<CancelTasksByUuidsArgs> for TasksCancelByUuidsReq {
    fn from(args: CancelTasksByUuidsArgs) -> Self {
        Self { uuids: args.uuids }
    }
}
