use sea_orm::entity::prelude::*;

/// The type of content stored as an attachment.
#[derive(EnumIter, DeriveActiveEnum, Clone, Debug, PartialEq, Eq)]
#[sea_orm(rs_type = "i32", db_type = "Integer")]
pub enum AttachmentContentType {
    NoSet = 0,
}

/// The type of content stored as an artifact.
#[derive(EnumIter, DeriveActiveEnum, Clone, Debug, PartialEq, Eq)]
#[sea_orm(rs_type = "i32", db_type = "Integer")]
pub enum ArtifactContentType {
    /// The artifact is a file containing the task's output.
    Result = 0,
    /// The artifact contains user-specified records of the task execution.
    /// Should be retrieved by user.
    ExecLog = 1,
    /// The artifact contains stdout and stderr of the worker's sub-process executing task.
    /// Automatically retrieved by worker.
    SystemLog = 2,
}
