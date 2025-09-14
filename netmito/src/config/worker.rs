use clap::Args;
use figment::{
    providers::{Env, Format, Serialized, Toml},
    value::magic::RelativePathBuf,
    Figment,
};
use serde::{Deserialize, Serialize};
use std::ops::Not;
use std::{collections::HashSet, time::Duration};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, Layer};
use url::Url;

use crate::error::Error;

use super::{coordinator::DEFAULT_COORDINATOR_ADDR, TracingGuard};

#[derive(Deserialize, Serialize, Debug)]
pub struct WorkerConfig {
    pub(crate) coordinator_addr: Url,
    #[serde(with = "humantime_serde")]
    pub(crate) polling_interval: Duration,
    #[serde(with = "humantime_serde")]
    pub(crate) heartbeat_interval: Duration,
    pub(crate) credential_path: Option<RelativePathBuf>,
    pub(crate) user: Option<String>,
    pub(crate) password: Option<String>,
    pub(crate) groups: HashSet<String>,
    pub(crate) tags: HashSet<String>,
    pub(crate) labels: HashSet<String>,
    pub(crate) log_path: Option<RelativePathBuf>,
    pub(crate) file_log: bool,
    #[serde(with = "humantime_serde")]
    pub(crate) lifetime: Option<Duration>,
    #[serde(default)]
    pub(crate) retain: bool,
    #[serde(default)]
    pub(crate) skip_redis: bool,
}

#[derive(Args, Debug, Serialize, Default, Clone)]
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
    /// The interval to poll tasks or resources
    #[arg(long)]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub polling_interval: Option<String>,
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
    /// The labels of this worker
    #[arg(short, long, num_args = 0.., value_delimiter = ',')]
    #[serde(skip_serializing_if = "::std::vec::Vec::is_empty")]
    pub labels: Vec<String>,
    /// The log file path. If not specified, then the default rolling log file path would be used.
    /// If specified, then the log file would be exactly at the path specified.
    #[arg(long)]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub log_path: Option<String>,
    /// Enable logging to file
    #[arg(long)]
    #[serde(skip_serializing_if = "<&bool>::not")]
    pub file_log: bool,
    /// The lifetime of the worker to alive (e.g., 7d, 1year)
    #[arg(long)]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub lifetime: Option<String>,
    /// Whether to retain the previous login state without refetching the credential
    #[arg(long)]
    #[serde(skip_serializing_if = "<&bool>::not")]
    pub retain: bool,
    /// Whether to skip connecting to Redis
    #[arg(long)]
    #[serde(skip_serializing_if = "<&bool>::not")]
    pub skip_redis: bool,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            coordinator_addr: Url::parse(&format!("http://{DEFAULT_COORDINATOR_ADDR}")).unwrap(),
            polling_interval: Duration::from_secs(180),
            heartbeat_interval: Duration::from_secs(300),
            credential_path: None,
            user: None,
            password: None,
            groups: HashSet::new(),
            tags: HashSet::new(),
            labels: HashSet::new(),
            log_path: None,
            file_log: false,
            lifetime: None,
            retain: false,
            skip_redis: false,
        }
    }
}

impl WorkerConfig {
    pub fn new(cli: &WorkerConfigCli) -> crate::error::Result<Self> {
        let global_config = dirs::config_dir().map(|mut p| {
            p.push("mitosis");
            p.push("config.toml");
            p
        });
        let mut figment = Figment::new().merge(Serialized::from(Self::default(), "worker"));
        if let Some(global_config) = global_config {
            if global_config.exists() {
                figment = figment.merge(Toml::file(global_config).nested());
            }
        }
        figment = figment
            .merge(Toml::file(cli.config.as_deref().unwrap_or("config.toml")).nested())
            .merge(Env::prefixed("MITO_").profile("worker"))
            .merge(Serialized::from(cli, "worker"))
            .select("worker");
        Ok(figment.extract()?)
    }

    pub fn setup_tracing_subscriber<T, U>(&self, worker_id: U) -> crate::error::Result<TracingGuard>
    where
        T: std::fmt::Display,
        U: Into<T>,
    {
        if self.file_log {
            let file_logger = self
                .log_path
                .as_ref()
                .and_then(|p| {
                    let path = p.relative();
                    let dir = path.parent();
                    let file_name = path.file_name();
                    match (dir, file_name) {
                        (Some(dir), Some(file_name)) => {
                            Some(tracing_appender::rolling::never(dir, file_name))
                        }
                        _ => None,
                    }
                })
                .or_else(|| {
                    dirs::cache_dir()
                        .map(|mut p| {
                            p.push("mitosis");
                            p.push("worker");
                            p
                        })
                        .map(|dir| {
                            let id = worker_id.into();
                            tracing_appender::rolling::never(dir, format!("{id}.log"))
                        })
                })
                .ok_or(Error::ConfigError(Box::new(figment::Error::from(
                    "log path not valid and cache directory not found",
                ))))?;
            let (non_blocking, guard) = tracing_appender::non_blocking(file_logger);
            let env_filter = tracing_subscriber::EnvFilter::try_from_env("MITO_FILE_LOG_LEVEL")
                .unwrap_or_else(|_| "netmito=info".into());
            let coordinator_guard = tracing_subscriber::registry()
                .with(
                    tracing_subscriber::fmt::layer().with_filter(
                        tracing_subscriber::EnvFilter::try_from_default_env()
                            .unwrap_or_else(|_| "netmito=info".into()),
                    ),
                )
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_writer(non_blocking)
                        .with_filter(env_filter),
                )
                .set_default();
            Ok(TracingGuard {
                subscriber_guard: Some(coordinator_guard),
                file_guard: Some(guard),
            })
        } else {
            let coordinator_guard = tracing_subscriber::registry()
                .with(
                    tracing_subscriber::fmt::layer().with_filter(
                        tracing_subscriber::EnvFilter::try_from_default_env()
                            .unwrap_or_else(|_| "netmito=info".into()),
                    ),
                )
                .set_default();
            Ok(TracingGuard {
                subscriber_guard: Some(coordinator_guard),
                file_guard: None,
            })
        }
    }
}
