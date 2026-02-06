mod agent;
mod artifact;
mod attachment;
mod exec;
mod group;
mod suite;
mod task;
mod user;
mod worker;

pub use agent::*;
pub use artifact::*;
pub use attachment::*;
pub use exec::*;
pub use group::*;
pub use suite::*;
pub use task::*;
pub use user::*;
pub use worker::*;

// Glob re-export skips pub(crate) items, so re-export explicitly
pub(crate) use worker::RawWorkerQueryInfo;

use sea_orm::FromQueryResult;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, FromQueryResult, Clone)]
pub struct CountQuery {
    pub count: i64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RedisConnectionInfo {
    pub url: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ShutdownReq {
    pub secret: String,
}
