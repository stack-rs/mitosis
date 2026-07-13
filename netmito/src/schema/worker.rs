use std::collections::{HashMap, HashSet};

use sea_orm::FromQueryResult;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::entity::{role::GroupWorkerRole, state::WorkerState};

#[derive(Debug, Clone, Default)]
pub enum WorkerTokenLifetime {
    #[default]
    Default,
    Duration(std::time::Duration),
    Never,
}

impl Serialize for WorkerTokenLifetime {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Default => serializer.serialize_str("default"),
            Self::Duration(duration) => serializer.serialize_str(
                &humantime_serde::re::humantime::format_duration(*duration).to_string(),
            ),
            Self::Never => serializer.serialize_str("never"),
        }
    }
}

impl<'de> Deserialize<'de> for WorkerTokenLifetime {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        match Option::<String>::deserialize(deserializer)?.as_deref() {
            None => Ok(Self::Default),
            Some(value) if value.eq_ignore_ascii_case("default") => Ok(Self::Default),
            Some(value) if value.eq_ignore_ascii_case("never") => Ok(Self::Never),
            Some(value) => humantime_serde::re::humantime::parse_duration(value)
                .map(Self::Duration)
                .map_err(serde::de::Error::custom),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RegisterWorkerReq {
    pub tags: HashSet<String>,
    pub labels: HashSet<String>,
    pub groups: HashSet<String>,
    #[serde(default)]
    pub lifetime: WorkerTokenLifetime,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RegisterWorkerResp {
    pub worker_id: Uuid,
    pub token: String,
    pub redis_url: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct WorkersQueryReq {
    pub group_name: Option<String>,
    pub role: Option<HashSet<GroupWorkerRole>>,
    pub tags: Option<HashSet<String>>,
    pub labels: Option<HashSet<String>>,
    pub creator_username: Option<String>,
    pub count: bool,
}

/// Request to shutdown multiple workers by filter criteria.
/// Uses the same filter fields as WorkersQueryReq.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct WorkersShutdownByFilterReq {
    pub group_name: Option<String>,
    pub role: Option<HashSet<GroupWorkerRole>>,
    pub tags: Option<HashSet<String>>,
    pub labels: Option<HashSet<String>>,
    pub creator_username: Option<String>,
    pub op: WorkerShutdownOp,
}

/// Response for batch worker shutdown operation
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct WorkersShutdownByFilterResp {
    pub shutdown_count: u64,
    pub group_name: String,
}

/// Request to shutdown multiple workers by UUIDs
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct WorkersShutdownByUuidsReq {
    pub uuids: Vec<Uuid>,
    pub op: WorkerShutdownOp,
}

/// Response for batch worker shutdown by UUIDs operation
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct WorkersShutdownByUuidsResp {
    pub shutdown_count: u64,
    pub failed_uuids: Vec<Uuid>,
}

#[derive(Debug, Serialize, Deserialize, FromQueryResult, Clone)]
pub(crate) struct RawWorkerQueryInfo {
    pub(crate) id: i64,
    pub(crate) worker_id: Uuid,
    pub(crate) creator_username: String,
    pub(crate) tags: Vec<String>,
    pub(crate) labels: Vec<String>,
    pub(crate) created_at: OffsetDateTime,
    pub(crate) updated_at: OffsetDateTime,
    pub(crate) state: WorkerState,
    pub(crate) last_heartbeat: OffsetDateTime,
    pub(crate) assigned_task_id: Option<Uuid>,
}

#[derive(Debug, Serialize, Deserialize, FromQueryResult, Clone)]
pub struct WorkerQueryInfo {
    pub worker_id: Uuid,
    pub creator_username: String,
    pub tags: Vec<String>,
    pub labels: Vec<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    pub state: WorkerState,
    pub last_heartbeat: OffsetDateTime,
    pub assigned_task_id: Option<Uuid>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct WorkerQueryResp {
    pub info: WorkerQueryInfo,
    pub groups: HashMap<String, GroupWorkerRole>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct WorkersQueryResp {
    pub count: u64,
    pub workers: Vec<WorkerQueryInfo>,
    pub group_name: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ReplaceWorkerTagsReq {
    pub tags: HashSet<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ReplaceWorkerLabelsReq {
    pub labels: HashSet<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UpdateGroupWorkerRoleReq {
    pub relations: HashMap<String, GroupWorkerRole>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RemoveGroupWorkerRoleReq {
    pub groups: HashSet<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RemoveGroupWorkerRoleParams {
    #[serde(default)]
    pub groups: HashSet<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct WorkerShutdown {
    pub op: Option<WorkerShutdownOp>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default, Copy)]
pub enum WorkerShutdownOp {
    #[default]
    #[serde(alias = "graceful")]
    Graceful,
    #[serde(alias = "force")]
    Force,
}

impl From<RawWorkerQueryInfo> for WorkerQueryInfo {
    fn from(model: RawWorkerQueryInfo) -> Self {
        Self {
            worker_id: model.worker_id,
            creator_username: model.creator_username,
            tags: model.tags,
            labels: model.labels,
            created_at: model.created_at,
            updated_at: model.updated_at,
            state: model.state,
            last_heartbeat: model.last_heartbeat,
            assigned_task_id: model.assigned_task_id,
        }
    }
}
