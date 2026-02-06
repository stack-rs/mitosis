use serde::{Deserialize, Serialize};

use crate::entity::state::UserState;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CreateUserReq {
    pub username: String,
    pub md5_password: [u8; 16],
    pub admin: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UserLoginReq {
    pub username: String,
    pub md5_password: [u8; 16],
    #[serde(default)]
    pub retain: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UserLoginResp {
    pub token: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UserChangePasswordReq {
    pub old_md5_password: [u8; 16],
    pub new_md5_password: [u8; 16],
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UserChangePasswordResp {
    pub token: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AdminChangePasswordReq {
    pub new_md5_password: [u8; 16],
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ChangeUserStateReq {
    pub state: UserState,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UserStateResp {
    pub state: UserState,
}
