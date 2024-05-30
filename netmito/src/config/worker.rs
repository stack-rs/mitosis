use clap::Args;
use figment::{
    providers::{Env, Format, Serialized, Toml},
    value::magic::RelativePathBuf,
    Figment,
};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, time::Duration};
use url::Url;

use super::coordinator::DEFAULT_COORDINATOR_ADDR;

#[derive(Deserialize, Serialize, Debug)]
pub struct WorkerConfig {
    pub(crate) coordinator_addr: Url,
    #[serde(with = "humantime_serde")]
    pub(crate) fetch_task_interval: Duration,
    #[serde(with = "humantime_serde")]
    pub(crate) heartbeat_interval: Duration,
    pub(crate) credential_path: Option<RelativePathBuf>,
    pub(crate) user: Option<String>,
    pub(crate) password: Option<String>,
    pub(crate) groups: HashSet<String>,
    pub(crate) tags: HashSet<String>,
    pub(crate) log_file: Option<RelativePathBuf>,
    pub(crate) no_log_file: bool,
}

#[derive(Args, Debug, Serialize, Default)]
#[command(rename_all = "kebab-case")]
pub struct WorkerConfigCli {
    /// The path of the config file
    #[arg(long)]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub config: Option<String>,
    /// The address of the coordinator
    #[arg(short, long = "coordinator")]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub coordinator_addr: Option<String>,
    /// The interval to fetch tasks
    #[arg(long = "fetch-interval")]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub fetch_task_interval: Option<String>,
    /// The interval to send heartbeat
    #[arg(long)]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub heartbeat_interval: Option<String>,
    /// The path of the user credential file
    #[arg(long)]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub credential_path: Option<String>,
    /// The username of the user
    #[arg(short, long)]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub user: Option<String>,
    /// The password of the user
    #[arg(short, long)]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub password: Option<String>,
    /// The groups allowed to submit tasks to this worker
    #[arg(short, long, num_args = 0.., value_delimiter = ',')]
    #[serde(skip_serializing_if = "::std::vec::Vec::is_empty")]
    pub groups: Vec<String>,
    /// The tags of this worker
    #[arg(short, long, num_args = 0.., value_delimiter = ',')]
    #[serde(skip_serializing_if = "::std::vec::Vec::is_empty")]
    pub tags: Vec<String>,
    /// The log file path. if not specified, then the default log file path would be used.
    /// Use `--no-log-file`` to disable logging to file
    #[arg(long)]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub log_file: Option<String>,
    /// Disable logging to file
    #[arg(long)]
    pub no_log_file: bool,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            coordinator_addr: Url::parse(&format!("http://{}", DEFAULT_COORDINATOR_ADDR)).unwrap(),
            fetch_task_interval: Duration::from_secs(5),
            heartbeat_interval: Duration::from_secs(10),
            credential_path: None,
            user: None,
            password: None,
            groups: HashSet::new(),
            tags: HashSet::new(),
            log_file: None,
            no_log_file: false,
        }
    }
}

impl WorkerConfig {
    pub fn new(cli: &WorkerConfigCli) -> crate::error::Result<Self> {
        Ok(Figment::new()
            .merge(Serialized::from(Self::default(), "worker"))
            .merge(Toml::file(cli.config.as_deref().unwrap_or("config.toml")).nested())
            .merge(Env::prefixed("MITO_").profile("worker"))
            .merge(Serialized::from(cli, "worker"))
            .select("worker")
            .extract()?)
    }
}
