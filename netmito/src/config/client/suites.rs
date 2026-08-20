use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    entity::state::{SuiteJobState, TaskSuiteState},
    schema::{
        CreateTaskSuiteReq, ExecHooks, ExecSpec, SuiteJobsQueryReq, TaskSuitesQueryReq,
        WorkerSchedulePlan,
    },
};

use super::parse_exec_spec;

#[derive(Serialize, Debug, Deserialize, Args, derive_more::From, Clone)]
pub struct SuitesArgs {
    #[command(subcommand)]
    pub command: SuitesCommands,
}

#[derive(Subcommand, Serialize, Debug, Deserialize, Clone)]
pub enum SuitesCommands {
    /// Create a new task suite
    // Boxed: the three optional hook specs make this variant far larger than the rest.
    Create(Box<CreateSuiteArgs>),
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
    /// Stop one of the suite's jobs, sending its agent back to pick a suite again
    StopJob(StopSuiteJobArgs),
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
    /// Claim a task only when a worker is free, instead of keeping the next
    /// one ready. Costs a round trip per task; for long tasks only
    #[arg(long, default_value_t = false)]
    pub no_prefetch: bool,
    /// Provision hook, as a JSON exec spec: '{"args":["sh","-c","./setup.sh"],"terminal_output":true}'.
    /// Runs once before any task; a non-zero exit fails the job and its tasks never start
    #[arg(long, value_parser = parse_exec_spec)]
    pub provision: Option<ExecSpec>,
    /// Cleanup hook, same JSON shape. Runs once after the tasks drain, even when the job already failed
    #[arg(long, value_parser = parse_exec_spec)]
    pub cleanup: Option<ExecSpec>,
    /// Background hook, same JSON shape. Runs alongside the tasks and is expected to outlive them
    #[arg(long, value_parser = parse_exec_spec)]
    pub background: Option<ExecSpec>,
}

impl From<CreateSuiteArgs> for CreateTaskSuiteReq {
    fn from(args: CreateSuiteArgs) -> Self {
        // A suite with no hook at all keeps `exec_hooks` absent rather than
        // carrying three nulls.
        let exec_hooks = match (args.provision, args.cleanup, args.background) {
            (None, None, None) => None,
            (provision, cleanup, background) => Some(ExecHooks {
                provision,
                cleanup,
                background,
            }),
        };
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
                prefetch: !args.no_prefetch,
            },
            exec_hooks,
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

/// The suite-side twin of `agents stop-job`: same wind-down, addressed by job
/// number and authorized on the suite's group. The agent may re-accept this very
/// suite, so it stops the *job*, it does not keep the agent away.
#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct StopSuiteJobArgs {
    /// The UUID of the suite
    pub uuid: Uuid,
    /// The suite's own job number, as shown by `suites jobs`
    #[arg(long)]
    pub job: i32,
    /// Stop now: the job is killed without cleanup and its uncommitted tasks are
    /// reclaimed. Without this the agent finishes the tasks it is running,
    /// commits them, and cleans up first.
    #[arg(short, long)]
    pub force: bool,
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
