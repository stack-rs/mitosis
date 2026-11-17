use std::collections::HashSet;
use std::io::Write;
use std::ops::Not;
use std::time::Duration;

use clap::Args;
use figment::{
    providers::{Env, Format, Serialized, Toml},
    value::magic::RelativePathBuf,
    Figment,
};
use serde::{Deserialize, Serialize};
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::{fmt::MakeWriter, layer::SubscriberExt, util::SubscriberInitExt, Layer};
use url::Url;

use crate::error::Error;

use super::{coordinator::DEFAULT_COORDINATOR_ADDR, TracingGuard};

/// A writer that adds the agent UUID at the start of each log line.
struct AgentIdWriter<W: Write> {
    inner: W,
    prefix: Vec<u8>,
    at_line_start: bool,
    buffer: Vec<u8>,
}

impl<W: Write> Drop for AgentIdWriter<W> {
    fn drop(&mut self) {
        let _ = self.flush_buffer();
    }
}

impl<W: Write> AgentIdWriter<W> {
    fn new(inner: W, agent_id: String) -> Self {
        // Make the prefix once. Then each line does not format it again.
        let prefix = format!("[agent:{}] ", agent_id).into_bytes();
        Self {
            inner,
            prefix,
            at_line_start: true,
            buffer: Vec::with_capacity(8192),
        }
    }

    fn flush_buffer(&mut self) -> std::io::Result<()> {
        if !self.buffer.is_empty() {
            self.inner.write_all(&self.buffer)?;
            self.buffer.clear();
        }
        Ok(())
    }
}

impl<W: Write> Write for AgentIdWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut remaining = buf;
        let total_len = buf.len();

        while !remaining.is_empty() {
            // Find the next newline.
            if let Some(newline_pos) = remaining.iter().position(|&b| b == b'\n') {
                // Write the prefix at the start of a line.
                if self.at_line_start && newline_pos > 0 {
                    self.buffer.extend_from_slice(&self.prefix);
                    self.at_line_start = false;
                }

                // Write the line and its newline.
                self.buffer.extend_from_slice(&remaining[..=newline_pos]);
                self.at_line_start = true;

                // Flush the buffer when it is larger than 4 KB.
                if self.buffer.len() > 4096 {
                    self.flush_buffer()?;
                }

                remaining = &remaining[newline_pos + 1..];
            } else {
                // The remaining data has no newline.
                if self.at_line_start && !remaining.is_empty() {
                    self.buffer.extend_from_slice(&self.prefix);
                    self.at_line_start = false;
                }
                self.buffer.extend_from_slice(remaining);
                break;
            }
        }

        Ok(total_len)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.flush_buffer()?;
        self.inner.flush()
    }
}

/// A `MakeWriter` that makes `AgentIdWriter` instances.
struct AgentIdMakeWriter<M> {
    inner: M,
    agent_id: String,
}

impl<'a, M> MakeWriter<'a> for AgentIdMakeWriter<M>
where
    M: MakeWriter<'a>,
{
    type Writer = AgentIdWriter<M::Writer>;

    fn make_writer(&'a self) -> Self::Writer {
        AgentIdWriter::new(self.inner.make_writer(), self.agent_id.clone())
    }
}

