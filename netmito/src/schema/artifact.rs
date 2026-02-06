use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::entity::{content::ArtifactContentType, state::TaskState};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UploadArtifactReq {
    pub content_type: ArtifactContentType,
    pub content_length: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UploadArtifactResp {
    pub url: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ArtifactQueryResp {
    pub content_type: ArtifactContentType,
    pub size: i64,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

impl From<crate::entity::artifacts::Model> for ArtifactQueryResp {
    fn from(model: crate::entity::artifacts::Model) -> Self {
        Self {
            content_type: model.content_type,
            size: model.size,
            created_at: model.created_at,
            updated_at: model.updated_at,
        }
    }
}

/// Request to batch download artifacts by filter criteria.
/// Uses the same filter fields as TasksQueryReq to find tasks, then downloads artifacts of the specified type.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ArtifactsDownloadByFilterReq {
    pub creator_usernames: Option<HashSet<String>>,
    pub group_name: Option<String>,
    pub tags: Option<HashSet<String>>,
    pub labels: Option<HashSet<String>>,
    pub states: Option<HashSet<TaskState>>,
    pub exit_status: Option<String>,
    pub priority: Option<String>,
    /// Set reporter_uuid will automatically exclude all non-completed tasks.
    pub reporter_uuid: Option<Uuid>,
    /// Filter tasks by suite UUID
    pub suite_uuid: Option<Uuid>,
    pub content_type: ArtifactContentType,
}

/// Request to batch download artifacts by task UUIDs.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ArtifactsDownloadByUuidsReq {
    pub uuids: Vec<Uuid>,
    pub content_type: ArtifactContentType,
}

/// Single artifact download item in batch response
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ArtifactDownloadItem {
    pub uuid: Uuid,
    pub url: String,
    pub size: i64,
}

/// Response for batch artifact download operations
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ArtifactsDownloadListResp {
    pub downloads: Vec<ArtifactDownloadItem>,
}

/// Request to batch delete artifacts by filter criteria.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ArtifactsDeleteByFilterReq {
    pub creator_usernames: Option<HashSet<String>>,
    pub group_name: Option<String>,
    pub tags: Option<HashSet<String>>,
    pub labels: Option<HashSet<String>>,
    pub states: Option<HashSet<TaskState>>,
    pub exit_status: Option<String>,
    pub priority: Option<String>,
    /// Set reporter_uuid will automatically exclude all non-completed tasks.
    pub reporter_uuid: Option<Uuid>,
    /// Filter tasks by suite UUID
    pub suite_uuid: Option<Uuid>,
    pub content_type: ArtifactContentType,
}

/// Request to batch delete artifacts by task UUIDs.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ArtifactsDeleteByUuidsReq {
    pub uuids: Vec<Uuid>,
    pub content_type: ArtifactContentType,
}

/// Response for batch artifact deletion by filter
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ArtifactsDeleteByFilterResp {
    pub deleted_count: u64,
}

/// Response for batch artifact deletion by UUIDs
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ArtifactsDeleteByUuidsResp {
    pub deleted_count: u64,
    pub failed_uuids: Vec<Uuid>,
}
