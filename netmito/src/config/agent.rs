use std::collections::HashSet;
use std::ops::Not;
use std::time::Duration;

use clap::Args;
use figment::{
    providers::{Env, Format, Serialized, Toml},
    value::magic::RelativePathBuf,
    Figment,
};
use serde::{Deserialize, Serialize};
use url::Url;

use super::coordinator::DEFAULT_COORDINATOR_ADDR;

#[derive(Deserialize, Serialize, Debug)]
pub struct AgentConfig {
    pub(crate) coordinator_addr: Url,
    pub(crate) credential_path: Option<RelativePathBuf>,
    pub(crate) user: Option<String>,
    pub(crate) password: Option<String>,
    pub(crate) groups: HashSet<String>,
    pub(crate) tags: HashSet<String>,
    pub(crate) labels: HashSet<String>,
    #[serde(with = "humantime_serde")]
    pub(crate) heartbeat_interval: Duration,
    #[serde(with = "humantime_serde")]
    pub(crate) lifetime: Option<Duration>,
    #[serde(default)]
    pub(crate) retain: bool,
}

#[derive(Args, Debug, Serialize, Default, Clone)]
#[command(rename_all = "kebab-case")]
pub struct AgentConfigCli {
    /// The path of the config file
    #[arg(long)]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub config: Option<String>,
    /// The address of the coordinator
    #[arg(short, long = "coordinator")]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub coordinator_addr: Option<String>,
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
    /// The groups to join
    #[arg(short, long, num_args = 0.., value_delimiter = ',')]
    #[serde(skip_serializing_if = "::std::vec::Vec::is_empty")]
    pub groups: Vec<String>,
    /// The tags for suite matching
    #[arg(short, long, num_args = 0.., value_delimiter = ',')]
    #[serde(skip_serializing_if = "::std::vec::Vec::is_empty")]
    pub tags: Vec<String>,
    /// The labels for identification
    #[arg(short, long, num_args = 0.., value_delimiter = ',')]
    #[serde(skip_serializing_if = "::std::vec::Vec::is_empty")]
    pub labels: Vec<String>,
    /// The interval to send heartbeat (e.g., "30s", "1m")
    #[arg(long)]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub heartbeat_interval: Option<String>,
    /// The lifetime of the agent token (e.g., "7d", "24h")
    #[arg(long)]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub lifetime: Option<String>,
    /// Whether to retain the previous login state without refreshing the credential
    #[arg(long)]
    #[serde(skip_serializing_if = "<&bool>::not")]
    pub retain: bool,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            coordinator_addr: Url::parse(&format!("http://{DEFAULT_COORDINATOR_ADDR}")).unwrap(),
            credential_path: None,
            user: None,
            password: None,
            groups: HashSet::new(),
            tags: HashSet::new(),
            labels: HashSet::new(),
            heartbeat_interval: Duration::from_secs(60),
            lifetime: None,
            retain: false,
        }
    }
}

impl AgentConfig {
    pub fn new(cli: &AgentConfigCli) -> crate::error::Result<Self> {
        let global_config = dirs::config_dir().map(|mut p| {
            p.push("mitosis");
            p.push("config.toml");
            p
        });
        let mut figment = Figment::new().merge(Serialized::from(Self::default(), "agent"));
        if let Some(global_config) = global_config {
            if global_config.exists() {
                figment = figment.merge(Toml::file(global_config).nested());
            }
        }
        figment = figment
            .merge(Toml::file(cli.config.as_deref().unwrap_or("config.toml")).nested())
            .merge(Env::prefixed("MITO_").profile("agent"))
            .merge(Serialized::from(cli, "agent"))
            .select("agent");
        Ok(figment.extract()?)
    }
}
