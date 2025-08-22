use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Debug, Deserialize, Args, derive_more::From, Clone)]
pub struct UsersArgs {
    #[command(subcommand)]
    pub command: UsersCommands,
}

#[derive(Subcommand, Serialize, Debug, Deserialize, derive_more::From, Clone)]
pub enum UsersCommands {
    /// User change the password
    ChangePassword(UserChangePasswordArgs),
    /// Get all groups the user has access to
    #[command(visible_alias("my-groups"))]
    Groups,
}

#[derive(Serialize, Debug, Deserialize, Args, derive_more::From, Clone)]
pub struct UserChangePasswordArgs {
    /// The original password of the user
    pub orig_password: Option<String>,
    /// The new password of the user
    pub new_password: Option<String>,
}
