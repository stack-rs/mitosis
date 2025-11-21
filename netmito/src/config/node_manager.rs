use clap::Args;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Configuration for node manager service
#[derive(Args, Debug, Clone, Serialize, Deserialize)]
#[command(rename_all = "kebab-case")]
pub struct NodeManagerConfigCli {
    /// Coordinator URL
    #[arg(long, env = "MITOSIS_COORDINATOR_URL")]
    pub coordinator_url: String,

    /// Manager tags (comma-separated, e.g., "gpu,linux,cuda:11.8")
    #[arg(long, env = "MITOSIS_MANAGER_TAGS", value_delimiter = ',')]
    #[serde(default)]
    pub tags: Vec<String>,

    /// Manager labels (comma-separated, e.g., "datacenter:us-west,machine_id:server-42")
    #[arg(long, env = "MITOSIS_MANAGER_LABELS", value_delimiter = ',')]
    #[serde(default)]
    pub labels: Vec<String>,

    /// Groups this manager belongs to (comma-separated)
    #[arg(long, env = "MITOSIS_MANAGER_GROUPS", value_delimiter = ',')]
    #[serde(default)]
    pub groups: Vec<String>,

    /// Configuration file path
    #[arg(short, long, env = "MITOSIS_MANAGER_CONFIG")]
    pub config: Option<PathBuf>,

    /// Username for authentication
    #[arg(long, env = "MITOSIS_MANAGER_USER")]
    pub user: Option<String>,

    /// Password for authentication
    #[arg(long, env = "MITOSIS_MANAGER_PASSWORD")]
    pub password: Option<String>,

    /// Token lifetime (e.g., "30d", "7d", "24h")
    #[arg(long, default_value = "30d")]
    pub token_lifetime: String,

    /// Heartbeat interval (e.g., "30s", "1m")
    #[arg(long, default_value = "30s")]
    pub heartbeat_interval: String,

    /// Log level
    #[arg(long, default_value = "info")]
    pub log_level: String,
}

/// Full node manager configuration (merged from CLI and file)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeManagerConfig {
    pub coordinator_url: String,
    pub tags: Vec<String>,
    pub labels: Vec<String>,
    pub groups: Vec<String>,
    pub user: Option<String>,
    pub password: Option<String>,

    #[serde(with = "humantime_serde")]
    pub token_lifetime: std::time::Duration,

    #[serde(with = "humantime_serde")]
    pub heartbeat_interval: std::time::Duration,

    #[serde(with = "humantime_serde", default = "default_reconnect_min_backoff")]
    pub reconnect_min_backoff: std::time::Duration,

    #[serde(with = "humantime_serde", default = "default_reconnect_max_backoff")]
    pub reconnect_max_backoff: std::time::Duration,

    pub log_level: String,
}

fn default_reconnect_min_backoff() -> std::time::Duration {
    std::time::Duration::from_secs(1)
}

fn default_reconnect_max_backoff() -> std::time::Duration {
    std::time::Duration::from_secs(60)
}

impl NodeManagerConfig {
    /// Load configuration from CLI args and optional config file
    pub fn from_cli(cli: &NodeManagerConfigCli) -> Result<Self, Box<dyn std::error::Error>> {
        let mut config = if let Some(config_path) = &cli.config {
            // Load from file
            let file_content = std::fs::read_to_string(config_path)?;
            toml::from_str::<NodeManagerConfig>(&file_content)?
        } else {
            // Use defaults
            NodeManagerConfig {
                coordinator_url: cli.coordinator_url.clone(),
                tags: cli.tags.clone(),
                labels: cli.labels.clone(),
                groups: cli.groups.clone(),
                user: cli.user.clone(),
                password: cli.password.clone(),
                token_lifetime: humantime::parse_duration(&cli.token_lifetime)?,
                heartbeat_interval: humantime::parse_duration(&cli.heartbeat_interval)?,
                reconnect_min_backoff: default_reconnect_min_backoff(),
                reconnect_max_backoff: default_reconnect_max_backoff(),
                log_level: cli.log_level.clone(),
            }
        };

        // CLI args override config file
        if !cli.tags.is_empty() {
            config.tags = cli.tags.clone();
        }
        if !cli.labels.is_empty() {
            config.labels = cli.labels.clone();
        }
        if !cli.groups.is_empty() {
            config.groups = cli.groups.clone();
        }
        if cli.user.is_some() {
            config.user = cli.user.clone();
        }
        if cli.password.is_some() {
            config.password = cli.password.clone();
        }

        Ok(config)
    }
}
