use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{entity::state::AgentState, schema::AgentsQueryReq};

#[derive(Serialize, Debug, Deserialize, Args, derive_more::From, Clone)]
pub struct AgentsArgs {
    #[command(subcommand)]
    pub command: AgentsCommands,
}

#[derive(Subcommand, Serialize, Debug, Deserialize, Clone)]
pub enum AgentsCommands {
    /// Query agents subject to a filter
    Query(QueryAgentsArgs),
    /// Shut an agent down (the agent record itself is kept)
    Shutdown(ShutdownAgentArgs),
    /// Stop the job an agent is running now, sending it back to pick a suite again
    StopJob(StopAgentJobArgs),
}

#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct QueryAgentsArgs {
    /// Filter by group name (defaults to your username when omitted)
    #[arg(short, long)]
    pub group: Option<String>,
    /// Filter by tags
    #[arg(short, long, num_args = 0.., value_delimiter = ',')]
    pub tags: Vec<String>,
    /// Filter by labels
    #[arg(short, long, num_args = 0.., value_delimiter = ',')]
    pub labels: Vec<String>,
    /// Filter by states
    #[arg(long, num_args = 0.., value_delimiter = ',')]
    pub states: Vec<AgentState>,
    /// Filter by creator username
    #[arg(long)]
    pub creator: Option<String>,
    /// Maximum number of results to return
    #[arg(long)]
    pub limit: Option<u64>,
    /// Number of results to skip (for pagination)
    #[arg(long)]
    pub offset: Option<u64>,
    /// Only return the number of matching agents
    #[arg(long)]
    pub count: bool,
}

impl From<QueryAgentsArgs> for AgentsQueryReq {
    fn from(args: QueryAgentsArgs) -> Self {
        Self {
            group_name: args.group,
            tags: (!args.tags.is_empty()).then(|| args.tags.into_iter().collect()),
            labels: (!args.labels.is_empty()).then(|| args.labels.into_iter().collect()),
            states: (!args.states.is_empty()).then(|| args.states.into_iter().collect()),
            creator_username: args.creator,
            limit: args.limit,
            offset: args.offset,
            count: args.count,
        }
    }
}

#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct ShutdownAgentArgs {
    /// The UUID of the agent
    pub uuid: Uuid,
    /// Stop now: the agent's in-flight job is killed without cleanup and its
    /// uncommitted tasks are reclaimed. Without this the agent finishes the
    /// tasks it is running, cleans up, and then stops.
    #[arg(short, long)]
    pub force: bool,
}

/// Stopping a job is how you preempt an agent by hand: it winds the job down and
/// immediately picks the highest-priority suite on offer, which may be the one it
/// was already running — a way to force a re-provision.
#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct StopAgentJobArgs {
    /// The UUID of the agent
    pub uuid: Uuid,
    /// Stop now: the job is killed without cleanup and its uncommitted tasks are
    /// reclaimed. Without this the agent finishes the tasks it is running,
    /// commits them, and cleans up first.
    #[arg(short, long)]
    pub force: bool,
}
