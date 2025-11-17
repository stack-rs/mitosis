use std::{fmt::Display, str::FromStr};

use clap::ValueEnum;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

/// The role of a user to a group.
#[derive(
    EnumIter,
    DeriveActiveEnum,
    Clone,
    Debug,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    ValueEnum,
    Ord,
    PartialOrd,
)]
#[sea_orm(rs_type = "i32", db_type = "Integer")]
pub enum UserGroupRole {
    /// The user can read the group's tasks.
    #[serde(alias = "read", alias = "READ")]
    Read = 0,
    /// The user can submit tasks to the group and bring up workers for the group.
    #[serde(alias = "write", alias = "WRITE")]
    Write = 1,
    /// The user can manage the group's membership and settings.
    #[serde(alias = "admin", alias = "ADMIN")]
    Admin = 2,
}

impl FromStr for UserGroupRole {
    type Err = crate::error::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "read" | "Read" | "READ" => Ok(Self::Read),
            "write" | "Write" | "WRITE" => Ok(Self::Write),
            "admin" | "Admin" | "ADMIN" => Ok(Self::Admin),
            _ => Err(crate::error::Error::Custom(format!(
                "Invalid UserGroupRole: {s}"
            ))),
        }
    }
}

impl Display for UserGroupRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

/// The role of a group to a worker.
#[derive(
    EnumIter,
    DeriveActiveEnum,
    Clone,
    Debug,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    ValueEnum,
    Copy,
)]
#[sea_orm(rs_type = "i32", db_type = "Integer")]
pub enum GroupWorkerRole {
    /// Reserved for future use.
    #[serde(alias = "read", alias = "READ")]
    Read = 0,
    /// The group can submit tasks to the worker's queue.
    #[serde(alias = "write", alias = "WRITE")]
    Write = 1,
    /// The group can manage the worker's ACL and settings.
    #[serde(alias = "admin", alias = "ADMIN")]
    Admin = 2,
}

impl FromStr for GroupWorkerRole {
    type Err = crate::error::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "read" | "Read" | "READ" => Ok(Self::Read),
            "write" | "Write" | "WRITE" => Ok(Self::Write),
            "admin" | "Admin" | "ADMIN" => Ok(Self::Admin),
            _ => Err(crate::error::Error::Custom(format!(
                "Invalid GroupWorkerRole: {s}"
            ))),
        }
    }
}

impl Display for GroupWorkerRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

/// The role of a group to a node manager.
#[derive(
    EnumIter,
    DeriveActiveEnum,
    Clone,
    Debug,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    ValueEnum,
    Copy,
)]
#[sea_orm(rs_type = "i32", db_type = "Integer")]
pub enum GroupNodeManagerRole {
    /// Reserved for future use (view manager status).
    #[serde(alias = "read", alias = "READ")]
    Read = 0,
    /// The group can submit task suites to the manager.
    #[serde(alias = "write", alias = "WRITE")]
    Write = 1,
    /// The group can manage the manager's ACL and settings.
    #[serde(alias = "admin", alias = "ADMIN")]
    Admin = 2,
}

impl FromStr for GroupNodeManagerRole {
    type Err = crate::error::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "read" | "Read" | "READ" => Ok(Self::Read),
            "write" | "Write" | "WRITE" => Ok(Self::Write),
            "admin" | "Admin" | "ADMIN" => Ok(Self::Admin),
            _ => Err(crate::error::Error::Custom(format!(
                "Invalid GroupNodeManagerRole: {s}"
            ))),
        }
    }
}

impl Display for GroupNodeManagerRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl GroupNodeManagerRole {
    pub fn has_write_access(&self) -> bool {
        matches!(self, Self::Write | Self::Admin)
    }

    pub fn has_admin_access(&self) -> bool {
        matches!(self, Self::Admin)
    }
}
