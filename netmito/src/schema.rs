use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::entity::state::UserState;

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateUserReq {
    pub username: String,
    pub md5_password: [u8; 16],
    pub admin: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UserLoginReq {
    pub username: String,
    pub md5_password: [u8; 16],
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UserLoginResp {
    pub token: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DeleteUserReq {
    pub username: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ChangeUserStateReq {
    pub username: String,
    pub state: UserState,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UserStateResp {
    pub state: UserState,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateGroupReq {
    pub group_name: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RegisterWorkerReq {
    pub tags: HashSet<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RegisterWorkerResp {
    pub worker_id: u64,
}
