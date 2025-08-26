use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};

use super::{CancelWorkerArgs, DeleteArtifactArgs, DeleteAttachmentArgs};

#[derive(Serialize, Debug, Deserialize, Args, derive_more::From, Clone)]
pub struct AdminArgs {
    #[command(subcommand)]
    pub command: AdminCommands,
}

#[derive(Subcommand, Serialize, Debug, Deserialize, derive_more::From, Clone)]
pub enum AdminCommands {
    /// Manage users
    Users(AdminUsersArgs),
    /// Shutdown the coordinator
    Shutdown(ShutdownArgs),
    /// Manage groups
    Groups(AdminGroupsArgs),
    /// Manage a task
    Tasks(AdminTasksArgs),
    /// Manage a worker
    Workers(AdminWorkersArgs),
}

#[derive(Serialize, Debug, Deserialize, Args, derive_more::From, Clone)]
pub struct AdminUsersArgs {
    #[command(subcommand)]
    pub command: AdminUsersCommands,
}

#[derive(Subcommand, Serialize, Debug, Deserialize, derive_more::From, Clone)]
pub enum AdminUsersCommands {
    /// Create a new user
    Create(AdminCreateUserArgs),
    /// Delete a user
    Delete(AdminDeleteUserArgs),
    /// Change the password of a user
    ChangePassword(ChangePasswordArgs),
}

#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct AdminCreateUserArgs {
    /// The username of the user
    pub username: Option<String>,
    /// The password of the user
    pub password: Option<String>,
    /// Whether to grant the new user as an admin user
    #[arg(long)]
    pub admin: bool,
}

#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct AdminDeleteUserArgs {
    /// The username of the user
    pub username: String,
}

#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct ChangePasswordArgs {
    /// The name of the user
    pub username: Option<String>,
    /// The new password of the user
    pub new_password: Option<String>,
}

#[derive(Serialize, Debug, Deserialize, Args, derive_more::From, Clone)]
pub struct ShutdownArgs {
    pub secret: String,
}

#[derive(Serialize, Debug, Deserialize, Args, derive_more::From, Clone)]
pub struct AdminGroupsArgs {
    #[command(subcommand)]
    pub command: AdminGroupsCommands,
}

#[derive(Subcommand, Serialize, Debug, Deserialize, derive_more::From, Clone)]
pub enum AdminGroupsCommands {
    /// Update storage quota of a group
    StorageQuota(AdminUpdateGroupStorageQuotaArgs),
    /// Delete an attachment of a group
    Attachments(AdminAttachmentsArgs),
}

#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct AdminUpdateGroupStorageQuotaArgs {
    pub group_name: String,
    pub storage_quota: String,
}

#[derive(Serialize, Debug, Deserialize, Args, derive_more::From, Clone)]
pub struct AdminAttachmentsArgs {
    #[command(subcommand)]
    pub command: AdminAttachmentsCommands,
}

#[derive(Subcommand, Serialize, Debug, Deserialize, derive_more::From, Clone)]
pub enum AdminAttachmentsCommands {
    Delete(DeleteAttachmentArgs),
}

#[derive(Serialize, Debug, Deserialize, Args, derive_more::From, Clone)]
pub struct AdminTasksArgs {
    #[command(subcommand)]
    pub command: AdminTasksCommands,
}

#[derive(Subcommand, Serialize, Debug, Deserialize, derive_more::From, Clone)]
pub enum AdminTasksCommands {
    Artifacts(AdminArtifactsArgs),
}

#[derive(Serialize, Debug, Deserialize, Args, derive_more::From, Clone)]
pub struct AdminArtifactsArgs {
    #[command(subcommand)]
    pub command: AdminArtifactsCommands,
}

#[derive(Subcommand, Serialize, Debug, Deserialize, derive_more::From, Clone)]
pub enum AdminArtifactsCommands {
    Delete(DeleteArtifactArgs),
}

#[derive(Serialize, Debug, Deserialize, Args, derive_more::From, Clone)]
pub struct AdminWorkersArgs {
    #[command(subcommand)]
    pub command: AdminWorkersCommands,
}

#[derive(Subcommand, Serialize, Debug, Deserialize, derive_more::From, Clone)]
pub enum AdminWorkersCommands {
    /// Cancel a worker
    Cancel(CancelWorkerArgs),
}
