use clap::{Args, Parser, Subcommand};
use figment::{
    providers::{Env, Format, Serialized, Toml},
    value::magic::RelativePathBuf,
    Figment,
};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::{entity::content::ArtifactContentType, schema::RemoteResourceDownload};

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
    Get(GetArgs),
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

#[derive(Serialize, Debug, Deserialize, Args)]
pub struct GetArgs {
    #[command(subcommand)]
    pub command: GetCommands,
}

#[derive(Subcommand, Serialize, Debug, Deserialize)]
pub enum CreateCommands {
    /// Create a new user
    User(CreateUserArgs),
    /// Create a new group
    Group(CreateGroupArgs),
}

#[derive(Subcommand, Serialize, Debug, Deserialize)]
pub enum GetCommands {
    /// Get the info of a task
    Task(GetTaskArgs),
    /// Download an artifact of a task
    Artifact(GetArtifactArgs),
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
    /// The environment variables to set
    #[arg(short, long, num_args = 0.., value_delimiter = ',', value_parser = parse_key_val::<String, String>)]
    pub envs: Vec<(String, String)>,
    /// Whether to collect the terminal standard output and error of the executed task.
    #[arg(long = "terminal")]
    pub terminal_output: bool,
    /// The command to run
    #[arg(last = true)]
    pub command: Vec<String>,
    #[arg(skip)]
    pub resources: Vec<RemoteResourceDownload>,
}

/// Parse a single key-value pair
fn parse_key_val<T, U>(
    s: &str,
) -> Result<(T, U), Box<dyn std::error::Error + Send + Sync + 'static>>
where
    T: std::str::FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
    U: std::str::FromStr,
    U::Err: std::error::Error + Send + Sync + 'static,
{
    let pos = s
        .find('=')
        .ok_or_else(|| format!("invalid KEY=value: no `=` found in `{s}`"))?;
    Ok((s[..pos].parse()?, s[pos + 1..].parse()?))
}

#[derive(Serialize, Debug, Deserialize, Args)]
pub struct GetTaskArgs {
    /// The UUID of the task
    pub uuid: String,
}

#[derive(Serialize, Debug, Deserialize, Args)]
pub struct GetArtifactArgs {
    /// The UUID of the artifact
    pub uuid: String,
    /// The content type of the artifact
    #[arg(value_enum)]
    pub content_type: ArtifactContentType,
    /// Specify the directory to download the artifact
    #[arg(short, long = "output")]
    pub output_path: Option<String>,
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

impl From<CreateArgs> for ClientCommand {
    fn from(args: CreateArgs) -> Self {
        Self::Create(args)
    }
}

impl From<GetArgs> for ClientCommand {
    fn from(args: GetArgs) -> Self {
        Self::Get(args)
    }
}

impl From<SubmitTaskArgs> for ClientCommand {
    fn from(args: SubmitTaskArgs) -> Self {
        Self::Submit(args)
    }
}

impl From<CreateUserArgs> for CreateCommands {
    fn from(args: CreateUserArgs) -> Self {
        Self::User(args)
    }
}

impl From<CreateUserArgs> for CreateArgs {
    fn from(args: CreateUserArgs) -> Self {
        Self {
            command: CreateCommands::User(args),
        }
    }
}

impl From<CreateUserArgs> for ClientCommand {
    fn from(args: CreateUserArgs) -> Self {
        Self::Create(args.into())
    }
}

impl From<CreateGroupArgs> for CreateCommands {
    fn from(args: CreateGroupArgs) -> Self {
        Self::Group(args)
    }
}

impl From<CreateGroupArgs> for CreateArgs {
    fn from(args: CreateGroupArgs) -> Self {
        Self {
            command: CreateCommands::Group(args),
        }
    }
}

impl From<CreateGroupArgs> for ClientCommand {
    fn from(args: CreateGroupArgs) -> Self {
        Self::Create(args.into())
    }
}

impl From<GetTaskArgs> for GetCommands {
    fn from(args: GetTaskArgs) -> Self {
        Self::Task(args)
    }
}

impl From<GetTaskArgs> for GetArgs {
    fn from(args: GetTaskArgs) -> Self {
        Self {
            command: GetCommands::Task(args),
        }
    }
}

impl From<GetTaskArgs> for ClientCommand {
    fn from(args: GetTaskArgs) -> Self {
        Self::Get(args.into())
    }
}

impl From<GetArtifactArgs> for GetCommands {
    fn from(args: GetArtifactArgs) -> Self {
        Self::Artifact(args)
    }
}

impl From<GetArtifactArgs> for GetArgs {
    fn from(args: GetArtifactArgs) -> Self {
        Self {
            command: GetCommands::Artifact(args),
        }
    }
}

impl From<GetArtifactArgs> for ClientCommand {
    fn from(args: GetArtifactArgs) -> Self {
        Self::Get(args.into())
    }
}