#[derive(Deserialize, Serialize, Debug)]
pub struct AgentConfig {
    pub(crate) coordinator_addr: Url,
    pub(crate) credential_path: Option<RelativePathBuf>,
    pub(crate) user: Option<String>,
    pub(crate) password: Option<String>,
    /// The group that gets the Admin role on the agent. Only an Admin of this group can shut the
    /// agent down, and the user that registers the agent must be one. The default is the personal
    /// group of the user.
    #[serde(default)]
    pub(crate) admin_group: Option<String>,
    /// The groups that get the Write role on the agent.
    pub(crate) groups: HashSet<String>,
    /// The tags that a suite must match. The agent can run a suite if it has all the tags of the
    /// suite.
    pub(crate) tags: HashSet<String>,
    /// Labels for queries. Suite matching does not use them.
    pub(crate) labels: HashSet<String>,
    #[serde(with = "humantime_serde")]
    pub(crate) heartbeat_interval: Duration,
    /// The time to wait before the agent tries again a coordinator call that could not connect.
    #[serde(with = "humantime_serde")]
    pub(crate) connect_retry_interval: Duration,
    /// The interval at which an idle agent asks the coordinator for a suite with no notification.
    /// `None` stops the poll: the agent then asks only after a notification from the heartbeat
    /// or the WebSocket.
    #[serde(with = "humantime_serde")]
    pub(crate) idle_poll_interval: Option<Duration>,
    /// The time to wait before the agent opens a dropped notification WebSocket again.
    #[serde(with = "humantime_serde")]
    pub(crate) ws_reconnect_interval: Duration,
    #[serde(default)]
    pub(crate) no_ws: bool,
    /// The lifetime of the agent token. `None` means the token never expires.
    #[serde(default, with = "humantime_serde")]
    pub(crate) lifetime: Option<Duration>,
    /// The machine code. If it is not set, the agent gets one from its cache, from
    /// `/etc/machine-id`, or makes a new one.
    #[serde(default)]
    pub(crate) machine_code: Option<String>,
    pub(crate) log_path: Option<RelativePathBuf>,
    pub(crate) file_log: bool,
    #[serde(default)]
    pub(crate) shared_log: bool,
    #[serde(default)]
    pub(crate) skip_redis: bool,
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
    /// The group that gets the Admin role on the agent and can shut it down. The default is the
    /// personal group of the user
    #[arg(long)]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub admin_group: Option<String>,
    /// The groups that get the Write role on the agent
    #[arg(short, long, num_args = 0.., value_delimiter = ',')]
    #[serde(skip_serializing_if = "::std::vec::Vec::is_empty")]
    pub groups: Vec<String>,
    /// The tags that task suites must match
    #[arg(short, long, num_args = 0.., value_delimiter = ',')]
    #[serde(skip_serializing_if = "::std::vec::Vec::is_empty")]
    pub tags: Vec<String>,
    /// The labels of this agent
    #[arg(short, long, num_args = 0.., value_delimiter = ',')]
    #[serde(skip_serializing_if = "::std::vec::Vec::is_empty")]
    pub labels: Vec<String>,
    /// The interval to send heartbeats (e.g. "30s", "1m")
    #[arg(long)]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub heartbeat_interval: Option<String>,
    /// The time between tries of a coordinator call that could not connect (e.g. "30s")
    #[arg(long)]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub connect_retry_interval: Option<String>,
    /// The interval at which an idle agent asks for a suite (e.g. "5s"). If not set, the agent
    /// waits for a notification
    #[arg(long)]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub idle_poll_interval: Option<String>,
    /// The time before the agent opens a dropped notification WebSocket again (e.g. "5s")
    #[arg(long)]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub ws_reconnect_interval: Option<String>,
    /// Get notifications only from the heartbeat, and open no notification WebSocket
    #[arg(long)]
    #[serde(skip_serializing_if = "<&bool>::not")]
    pub no_ws: bool,
    /// The lifetime of the agent token (e.g. 7d, 1year). If not given, the agent token does not expire
    #[arg(long, value_parser = humantime_serde::re::humantime::parse_duration)]
    #[serde(
        with = "humantime_serde",
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub lifetime: Option<Duration>,
    /// The machine code. It replaces the code from /etc/machine-id
    #[arg(long)]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub machine_code: Option<String>,
    /// The path of the log file. If not given, the agent uses the default rolling log file
    #[arg(long)]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub log_path: Option<String>,
    /// Write the logs to a file
    #[arg(long)]
    #[serde(skip_serializing_if = "<&bool>::not")]
    pub file_log: bool,
    /// Write the logs of all agents to one file, with a new file each day (3 files at most)
    #[arg(long)]
    #[serde(skip_serializing_if = "<&bool>::not")]
    pub shared_log: bool,
    /// Do not connect to Redis
    #[arg(long)]
    #[serde(skip_serializing_if = "<&bool>::not")]
    pub skip_redis: bool,
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
            machine_code: None,
            log_path: None,
            file_log: false,
            shared_log: false,
            skip_redis: false,
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

