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
    /// Group granted Admin over the agent — the role that may shut it down. The
    /// registering user must be an Admin of it. Defaults to their own group.
    #[serde(default)]
    pub(crate) admin_group: Option<String>,
    /// Groups the agent joins; each gains Write access to it.
    pub(crate) groups: HashSet<String>,
    /// Tags a suite is matched against. A suite is eligible when the agent
    /// carries every tag the suite asks for.
    pub(crate) tags: HashSet<String>,
    /// Free-form labels for querying; not used for matching.
    pub(crate) labels: HashSet<String>,
    #[serde(with = "humantime_serde")]
    pub(crate) heartbeat_interval: Duration,
    /// How long to wait before retrying a coordinator call that could not
    /// connect.
    #[serde(with = "humantime_serde")]
    pub(crate) connect_retry_interval: Duration,
    /// How often an idle agent asks the coordinator for a suite rather than
    /// waiting to be told about one. Unset disables polling and agent only
    /// picks up suite after a heartbeat or upon a WebSocket notification.
    #[serde(with = "humantime_serde")]
    pub(crate) idle_poll_interval: Option<Duration>,
    /// How long to wait before reopening a notification WebSocket that dropped.
    #[serde(with = "humantime_serde")]
    pub(crate) ws_reconnect_interval: Duration,
    #[serde(default)]
    pub(crate) no_ws: bool,
    #[serde(with = "humantime_serde")]
    pub(crate) lifetime: Option<Duration>,
    #[serde(default)]
    pub(crate) retain: bool,
    /// Explicit machine code. When unset the agent resolves one from its cache,
    /// `/etc/machine-id`, or a freshly generated value.
    #[serde(default)]
    pub(crate) machine_code: Option<String>,
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
    /// The group granted Admin over the agent, which may shut it down. Defaults
    /// to the registering user's own group
    #[arg(long)]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub admin_group: Option<String>,
    /// The groups to join
    #[arg(short, long, num_args = 0.., value_delimiter = ',')]
    #[serde(skip_serializing_if = "::std::vec::Vec::is_empty")]
    pub groups: Vec<String>,
    /// The tags used to match task suites
    #[arg(short, long, num_args = 0.., value_delimiter = ',')]
    #[serde(skip_serializing_if = "::std::vec::Vec::is_empty")]
    pub tags: Vec<String>,
    /// The labels for identification
    #[arg(short, long, num_args = 0.., value_delimiter = ',')]
    #[serde(skip_serializing_if = "::std::vec::Vec::is_empty")]
    pub labels: Vec<String>,
    /// The interval to send heartbeats (e.g. "30s", "1m")
    #[arg(long)]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub heartbeat_interval: Option<String>,
    /// The interval between retries of a coordinator call that could not
    /// connect (e.g. "30s")
    #[arg(long)]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub connect_retry_interval: Option<String>,
    /// The interval for an idle agent to poll for a suite (e.g. "5s"). Waits to
    /// be notified of one instead if unset
    #[arg(long)]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub idle_poll_interval: Option<String>,
    /// The interval before reopening a dropped notification WebSocket (e.g. "5s")
    #[arg(long)]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub ws_reconnect_interval: Option<String>,
    /// Whether to take notifications from the heartbeat only, without opening
    /// the notification WebSocket
    #[arg(long)]
    #[serde(skip_serializing_if = "<&bool>::not")]
    pub no_ws: bool,
    /// The lifetime of the agent token (e.g. "7d", "24h"). Never expires if unset
    #[arg(long)]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub lifetime: Option<String>,
    /// Whether to retain the previous login state without refreshing the credential
    #[arg(long)]
    #[serde(skip_serializing_if = "<&bool>::not")]
    pub retain: bool,
    /// Explicit machine code (overrides /etc/machine-id auto-detection)
    #[arg(long)]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub machine_code: Option<String>,
    /// Run one suite and exit instead of staying up for more work. Intended for
    /// tests and one-shot batches.
    #[arg(long)]
    #[serde(skip_serializing_if = "<&bool>::not")]
    pub run_once: bool,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            coordinator_addr: Url::parse(&format!("http://{DEFAULT_COORDINATOR_ADDR}")).unwrap(),
            credential_path: None,
            user: None,
            password: None,
            admin_group: None,
            groups: HashSet::new(),
            tags: HashSet::new(),
            labels: HashSet::new(),
            heartbeat_interval: Duration::from_secs(60),
            connect_retry_interval: Duration::from_secs(30),
            idle_poll_interval: None,
            ws_reconnect_interval: Duration::from_secs(5),
            no_ws: false,
            lifetime: None,
            retain: false,
            machine_code: None,
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
