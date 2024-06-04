use core::fmt;

use clap::ValueEnum;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

/// The type of content stored as an attachment.
#[derive(EnumIter, DeriveActiveEnum, Clone, Debug, PartialEq, Eq)]
#[sea_orm(rs_type = "i32", db_type = "Integer")]
pub enum AttachmentContentType {
    NoSet = 0,
}

/// The type of content stored as an artifact.
#[derive(
    EnumIter, DeriveActiveEnum, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Copy, ValueEnum,
)]
#[sea_orm(rs_type = "i32", db_type = "Integer")]
#[serde(rename_all = "kebab-case")]
pub enum ArtifactContentType {
    /// The artifact is a file containing the task's output.
    Result = 0,
    /// The artifact contains user-specified records of the task execution.
    /// Should be retrieved by user.
    ExecLog = 1,
    /// The artifact contains stdout and stderr of the worker's sub-process executing task.
    /// Automatically retrieved by worker.
    StdLog = 2,
}

impl fmt::Display for ArtifactContentType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ArtifactContentType::Result => write!(f, "result.tar.gz"),
            ArtifactContentType::ExecLog => write!(f, "exec-log.tar.gz"),
            ArtifactContentType::StdLog => write!(f, "std-log.tar.gz"),
        }
    }
}
