use std::fmt::Display;

use clap::ValueEnum;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

/// The role of a user to a group.
#[derive(EnumIter, DeriveActiveEnum, Clone, Debug, PartialEq, Eq)]
#[sea_orm(rs_type = "i32", db_type = "Integer")]
pub enum UserGroupRole {
    /// The user can read the group's tasks.
    Read = 0,
    /// The user can submit tasks to the group and bring up workers for the group.
    Write = 1,
    /// The user can manage the group's membership and settings.
    Admin = 2,
}

/// The role of a group to a worker.
#[derive(
    EnumIter, DeriveActiveEnum, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, ValueEnum,
)]
#[sea_orm(rs_type = "i32", db_type = "Integer")]
pub enum GroupWorkerRole {
    /// Reserved for future use.
    Read = 0,
    /// The group can submit tasks to the worker's queue.
    Write = 1,
    /// The group can manage the worker's ACL and settings.
    Admin = 2,
}

impl Display for GroupWorkerRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}
