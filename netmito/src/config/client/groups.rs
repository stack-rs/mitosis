use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};

use crate::entity::role::UserGroupRole;

use super::{parse_key_val_colon, AttachmentsArgs};

#[derive(Serialize, Debug, Deserialize, Args, derive_more::From)]
pub struct GroupsArgs {
    #[command(subcommand)]
    pub command: GroupsCommands,
}

#[derive(Subcommand, Serialize, Debug, Deserialize, derive_more::From)]
pub enum GroupsCommands {
    /// Create a new group
    Create(CreateGroupArgs),
    /// Get the information of a group
    Get(GetGroupArgs),
    /// Update the roles of users to a group
    UpdateUser(UpdateUserGroupArgs),
    /// Remove the accessibility of users from a group
    RemoveUser(RemoveUserGroupArgs),
    /// Query, upload, download or delete an attachment
    Attachments(AttachmentsArgs),
}

#[derive(Serialize, Debug, Deserialize, Args)]
pub struct CreateGroupArgs {
    /// The name of the group
    pub group: String,
}

#[derive(Serialize, Debug, Deserialize, Args)]
pub struct GetGroupArgs {
    /// The name of the group
    pub group: String,
}

#[derive(Serialize, Debug, Deserialize, Args)]
pub struct UpdateUserGroupArgs {
    /// The name of the group
    pub group: String,
    /// The username and role of the user to the group
    #[arg(num_args = 0.., value_parser = parse_key_val_colon::<String, UserGroupRole>)]
    pub roles: Vec<(String, UserGroupRole)>,
}

#[derive(Serialize, Debug, Deserialize, Args)]
pub struct RemoveUserGroupArgs {
    /// The name of the group
    pub group: String,
    /// The username of the user
    #[arg(num_args = 0..)]
    pub users: Vec<String>,
}
