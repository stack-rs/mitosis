use std::collections::HashSet;

use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    entity::state::TaskSuiteState,
    schema::{
        ChangeTaskSuiteReq, CreateTaskSuiteReq, ExecHooks, ExecHooksUpdate, ExecSpec,
        TaskSuitesQueryReq, UpdateOp, WorkerSchedulePlan,
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
    // Put the arguments in a box: the three optional hook specs make this variant much larger
    // than the other variants.
    Create(Box<CreateSuiteArgs>),
    /// Change the properties of a suite. The change applies to the jobs that start after it
    Change(Box<ChangeSuiteArgs>),
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
    /// The provision hook, as a JSON exec spec, for example
    /// '{"args":["sh","-c","./setup.sh"],"terminal_output":true}'. It runs one time before the
    /// tasks. If it exits with a non-zero code, the job fails and its tasks do not start
    #[arg(long, value_parser = parse_exec_spec)]
    pub provision: Option<ExecSpec>,
    /// The cleanup hook, with the same JSON shape. It runs one time after the last task of the
    /// job ends. It also runs if the job failed
    #[arg(long, value_parser = parse_exec_spec)]
    pub cleanup: Option<ExecSpec>,
    /// The background hook, with the same JSON shape. It runs at the same time as the tasks, and
    /// usually continues to run until the job ends
    #[arg(long, value_parser = parse_exec_spec)]
    pub background: Option<ExecSpec>,
}

impl From<CreateSuiteArgs> for CreateTaskSuiteReq {
    fn from(args: CreateSuiteArgs) -> Self {
        // If no hook is given, keep `exec_hooks` absent. Do not send an object with three nulls.
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
                task_prefetch_count: args.prefetch,
            },
            exec_hooks,
        }
    }
}

/// A field does not change if you do not give its flag. Each nullable field has a `--clear-*`
/// flag that conflicts with its value flag. If you set and clear the same field, clap shows a
/// parse error, and the client does not send the request.
#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct ChangeSuiteArgs {
    /// The UUID of the suite
    pub uuid: Uuid,
    /// The new name of the suite
    #[arg(short, long, conflicts_with = "clear_name")]
    pub name: Option<String>,
    /// Remove the name of the suite
    #[arg(long)]
    pub clear_name: bool,
    /// The new description of the suite
    #[arg(short, long, conflicts_with = "clear_description")]
    pub description: Option<String>,
    /// Remove the description of the suite
    #[arg(long)]
    pub clear_description: bool,
    /// The new tags that an agent must have to run the suite (for example, gpu,linux)
    #[arg(short, long, num_args = 1.., value_delimiter = ',', conflicts_with = "clear_tags")]
    pub tags: Option<Vec<String>>,
    /// Remove all tags. A suite with no tags matches all agents that its group can write to
    #[arg(long)]
    pub clear_tags: bool,
    /// The new labels, which you can use to query and filter suites
    #[arg(short, long, num_args = 1.., value_delimiter = ',', conflicts_with = "clear_labels")]
    pub labels: Option<Vec<String>>,
    /// Remove all labels
    #[arg(long)]
    pub clear_labels: bool,
    /// The scheduling priority of the suite. A higher value is more important
    #[arg(short, long)]
    pub priority: Option<i32>,
    /// The number of workers that each agent starts for this suite. You must also give
    /// `--prefetch`
    #[arg(short, long, requires = "prefetch")]
    pub workers: Option<u32>,
    /// The number of tasks that each worker gets before it needs them. You must also give
    /// `--workers`
    #[arg(long, requires = "workers")]
    pub prefetch: Option<u32>,
    /// The new provision hook, as a JSON exec spec
    #[arg(long, value_parser = parse_exec_spec, conflicts_with = "clear_provision")]
    pub provision: Option<ExecSpec>,
    /// Remove the provision hook
    #[arg(long)]
    pub clear_provision: bool,
    /// The new cleanup hook, with the same JSON shape
    #[arg(long, value_parser = parse_exec_spec, conflicts_with = "clear_cleanup")]
    pub cleanup: Option<ExecSpec>,
    /// Remove the cleanup hook
    #[arg(long)]
    pub clear_cleanup: bool,
    /// The new background hook, with the same JSON shape
    #[arg(long, value_parser = parse_exec_spec, conflicts_with = "clear_background")]
    pub background: Option<ExecSpec>,
    /// Remove the background hook
    #[arg(long)]
    pub clear_background: bool,
}

/// Convert a value flag and its `--clear-*` flag into one `UpdateOp`. clap does not accept the
/// two flags together. If the two flags arrive, the value is used.
fn update_op<T>(value: Option<T>, clear: bool) -> Option<UpdateOp<T>> {
    match (value, clear) {
        (Some(value), _) => Some(UpdateOp::SetTo(value)),
        (None, true) => Some(UpdateOp::SetNull),
        (None, false) => None,
    }
}

impl From<ChangeSuiteArgs> for ChangeTaskSuiteReq {
    fn from(args: ChangeSuiteArgs) -> Self {
        // An array column cannot be null, so its cleared value is an empty set. The CLI uses the
        // same flag pair for all fields, but the request is different for these two fields.
        let tags = match (args.tags, args.clear_tags) {
            (Some(tags), _) => Some(tags.into_iter().collect()),
            (None, true) => Some(HashSet::new()),
            (None, false) => None,
        };
        let labels = match (args.labels, args.clear_labels) {
            (Some(labels), _) => Some(labels.into_iter().collect()),
            (None, true) => Some(HashSet::new()),
            (None, false) => None,
        };
        // The request replaces the full plan, so it must contain the full plan or no plan. clap
        // requires the two flags together. A missing flag must not get a default value that
        // replaces the current value of the suite.
        let worker_schedule = match (args.workers, args.prefetch) {
            (Some(worker_count), Some(prefetch)) => Some(WorkerSchedulePlan::FixedWorkers {
                worker_count,
                cpu_binding: None,
                task_prefetch_count: prefetch,
            }),
            _ => None,
        };
        let hooks = ExecHooksUpdate {
            provision: update_op(args.provision, args.clear_provision),
            cleanup: update_op(args.cleanup, args.clear_cleanup),
            background: update_op(args.background, args.clear_background),
        };
        Self {
            name: update_op(args.name, args.clear_name),
            description: update_op(args.description, args.clear_description),
            tags,
            labels,
            priority: args.priority,
            worker_schedule,
            exec_hooks: (!hooks.is_empty()).then_some(hooks),
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
