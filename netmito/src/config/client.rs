use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use clap::{Args, Parser, Subcommand};
use figment::{
    providers::{Env, Format, Serialized, Toml},
    value::magic::RelativePathBuf,
    Figment,
};
use serde::{Deserialize, Serialize};
use url::Url;
use uuid::Uuid;

use crate::{
    entity::{
        content::ArtifactContentType,
        state::{TaskExecState, TaskState},
    },
    schema::{RemoteResourceDownload, TaskSpec, TasksQueryReq},
};

use super::coordinator::DEFAULT_COORDINATOR_ADDR;

#[derive(Deserialize, Serialize, Debug)]
pub struct ClientConfig {
    pub(crate) coordinator_addr: Url,
    pub(crate) credential_path: Option<RelativePathBuf>,
    pub(crate) user: Option<String>,
    pub(crate) password: Option<String>,
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
    /// Get the info of a task, artifact, attachment, or a list of tasks subject to the filters
    ///
    /// Query for users or workers is not implemented yet
    Get(GetArgs),
    /// Submit a task
    Submit(SubmitTaskCmdArgs),
    /// Upload an attachment to a group
    Upload(UploadAttachmentArgs),
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
    Artifact(GetArtifactCmdArgs),
    /// Download an attachment of a group
    Attachment(GetAttachmentCmdArgs),
    /// Query a list of tasks subject to the filter
    Tasks(GetTasksArgs),
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

#[derive(Serialize, Debug, Deserialize)]
pub struct SubmitTaskArgs {
    /// The name of the group this task is submitted to
    pub group_name: Option<String>,
    /// The tags of the task, used to filter workers to execute the task
    pub tags: HashSet<String>,
    /// The labels of the task, used for querying tasks
    pub labels: HashSet<String>,
    /// The timeout of the task.
    pub timeout: std::time::Duration,
    /// The priority of the task.
    pub priority: i32,
    /// The specification of the task
    pub task_spec: TaskSpec,
}

#[derive(Serialize, Debug, Deserialize, Args)]
pub struct SubmitTaskCmdArgs {
    /// The name of the group this task is submitted to
    #[arg(short = 'g', long = "group")]
    pub group_name: Option<String>,
    /// The tags of the task, used to filter workers to execute the task
    #[arg(short, long, num_args = 0.., value_delimiter = ',')]
    pub tags: Vec<String>,
    /// The labels of the task, used for querying tasks
    #[arg(short, long, num_args = 0.., value_delimiter = ',')]
    pub labels: Vec<String>,
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
    /// The UUID and the state of the task to watch before triggering this task.
    /// Should specify it as `UUID,STATE`, e.g. `123e4567-e89b-12d3-a456-426614174000,ExecSpawned`.
    #[arg(long, value_parser = parse_watch_task::<Uuid, TaskExecState>)]
    pub watch: Option<(Uuid, TaskExecState)>,
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

#[derive(Serialize, Debug, Deserialize, Args)]
pub struct GetTaskArgs {
    /// The UUID of the task
    pub uuid: Uuid,
}

#[derive(Serialize, Debug, Deserialize, Args)]
pub struct GetArtifactCmdArgs {
    /// The UUID of the artifact
    pub uuid: Uuid,
    /// The content type of the artifact
    #[arg(value_enum)]
    pub content_type: ArtifactContentType,
    /// Specify the directory to download the artifact
    #[arg(short, long = "output")]
    pub output_path: Option<String>,
}

#[derive(Serialize, Debug, Deserialize, Args)]
pub struct GetTasksArgs {
    /// The username of the creator who submitted the tasks
    #[arg(short, long)]
    pub creator: Option<String>,
    /// The name of the group the tasks belong to
    #[arg(short, long)]
    pub group: Option<String>,
    /// The tags of the tasks
    #[arg(short, long, num_args = 0.., value_delimiter = ',')]
    pub tags: Vec<String>,
    /// The labels of the tasks
    #[arg(short, long, num_args = 0.., value_delimiter = ',')]
    pub labels: Vec<String>,
    /// The state of the tasks
    #[arg(short, long)]
    pub state: Option<TaskState>,
    /// The limit of the tasks to query
    #[arg(long)]
    pub limit: Option<u64>,
    /// The offset of the tasks to query
    #[arg(long)]
    pub offset: Option<u64>,
}

#[derive(Serialize, Debug, Deserialize)]
pub struct GetArtifactArgs {
    /// The UUID of the artifact
    pub uuid: Uuid,
    /// The content type of the artifact
    pub content_type: ArtifactContentType,
    /// Specify the path to download the artifact
    pub output_path: PathBuf,
}

#[derive(Serialize, Debug, Deserialize, Args)]
pub struct GetAttachmentCmdArgs {
    /// The group of the attachment belongs to
    pub group_name: String,
    /// The key of the attachment
    pub key: String,
    /// Specify the path to download the artifact
    #[arg(short, long = "output")]
    pub output_path: Option<String>,
}

#[derive(Serialize, Debug, Deserialize)]
pub struct GetAttachmentArgs {
    /// The group of the attachment belongs to
    pub group_name: String,
    /// The key of the attachment
    pub key: String,
    /// Specify the path to download the artifact
    pub output_path: PathBuf,
}

#[derive(Serialize, Debug, Deserialize, Args)]
pub struct UploadAttachmentArgs {
    /// The group of the attachment uploaded to
    #[arg(short = 'g', long = "group")]
    pub group_name: Option<String>,
    /// The path of the local file to upload
    pub local_file: PathBuf,
    /// The key of the attachment uploaded to. If not specified, the filename will be used.
    pub key: Option<String>,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            coordinator_addr: Url::parse(&format!("http://{}", DEFAULT_COORDINATOR_ADDR)).unwrap(),
            credential_path: None,
            user: None,
            password: None,
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

impl From<SubmitTaskCmdArgs> for ClientCommand {
    fn from(args: SubmitTaskCmdArgs) -> Self {
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

impl From<GetArtifactCmdArgs> for GetCommands {
    fn from(args: GetArtifactCmdArgs) -> Self {
        Self::Artifact(args)
    }
}

impl From<GetArtifactCmdArgs> for GetArgs {
    fn from(args: GetArtifactCmdArgs) -> Self {
        Self {
            command: GetCommands::Artifact(args),
        }
    }
}

impl From<GetArtifactCmdArgs> for ClientCommand {
    fn from(args: GetArtifactCmdArgs) -> Self {
        Self::Get(args.into())
    }
}

impl From<GetArtifactCmdArgs> for GetArtifactArgs {
    fn from(args: GetArtifactCmdArgs) -> Self {
        let output_path = args
            .output_path
            .map(|dir| {
                let dir = Path::new(&dir);
                if dir.is_dir() {
                    let file_name = args.content_type.to_string();
                    dir.join(file_name)
                } else {
                    dir.to_path_buf()
                }
            })
            .unwrap_or_else(|| {
                let file_name = args.content_type.to_string();
                Path::new("").join(file_name)
            });
        Self {
            uuid: args.uuid,
            content_type: args.content_type,
            output_path,
        }
    }
}

impl From<GetAttachmentCmdArgs> for GetCommands {
    fn from(args: GetAttachmentCmdArgs) -> Self {
        Self::Attachment(args)
    }
}

impl From<GetAttachmentCmdArgs> for GetArgs {
    fn from(args: GetAttachmentCmdArgs) -> Self {
        Self {
            command: GetCommands::Attachment(args),
        }
    }
}

impl From<GetAttachmentCmdArgs> for ClientCommand {
    fn from(args: GetAttachmentCmdArgs) -> Self {
        Self::Get(args.into())
    }
}

impl From<GetAttachmentCmdArgs> for GetAttachmentArgs {
    fn from(args: GetAttachmentCmdArgs) -> Self {
        let output_path = args
            .output_path
            .map(|dir| {
                let dir = Path::new(&dir);
                if dir.is_dir() {
                    let file_name = args.key.clone();
                    dir.join(file_name)
                } else {
                    dir.to_path_buf()
                }
            })
            .unwrap_or_else(|| {
                let file_name = args.key.clone();
                Path::new("").join(file_name)
            });
        Self {
            group_name: args.group_name,
            key: args.key,
            output_path,
        }
    }
}

impl From<GetTasksArgs> for GetCommands {
    fn from(args: GetTasksArgs) -> Self {
        Self::Tasks(args)
    }
}

impl From<GetTasksArgs> for GetArgs {
    fn from(args: GetTasksArgs) -> Self {
        Self {
            command: GetCommands::Tasks(args),
        }
    }
}

impl From<GetTasksArgs> for ClientCommand {
    fn from(args: GetTasksArgs) -> Self {
        Self::Get(args.into())
    }
}

impl From<SubmitTaskCmdArgs> for SubmitTaskArgs {
    fn from(args: SubmitTaskCmdArgs) -> Self {
        let task_spec = TaskSpec::new(
            args.command,
            args.envs,
            args.resources,
            args.terminal_output,
            args.watch,
        );
        Self {
            group_name: args.group_name,
            tags: args.tags.into_iter().collect(),
            labels: args.labels.into_iter().collect(),
            timeout: args.timeout,
            priority: args.priority,
            task_spec,
        }
    }
}

impl From<GetTasksArgs> for TasksQueryReq {
    fn from(args: GetTasksArgs) -> Self {
        Self {
            creator_username: args.creator,
            group_name: args.group,
            tags: if args.tags.is_empty() {
                None
            } else {
                Some(args.tags.into_iter().collect())
            },
            labels: if args.labels.is_empty() {
                None
            } else {
                Some(args.labels.into_iter().collect())
            },
            state: args.state,
            limit: args.limit,
            offset: args.offset,
        }
    }
}

impl From<UploadAttachmentArgs> for ClientCommand {
    fn from(args: UploadAttachmentArgs) -> Self {
        Self::Upload(args)
    }
}
