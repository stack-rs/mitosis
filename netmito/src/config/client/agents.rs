use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    entity::state::AgentState,
    schema::{AgentsQueryReq, RegisterAgentReq},
};

#[derive(Serialize, Debug, Deserialize, Args, derive_more::From, Clone)]
pub struct AgentsArgs {
    #[command(subcommand)]
    pub command: AgentsCommands,
}

#[derive(Subcommand, Serialize, Debug, Deserialize, derive_more::From, Clone)]
pub enum AgentsCommands {
    /// Register a new agent and get a token
    Register(RegisterAgentArgs),
    /// Query agents subject to the filter
    Query(QueryAgentsArgs),
    /// Shutdown an agent
    Shutdown(ShutdownAgentArgs),
}

#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct RegisterAgentArgs {
    /// Tags for suite matching (e.g., gpu,linux)
    #[arg(short, long, num_args = 0.., value_delimiter = ',')]
    pub tags: Vec<String>,
    /// Labels for querying/filtering
    #[arg(short, long, num_args = 0.., value_delimiter = ',')]
    pub labels: Vec<String>,
    /// Groups to associate with this agent (agent gets Write role from these groups)
    #[arg(short, long, num_args = 0.., value_delimiter = ',')]
    pub groups: Vec<String>,
    /// Stable identifier for the machine (default: auto-detected from /etc/machine-id)
    #[arg(long)]
    pub machine_code: Option<String>,
}

impl From<RegisterAgentArgs> for RegisterAgentReq {
    fn from(args: RegisterAgentArgs) -> Self {
        Self {
            tags: args.tags.into_iter().collect(),
            labels: args.labels.into_iter().collect(),
            groups: args.groups.into_iter().collect(),
            lifetime: None,
            machine_code: args.machine_code,
        }
    }
}

#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct QueryAgentsArgs {
    /// Filter by group name
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
    /// Maximum number of results to return
    #[arg(long)]
    pub limit: Option<u64>,
    /// Number of results to skip (for pagination)
    #[arg(long)]
    pub offset: Option<u64>,
    /// Only count the number of matching agents
    #[arg(long)]
    pub count: bool,
    /// Show verbose agent information
    #[arg(short, long)]
    pub verbose: bool,
}

impl From<QueryAgentsArgs> for AgentsQueryReq {
    fn from(args: QueryAgentsArgs) -> Self {
        Self {
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
            creator_username: None,
            limit: args.limit,
            offset: args.offset,
            count: args.count,
        }
    }
}

#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct ShutdownAgentArgs {
    /// The UUID of the agent to shutdown
    pub uuid: Uuid,
    /// Force shutdown (interrupts running tasks immediately)
    #[arg(short, long)]
    pub force: bool,
}
