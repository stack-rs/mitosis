use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    entity::state::{SuiteJobState, TaskSuiteState},
    schema::{CreateTaskSuiteReq, SuiteJobsQueryReq, TaskSuitesQueryReq, WorkerSchedulePlan},
};

#[derive(Serialize, Debug, Deserialize, Args, derive_more::From, Clone)]
pub struct SuitesArgs {
    #[command(subcommand)]
    pub command: SuitesCommands,
}

#[derive(Subcommand, Serialize, Debug, Deserialize, Clone)]
pub enum SuitesCommands {
    /// Create a new task suite
    Create(CreateSuiteArgs),
    /// Query task suites subject to a filter
    Query(QuerySuitesArgs),
    /// Get the details of a task suite
    Get(GetSuiteArgs),
    /// Close a task suite (mark it idle; tasks still run and a new task reopens it)
    Close(GetSuiteArgs),
    /// Cancel a task suite (terminal; cancels its pending tasks)
    Cancel(CancelSuiteArgs),
    /// Set agent overrides on a suite in one batch: include, exclude, and/or clear agents
    Override(AgentsForSuiteOverrideArgs),
    /// Inspect the suite's jobs — one per agent attempt at running it
    Jobs(SuiteJobsArgs),
}

#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct CreateSuiteArgs {
    /// The group that owns this suite
    #[arg(short, long)]
    pub group: String,
    /// Optional human-readable name for the suite
    #[arg(short, long)]
    pub name: Option<String>,
    /// Optional description for the suite
    #[arg(short, long)]
    pub description: Option<String>,
    /// Tags for agent matching (e.g. gpu,linux)
    #[arg(short, long, num_args = 0.., value_delimiter = ',')]
    pub tags: Vec<String>,
    /// Labels for querying/filtering (e.g. project:foo,phase:bar)
    #[arg(short, long, num_args = 0.., value_delimiter = ',')]
    pub labels: Vec<String>,
    /// Suite scheduling priority (higher = more important)
    #[arg(short, long, default_value_t = 0)]
    pub priority: i32,
    /// Number of workers each agent spawns for this suite
    #[arg(short, long, default_value_t = 1)]
    pub workers: u32,
    /// Number of tasks each worker prefetches locally
    #[arg(long, default_value_t = 16)]
    pub prefetch: u32,
}

impl From<CreateSuiteArgs> for CreateTaskSuiteReq {
    fn from(args: CreateSuiteArgs) -> Self {
        Self {
            name: args.name,
            description: args.description,
            group_name: args.group,
            tags: args.tags.into_iter().collect(),
            labels: args.labels.into_iter().collect(),
            priority: args.priority,
            worker_schedule: WorkerSchedulePlan::FixedWorkers {
                worker_count: args.workers,
                cpu_binding: None,
                task_prefetch_count: args.prefetch,
            },
            exec_hooks: None,
        }
    }
}

#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct QuerySuitesArgs {
    /// Filter by group name (defaults to your username when omitted)
    #[arg(short, long)]
    pub group: Option<String>,
    /// Filter by exact suite name
    #[arg(long)]
    pub name: Option<String>,
    /// Filter by tags
    #[arg(short, long, num_args = 0.., value_delimiter = ',')]
    pub tags: Vec<String>,
    /// Filter by labels
    #[arg(short, long, num_args = 0.., value_delimiter = ',')]
    pub labels: Vec<String>,
    /// Filter by states
    #[arg(long, num_args = 0.., value_delimiter = ',')]
    pub states: Vec<TaskSuiteState>,
    /// Filter by priority (e.g. ">5", "<=10")
    #[arg(long)]
    pub priority: Option<String>,
    /// Maximum number of results to return
    #[arg(long)]
    pub limit: Option<u64>,
    /// Number of results to skip (for pagination)
    #[arg(long)]
    pub offset: Option<u64>,
    /// Only return the number of matching suites
    #[arg(long)]
    pub count: bool,
    /// Show verbose suite information
    #[arg(short, long)]
    pub verbose: bool,
}

impl From<QuerySuitesArgs> for TaskSuitesQueryReq {
    fn from(args: QuerySuitesArgs) -> Self {
        Self {
            name: args.name,
            description: None,
            creator_usernames: None,
            group_name: args.group,
            tags: (!args.tags.is_empty()).then(|| args.tags.into_iter().collect()),
            labels: (!args.labels.is_empty()).then(|| args.labels.into_iter().collect()),
            states: (!args.states.is_empty()).then(|| args.states.into_iter().collect()),
            priority: args.priority,
            limit: args.limit,
            offset: args.offset,
            count: args.count,
        }
    }
}

#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct GetSuiteArgs {
    /// The UUID of the suite
    pub uuid: Uuid,
}

#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct CancelSuiteArgs {
    /// The UUID of the suite to cancel
    pub uuid: Uuid,
    /// Force-cancel: running agents are killed without cleanup
    #[arg(short, long)]
    pub force: bool,
}

#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct AgentsForSuiteOverrideArgs {
    /// The UUID of the suite
    pub uuid: Uuid,
    /// Agents to manually include (pin them even if they do not tag-match)
    #[arg(long, num_args = 0.., value_delimiter = ',')]
    pub include: Vec<Uuid>,
    /// Agents to manually exclude (block them even if they tag-match)
    #[arg(long, num_args = 0.., value_delimiter = ',')]
    pub exclude: Vec<Uuid>,
    /// Agents to reset to the tag-match default (clear any manual include/exclude)
    #[arg(long, num_args = 0.., value_delimiter = ',')]
    pub clear: Vec<Uuid>,
}

#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct SuiteJobsArgs {
    /// The UUID of the suite
    pub uuid: Uuid,
    /// Show one job in full, including its hook executions
    #[arg(long)]
    pub job: Option<i32>,
    /// Filter the listing by job state
    #[arg(long, num_args = 0.., value_delimiter = ',')]
    pub states: Vec<SuiteJobState>,
    /// Only list jobs run by this agent
    #[arg(long)]
    pub agent: Option<Uuid>,
    /// Maximum number of jobs to list
    #[arg(long)]
    pub limit: Option<u64>,
    /// Number of jobs to skip (for pagination)
    #[arg(long)]
    pub offset: Option<u64>,
    /// Report the number of matching jobs instead of listing them
    #[arg(long)]
    pub count: bool,
}

impl From<&SuiteJobsArgs> for SuiteJobsQueryReq {
    fn from(args: &SuiteJobsArgs) -> Self {
        Self {
            states: (!args.states.is_empty()).then(|| args.states.iter().copied().collect()),
            agent_uuid: args.agent,
            limit: args.limit,
            offset: args.offset,
            count: args.count,
        }
    }
}
