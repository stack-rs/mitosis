use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    entity::role::GroupWorkerRole,
    schema::{
        RemoveGroupWorkerRoleReq, ReplaceWorkerTagsReq, UpdateGroupWorkerRoleReq, WorkersQueryReq,
    },
};

use super::parse_key_val_colon;

#[derive(Serialize, Debug, Deserialize, Args, derive_more::From, Clone)]
pub struct WorkersArgs {
    #[command(subcommand)]
    pub command: WorkersCommands,
}

#[derive(Subcommand, Serialize, Debug, Deserialize, derive_more::From, Clone)]
pub enum WorkersCommands {
    /// Cancel a worker
    Cancel(CancelWorkerArgs),
    /// Replace tags of a worker
    UpdateTags(WorkerUpdateTagsArgs),
    /// Update the roles of groups to a worker
    UpdateRoles(UpdateWorkerGroupArgs),
    /// Remove the accessibility of groups from a worker
    RemoveRoles(RemoveWorkerGroupArgs),
    /// Get information about a worker
    Get(GetWorkerArgs),
    /// Query workers subject to the filter
    Query(WorkersQueryArgs),
}

#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct CancelWorkerArgs {
    /// The UUID of the worker
    pub uuid: Uuid,
    /// Whether to force the worker to shutdown.
    /// If not specified, the worker will be try to shutdown gracefully
    #[arg(short, long)]
    pub force: bool,
}

#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct WorkerUpdateTagsArgs {
    /// The UUID of the worker
    pub uuid: Uuid,
    /// The tags to replace
    #[arg(num_args = 0..)]
    pub tags: Vec<String>,
}

#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct UpdateWorkerGroupArgs {
    /// The UUID of the worker
    pub uuid: Uuid,
    /// The name and role of the group on the worker to update
    #[arg(num_args = 0.., value_parser = parse_key_val_colon::<String, GroupWorkerRole>)]
    pub roles: Vec<(String, GroupWorkerRole)>,
}

#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct RemoveWorkerGroupArgs {
    /// The UUID of the worker
    pub uuid: Uuid,
    /// The name of the group to update
    #[arg(num_args = 0..)]
    pub groups: Vec<String>,
}

#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct GetWorkerArgs {
    /// The UUID of the worker
    pub uuid: Uuid,
}

#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct WorkersQueryArgs {
    /// The name of the group has access to the workers
    #[arg(short, long)]
    pub group: Option<String>,
    /// The role of the group on the workers
    #[arg(short, long, num_args = 0.., value_delimiter = ',')]
    pub role: Vec<GroupWorkerRole>,
    /// The tags of the workers
    #[arg(short, long, num_args = 0.., value_delimiter = ',')]
    pub tags: Vec<String>,
    /// The username of the creator
    #[arg(long)]
    pub creator: Option<String>,
    /// Whether to output the verbose information of workers
    #[arg(short, long)]
    pub verbose: bool,
    /// Only count the number of workers
    #[arg(long)]
    pub count: bool,
}

impl From<WorkersQueryArgs> for WorkersQueryReq {
    fn from(args: WorkersQueryArgs) -> Self {
        Self {
            group_name: args.group,
            role: if args.role.is_empty() {
                None
            } else {
                Some(args.role.into_iter().collect())
            },
            tags: if args.tags.is_empty() {
                None
            } else {
                Some(args.tags.into_iter().collect())
            },
            creator_username: args.creator,
            count: args.count,
        }
    }
}

impl From<UpdateWorkerGroupArgs> for UpdateGroupWorkerRoleReq {
    fn from(args: UpdateWorkerGroupArgs) -> Self {
        Self {
            relations: args.roles.into_iter().collect(),
        }
    }
}

impl From<WorkerUpdateTagsArgs> for ReplaceWorkerTagsReq {
    fn from(args: WorkerUpdateTagsArgs) -> Self {
        Self {
            tags: args.tags.into_iter().collect(),
        }
    }
}

impl From<RemoveWorkerGroupArgs> for RemoveGroupWorkerRoleReq {
    fn from(args: RemoveWorkerGroupArgs) -> Self {
        Self {
            groups: args.groups.into_iter().collect(),
        }
    }
}
