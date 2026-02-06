use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::entity::{role::UserGroupRole, state::GroupState};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CreateGroupReq {
    pub group_name: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GroupQueryInfo {
    pub group_name: String,
    pub creator_username: String,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    pub state: GroupState,
    pub task_count: i64,
    pub storage_quota: i64,
    pub storage_used: i64,
    pub worker_count: i64,
    pub users_in_group: Option<HashMap<String, UserGroupRole>>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GroupsQueryResp {
    pub groups: HashMap<String, UserGroupRole>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ChangeGroupStorageQuotaReq {
    pub storage_quota: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ChangeUserGroupQuota {
    pub group_quota: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GroupStorageQuotaResp {
    pub storage_quota: i64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UserGroupQuotaResp {
    pub group_quota: i32,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UpdateUserGroupRoleReq {
    pub relations: HashMap<String, UserGroupRole>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RemoveUserGroupRoleReq {
    pub users: HashSet<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RemoveUserGroupRoleParams {
    #[serde(default)]
    pub users: HashSet<String>,
}
