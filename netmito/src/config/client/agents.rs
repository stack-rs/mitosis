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
    /// Shut an agent down. The agent record stays.
    Shutdown(ShutdownAgentArgs),
    /// Stop the job that an agent runs now. The agent then picks a suite again
    StopJob(StopAgentJobArgs),
}

#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct QueryAgentsArgs {
    /// Filter by group name. The default is your username.
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
    /// Number of results to skip
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
    /// Mark the agent offline now. Without this flag, the agent finishes its
    /// current work and then stops.
    #[arg(short, long)]
    pub force: bool,
}

/// A stop moves an agent by hand: the agent ends the job and picks the suite with the highest
/// priority now. This can be the same suite, then the agent provisions it again.
#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct StopAgentJobArgs {
    /// The UUID of the agent
    pub uuid: Uuid,
    /// Stop now: the job is killed without cleanup, and its tasks that are not committed go back to
    /// Ready. Without this flag, the agent finishes and commits its running tasks, and runs the
    /// cleanup hook first.
    #[arg(short, long)]
    pub force: bool,
}
