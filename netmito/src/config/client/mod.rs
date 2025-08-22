use clap::{Args, Parser, Subcommand};
use figment::{
    providers::{Env, Format, Serialized, Toml},
    value::magic::RelativePathBuf,
    Figment,
};
use serde::{Deserialize, Serialize};
use url::Url;

use super::coordinator::DEFAULT_COORDINATOR_ADDR;

pub mod admin;
pub mod artifacts;
pub mod attachments;
pub mod groups;
pub mod tasks;
pub mod users;
pub mod workers;
pub use admin::*;
pub use artifacts::*;
pub use attachments::*;
pub use groups::*;
pub use tasks::*;
pub use users::*;
pub use workers::*;

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct ClientConfig {
    pub coordinator_addr: Url,
    pub credential_path: Option<RelativePathBuf>,
    pub user: Option<String>,
    pub password: Option<String>,
    pub retain: bool,
}

#[derive(Args, Debug, Serialize, Default, Clone)]
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
    /// Whether to retain the previous login state without refetching the credential
    #[arg(long)]
    pub retain: bool,
    /// The command to run
    #[command(subcommand)]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub command: Option<ClientCommand>,
}

#[derive(Parser, Clone)]
#[command(name = "")]
pub struct ClientInteractiveShell {
    #[command(subcommand)]
    pub(crate) command: ClientCommand,
}

#[derive(Subcommand, Serialize, Debug, Deserialize, derive_more::From, Clone)]
pub enum ClientCommand {
    /// Admin operations, including shutdown the coordinator, chaning user password, etc.
    Admin(AdminArgs),
    /// Authenticate current user
    Auth,
    /// Login with username and password
    Login(LoginArgs),
    /// Manage users, including changing password, querying the accessible groups etc.
    Users(UsersArgs),
    /// Manage groups, including creating a group, querying groups, etc.
    Groups(GroupsArgs),
    /// Manage tasks, including submitting a task, querying tasks, etc.
    Tasks(TasksArgs),
    /// Manage workers, including querying workers, cancel workers, etc.
    Workers(WorkersArgs),
    /// Run an external command
    Cmd(CmdArgs),
    /// Quit the client's interactive mode
    #[command(visible_alias("exit"))]
    Quit,
}

#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct LoginArgs {
    /// The username of the user
    pub username: Option<String>,
    /// The password of the user
    pub password: Option<String>,
    /// Whether to retain the previous login state without refetching the credential
    #[arg(long)]
    pub retain: bool,
}

#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct CmdArgs {
    /// Do not merge the command into one string
    #[arg(short, long)]
    pub split: bool,
    /// The command to run
    #[arg(last = true)]
    pub command: Vec<String>,
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

/// Parse a single key-value pair
fn parse_key_val_colon<T, U>(
    s: &str,
) -> Result<(T, U), Box<dyn std::error::Error + Send + Sync + 'static>>
where
    T: std::str::FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
    U: std::str::FromStr,
    U::Err: std::error::Error + Send + Sync + 'static,
{
    let pos = s
        .find(':')
        .ok_or_else(|| format!("invalid key-value pair: no `:` found in `{s}`"))?;
    Ok((s[..pos].parse()?, s[pos + 1..].parse()?))
}

/// Parse a single key-value pair
fn parse_watch_task<T, U>(
    s: &str,
) -> Result<(T, U), Box<dyn std::error::Error + Send + Sync + 'static>>
where
    T: std::str::FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
    U: std::str::FromStr,
    U::Err: std::error::Error + Send + Sync + 'static,
{
    let pos = s
        .find(',')
        .ok_or_else(|| format!("invalid watched task: no `,` found in `{s}`"))?;
    Ok((s[..pos].parse()?, s[pos + 1..].parse()?))
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            coordinator_addr: Url::parse(&format!("http://{DEFAULT_COORDINATOR_ADDR}")).unwrap(),
            credential_path: None,
            user: None,
            password: None,
            retain: false,
        }
    }
}

impl ClientConfig {
    pub fn new(cli: &ClientConfigCli) -> crate::error::Result<Self> {
        let global_config = dirs::config_dir().map(|mut p| {
            p.push("mitosis");
            p.push("config.toml");
            p
        });
        let mut figment = Figment::new().merge(Serialized::from(Self::default(), "client"));
        if let Some(global_config) = global_config {
            if global_config.exists() {
                figment = figment.merge(Toml::file(global_config).nested());
            }
        }
        figment = figment
            .merge(Toml::file(cli.config.as_deref().unwrap_or("config.toml")).nested())
            .merge(Env::prefixed("MITO_").profile("client"))
            .merge(Serialized::from(cli, "client"))
            .select("client");
        Ok(figment.extract()?)
    }
}