    pub fn setup_tracing_subscriber<T, U>(&self, agent_id: U) -> crate::error::Result<TracingGuard>
    where
        T: std::fmt::Display,
        U: Into<T>,
    {
        let (file_layer, file_guard) = if self.file_log {
            let id = agent_id.into();
            let id_str = id.to_string();

            // `shared_log` selects the file logger.
            let file_logger = if self.shared_log {
                // A shared log: use `log_path` if it is given, else "agents.log"
                self.log_path
                    .as_ref()
                    .and_then(|p| {
                        let path = p.relative();
                        let dir = path.parent();
                        let file_name = path.file_name();
                        match (dir, file_name) {
                            (Some(dir), Some(file_name)) => {
                                // A new file each day, 3 files at most.
                                RollingFileAppender::builder()
                                    .rotation(Rotation::DAILY)
                                    .filename_prefix(file_name.to_string_lossy().to_string())
                                    .max_log_files(3)
                                    .build(dir)
                                    .ok()
                            }
                            _ => None,
                        }
                    })
                    .or_else(|| {
                        // "agents.log" in the cache directory, with a new file each day.
                        dirs::cache_dir()
                            .map(|mut p| {
                                p.push("mitosis");
                                p.push("agent");
                                p
                            })
                            .and_then(|dir| {
                                RollingFileAppender::builder()
                                    .rotation(Rotation::DAILY)
                                    .filename_prefix("agents.log")
                                    .max_log_files(3)
                                    .build(dir)
                                    .ok()
                            })
                    })
                    .ok_or(Error::ConfigError(Box::new(figment::Error::from(
                        "log path not valid and cache directory not found",
                    ))))?
            } else {
                // A log for this agent only, with no rotation.
                self.log_path
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
                                p.push("agent");
                                p
                            })
                            .map(|dir| {
                                tracing_appender::rolling::never(dir, format!("{id_str}.log"))
                            })
                    })
                    .ok_or(Error::ConfigError(Box::new(figment::Error::from(
                        "log path not valid and cache directory not found",
                    ))))?
            };

            let (non_blocking, guard) = tracing_appender::non_blocking(file_logger);
            let env_filter = tracing_subscriber::EnvFilter::try_from_env("MITO_FILE_LOG_LEVEL")
                .unwrap_or_else(|_| "netmito=info".into());

            // With `shared_log`, one file holds the logs of many agents, thus each line has its
            // agent id. `BoxMakeWriter` gives the two writers one type, thus the two cases use the
            // same layers below.
            let writer = if self.shared_log {
                tracing_subscriber::fmt::writer::BoxMakeWriter::new(AgentIdMakeWriter {
                    inner: non_blocking,
                    agent_id: id_str,
                })
            } else {
                tracing_subscriber::fmt::writer::BoxMakeWriter::new(non_blocking)
            };
            let layer = tracing_subscriber::fmt::layer()
                .with_file(true)
                .with_line_number(true)
                .with_writer(writer)
                .with_filter(env_filter);
            (Some(layer), Some(guard))
        } else {
            (None, None)
        };

        tracing_subscriber::registry()
            .with(
                tracing_subscriber::fmt::layer()
                    .with_file(true)
                    .with_line_number(true)
                    .with_filter(
                        tracing_subscriber::EnvFilter::try_from_default_env()
                            .unwrap_or_else(|_| "netmito=info".into()),
                    ),
            )
            .with(file_layer)
            // The same tracing-subscriber workaround as in `setup_tracing_subscriber` of the
            // coordinator. See there for the reason. Remove all of them when tokio-rs/tracing#2519
            // is fixed.
            .with(tracing_subscriber::layer::Identity::new())
            .init();
        Ok(TracingGuard { file_guard })
    }
}
