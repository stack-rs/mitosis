use clap::{Args, Parser, Subcommand};
use figment::{
    providers::{Env, Format, Serialized, Toml},
    value::magic::RelativePathBuf,
    Figment,
};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::schema::TaskCommand;

use super::coordinator::DEFAULT_COORDINATOR_ADDR;

#[derive(Deserialize, Serialize, Debug)]
pub struct ClientConfig {
    pub(crate) coordinator_addr: Url,
    pub(crate) credential_path: Option<RelativePathBuf>,
    pub(crate) user: Option<String>,
    pub(crate) password: Option<String>,
    pub(crate) interactive: bool,
    pub(crate) command: Option<ClientCommand>,
}

#[derive(Args, Debug, Serialize, Default)]
#[command(rename_all = "kebab-case")]
pub struct ClientConfigCli {
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
    /// Enable interactive mode
    #[arg(short, long)]
    pub interactive: bool,
    /// The command to run
    #[command(subcommand)]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub command: Option<ClientCommand>,
}

#[derive(Parser)]
#[command(name = "")]
pub struct ClientInteractiveShell {
    #[command(subcommand)]
    pub(crate) command: ClientCommand,
}

#[derive(Subcommand, Serialize, Debug, Deserialize)]
pub enum ClientCommand {
    /// Create a new user or group
    Create(CreateArgs),
    /// Get the info of a user, group, worker or task (not implemented yet)
    Get,
    /// Submit a task
    Submit(SubmitTaskArgs),
    /// Quit the client's interactive mode
    Quit,
}

#[derive(Serialize, Debug, Deserialize, Args)]
pub struct CreateArgs {
    #[command(subcommand)]
    pub command: CreateCommands,
}

#[derive(Subcommand, Serialize, Debug, Deserialize)]
pub enum CreateCommands {
    /// Create a new user
    User(CreateUserArgs),
    /// Create a new group
    Group(CreateGroupArgs),
}

#[derive(Serialize, Debug, Deserialize, Args)]
pub struct CreateUserArgs {
    /// The username of the user
    #[arg(short, long)]
    pub username: String,
    /// The password of the user
    #[arg(short, long)]
    pub password: String,
    /// Whether the user is an admin
    #[arg(long)]
    pub admin: bool,
}

#[derive(Serialize, Debug, Deserialize, Args)]
pub struct CreateGroupArgs {
    /// The name of the group
    #[arg(short = 'n', long)]
    pub name: String,
}

#[derive(Serialize, Debug, Deserialize, Args)]
pub struct SubmitTaskArgs {
    /// The name of the group this task is submitted to
    #[arg(short = 'g', long = "group")]
    pub group_name: Option<String>,
    /// The tags of the task
    #[arg(short, long, num_args = 0.., value_delimiter = ',')]
    pub tags: Vec<String>,
    /// The timeout of the task.
    #[arg(long, value_parser = humantime_serde::re::humantime::parse_duration, default_value="10min")]
    pub timeout: std::time::Duration,
    /// The priority of the task.
    #[arg(short, long, default_value_t = 0)]
    pub priority: i32,
    /// The shell to use
    #[arg(short, long, value_enum, default_value_t=TaskCommand::Bash)]
    pub shell: TaskCommand,
    /// Whether to collect the terminal standard output and error of the executed task.
    #[arg(long = "terminal")]
    pub terminal_output: bool,
    /// The command to run
    #[arg(last = true)]
    pub spec: Vec<String>,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            coordinator_addr: Url::parse(&format!("http://{}", DEFAULT_COORDINATOR_ADDR)).unwrap(),
            credential_path: None,
            user: None,
            password: None,
            interactive: false,
            command: None,
        }
    }
}

impl ClientConfig {
    pub fn new(cli: &ClientConfigCli) -> crate::error::Result<Self> {
        Ok(Figment::new()
            .merge(Serialized::from(Self::default(), "client"))
            .merge(Toml::file(cli.config.as_deref().unwrap_or("config.toml")).nested())
            .merge(Env::prefixed("MITO_").profile("client"))
            .merge(Serialized::from(cli, "client"))
            .select("client")
            .extract()?)
    }
}
