use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    entity::state::TaskSuiteState,
    schema::{
        AddSuiteAgentsReq, CreateTaskSuiteReq, RemoveSuiteAgentsReq, TaskSuitesQueryReq,
        WorkerSchedulePlan,
    },
};

#[derive(Serialize, Debug, Deserialize, Args, derive_more::From, Clone)]
pub struct SuitesArgs {
    #[command(subcommand)]
    pub command: SuitesCommands,
}

#[derive(Subcommand, Serialize, Debug, Deserialize, derive_more::From, Clone)]
pub enum SuitesCommands {
    /// Create a new task suite
    Create(CreateSuiteArgs),
    /// Query task suites subject to the filter
    Query(QuerySuitesArgs),
    /// Get details of a task suite
    Get(GetSuiteArgs),
    /// Cancel a task suite (cancels pending tasks)
    Cancel(CancelSuiteArgs),
    /// Close a task suite (stop accepting new tasks, existing tasks still run)
    Close(CloseSuiteArgs),
    /// Refresh agents assigned to a task suite (re-run tag matching)
    RefreshAgents(RefreshSuiteAgentsArgs),
    /// Manually add agents to a task suite
    AddAgents(AddSuiteAgentsArgs),
    /// Remove agents from a task suite
    RemoveAgents(RemoveSuiteAgentsArgs),
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
    /// Tags for agent matching (e.g., gpu,linux)
    #[arg(short, long, num_args = 0.., value_delimiter = ',')]
    pub tags: Vec<String>,
    /// Labels for querying/filtering (e.g., project:foo,phase:bar)
    #[arg(short, long, num_args = 0.., value_delimiter = ',')]
    pub labels: Vec<String>,
    /// Suite scheduling priority (higher = more important)
    #[arg(short, long, default_value_t = 0)]
    pub priority: i32,
    /// Number of workers to spawn per agent
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
    /// Filter by group name
    #[arg(short, long)]
    pub group: Option<String>,
    /// Filter by suite name (exact match)
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
    /// Maximum number of results to return
    #[arg(long)]
    pub limit: Option<u64>,
    /// Number of results to skip (for pagination)
    #[arg(long)]
    pub offset: Option<u64>,
    /// Only count the number of matching suites
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
            states: if args.states.is_empty() {
                None
            } else {
                Some(args.states.into_iter().collect())
            },
            priority: None,
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
    /// Force-cancel (also cancels tasks currently running on agents)
    #[arg(short, long)]
    pub force: bool,
}

#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct CloseSuiteArgs {
    /// The UUID of the suite to close
    pub uuid: Uuid,
}

#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct RefreshSuiteAgentsArgs {
    /// The UUID of the suite
    pub uuid: Uuid,
}

#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct AddSuiteAgentsArgs {
    /// The UUID of the suite
    pub uuid: Uuid,
    /// UUIDs of agents to add (space-separated or comma-separated)
    #[arg(num_args = 1.., value_delimiter = ',')]
    pub agent_uuids: Vec<Uuid>,
}

impl From<AddSuiteAgentsArgs> for AddSuiteAgentsReq {
    fn from(args: AddSuiteAgentsArgs) -> Self {
        Self {
            agent_uuids: args.agent_uuids,
        }
    }
}

#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct RemoveSuiteAgentsArgs {
    /// The UUID of the suite
    pub uuid: Uuid,
    /// UUIDs of agents to remove (space-separated or comma-separated)
    #[arg(num_args = 1.., value_delimiter = ',')]
    pub agent_uuids: Vec<Uuid>,
}

impl From<RemoveSuiteAgentsArgs> for RemoveSuiteAgentsReq {
    fn from(args: RemoveSuiteAgentsArgs) -> Self {
        Self {
            agent_uuids: args.agent_uuids,
        }
    }
}
