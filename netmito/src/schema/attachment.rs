use sea_orm::FromQueryResult;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::entity::content::AttachmentContentType;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UploadAttachmentReq {
    pub key: String,
    pub content_length: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UploadAttachmentResp {
    pub url: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AttachmentMetadata {
    pub content_type: AttachmentContentType,
    pub size: i64,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AttachmentsQueryReq {
    pub key: Option<String>,
    pub limit: Option<u64>,
    pub offset: Option<u64>,
    pub count: bool,
}

#[derive(Debug, Serialize, Deserialize, FromQueryResult, Clone)]
pub struct AttachmentQueryInfo {
    pub key: String,
    pub content_type: AttachmentContentType,
    pub size: i64,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AttachmentsQueryResp {
    pub count: u64,
    pub attachments: Vec<AttachmentQueryInfo>,
    pub group_name: String,
}

/// Request to batch download attachments by filter criteria.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AttachmentsDownloadByFilterReq {
    pub key: Option<String>,
    pub limit: Option<u64>,
    pub offset: Option<u64>,
}

/// Request to batch download attachments by keys.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AttachmentsDownloadByKeysReq {
    pub keys: Vec<String>,
}

/// Single attachment download item in batch response
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AttachmentDownloadItem {
    pub key: String,
    pub url: String,
    pub size: i64,
}

/// Response for batch attachment download operations
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AttachmentsDownloadListResp {
    pub downloads: Vec<AttachmentDownloadItem>,
    pub group_name: String,
}

/// Request to batch delete attachments by filter criteria.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AttachmentsDeleteByFilterReq {
    pub key: Option<String>,
    pub limit: Option<u64>,
    pub offset: Option<u64>,
}

/// Request to batch delete attachments by keys.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AttachmentsDeleteByKeysReq {
    pub keys: Vec<String>,
}

/// Response for batch attachment deletion by filter
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AttachmentsDeleteByFilterResp {
    pub deleted_count: u64,
    pub group_name: String,
}

/// Response for batch attachment deletion by keys
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AttachmentsDeleteByKeysResp {
    pub deleted_count: u64,
    pub failed_keys: Vec<String>,
    pub group_name: String,
}
