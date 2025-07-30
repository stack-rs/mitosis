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
        role::{GroupWorkerRole, UserGroupRole},
        state::{TaskExecState, TaskState},
    },
    schema::{
        AttachmentsQueryReq, ChangeTaskReq, RemoteResourceDownload, RemoveGroupWorkerRoleReq,
        RemoveUserGroupRoleReq, ReplaceWorkerTagsReq, TaskSpec, TasksQueryReq,
        UpdateGroupWorkerRoleReq, UpdateTaskLabelsReq, UpdateUserGroupRoleReq, WorkersQueryReq,
    },
};

use super::coordinator::DEFAULT_COORDINATOR_ADDR;

#[derive(Deserialize, Serialize, Debug)]
pub struct ClientConfig {
    pub coordinator_addr: Url,
    pub credential_path: Option<RelativePathBuf>,
    pub user: Option<String>,
    pub password: Option<String>,
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
    /// Admin operations, including shutdown the coordinator, chaning user password, etc.
    Admin(AdminArgs),
    /// Authenticate the user
    Auth(AuthArgs),
    /// Create a new user or group
    Create(CreateArgs),
    /// Get the info of task, attachment, worker or group, or query a list of them subject to the
    /// filters. Download attachment and artifact is also supported.
    ///
    /// Query for users is not implemented yet
    Get(GetArgs),
    /// Submit a task
    Submit(SubmitTaskCmdArgs),
    /// Upload an artifact or attachment
    Upload(UploadArgs),
    /// Manage a task, a user, a group or a worker
    Manage(ManageArgs),
    /// Quit the client's interactive mode
    Quit,
}

#[derive(Serialize, Debug, Deserialize, Args)]
pub struct AdminArgs {
    #[command(subcommand)]
    pub command: AdminCommands,
}

#[derive(Serialize, Debug, Deserialize, Args)]
pub struct AuthArgs {
    /// The username of the user
    pub username: Option<String>,
    /// The password of the user
    pub password: Option<String>,
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

#[derive(Serialize, Debug, Deserialize, Args)]
pub struct UploadArgs {
    #[command(subcommand)]
    pub command: UploadCommands,
}

#[derive(Serialize, Debug, Deserialize, Args)]
pub struct ManageArgs {
    #[command(subcommand)]
    pub command: ManageCommands,
}

#[derive(Serialize, Debug, Deserialize, Args)]
pub struct ShutdownArgs {
    pub secret: String,
}

#[derive(Subcommand, Serialize, Debug, Deserialize)]
pub enum AdminCommands {
    /// Change the password of a user
    Password(ChangePasswordArgs),
    /// Shutdown the coordinator
    Shutdown(ShutdownArgs),
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
    /// Query a list of tasks subject to the filter
    Tasks(GetTasksArgs),
    /// Get the metadata of an attachment
    AttachmentMeta(GetAttachmentMetaArgs),
    /// Query a list of attachments subject to the filter
    Attachments(GetAttachmentsArgs),
    /// Get the info of a worker
    Worker(GetWorkerArgs),
    /// Query a list of workers subject to the filter
    Workers(GetWorkersArgs),
    /// Get the information of a group
    Group(GetGroupArgs),
    /// Get all groups the user has access to
    Groups,
    /// Download an artifact of a task
    Artifact(GetArtifactCmdArgs),
    /// Download an attachment of a group
    Attachment(GetAttachmentCmdArgs),
}

#[derive(Subcommand, Serialize, Debug, Deserialize)]
pub enum UploadCommands {
    /// Upload an artifact to a task
    Artifact(UploadArtifactArgs),
    /// Upload an attachment to a group
    Attachment(UploadAttachmentArgs),
}

#[derive(Subcommand, Serialize, Debug, Deserialize)]
pub enum ManageCommands {
    /// Manage a worker
    Worker(ManageWorkerArgs),
    /// Manage a task
    Task(ManageTaskArgs),
    /// Manage a group
    Group(ManageGroupArgs),
    /// Manage a user (change password)
    User(ManageUserArgs),
}

#[derive(Serialize, Debug, Deserialize, Args)]
pub struct ChangePasswordArgs {
    /// The name of the user
    pub username: Option<String>,
    /// The new password of the user
    pub new_password: Option<String>,
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
    /// Only output the URL without downloading the attachment
    #[arg(long)]
    pub no_download: bool,
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
    /// Whether to output the verbose information of workers
    #[arg(short, long)]
    pub verbose: bool,
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
    /// Only output the URL without downloading the attachment
    #[arg(long)]
    pub no_download: bool,
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
pub struct GetAttachmentsArgs {
    /// The name of the group the attachments belong to
    #[arg(short, long)]
    pub group: Option<String>,
    /// The prefix of the key of the attachments
    #[arg(short, long = "key")]
    pub key_prefix: Option<String>,
    /// The limit of the tasks to query
    #[arg(long)]
    pub limit: Option<u64>,
    /// The offset of the tasks to query
    #[arg(long)]
    pub offset: Option<u64>,
}

#[derive(Serialize, Debug, Deserialize, Args)]
pub struct GetAttachmentMetaArgs {
    /// The group of the attachment belongs to
    pub group_name: String,
    /// The key of the attachment
    pub key: String,
}

#[derive(Serialize, Debug, Deserialize, Args)]
pub struct GetWorkerArgs {
    /// The UUID of the worker
    pub uuid: Uuid,
}

#[derive(Serialize, Debug, Deserialize, Args)]
pub struct GetWorkersArgs {
    /// The name of the group has access to the workers
    #[arg(short, long)]
    pub group: Option<String>,
    /// The role of the group on the workers
    #[arg(short, long, num_args = 0.., value_delimiter = ',')]
    pub role: Vec<GroupWorkerRole>,
    /// The tags of the workers
    #[arg(short, long, num_args = 0.., value_delimiter = ',')]
    pub tags: Vec<String>,
    /// The username of the creator
    #[arg(long)]
    pub creator: Option<String>,
    /// Whether to output the verbose information of workers
    #[arg(short, long)]
    pub verbose: bool,
}

#[derive(Serialize, Debug, Deserialize, Args)]
pub struct GetGroupArgs {
    /// The name of the group
    pub group: String,
}

#[derive(Serialize, Debug, Deserialize, Args)]
pub struct UploadArtifactArgs {
    /// The UUID of the artifact
    pub uuid: Uuid,
    /// The content type of the artifact
    #[arg(value_enum)]
    pub content_type: ArtifactContentType,
    /// The path of the local file to upload
    pub local_file: PathBuf,
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

#[derive(Serialize, Debug, Deserialize, Args)]
pub struct ManageWorkerArgs {
    /// The UUID of the worker
    pub uuid: Uuid,
    #[command(subcommand)]
    pub command: ManageWorkerCommands,
}

#[derive(Subcommand, Serialize, Debug, Deserialize)]
pub enum ManageWorkerCommands {
    /// Cancel a worker
    Cancel(CancelWorkerArgs),
    /// Replace tags of a worker
    UpdateTags(ReplaceWorkerTagsArgs),
    /// Update the roles of groups to a worker
    UpdateRoles(UpdateWorkerGroupArgs),
    /// Remove the accessibility of groups from a worker
    RemoveRoles(RemoveWorkerGroupArgs),
}

#[derive(Serialize, Debug, Deserialize, Args)]
pub struct CancelWorkerArgs {
    /// Whether to force the worker to shutdown.
    /// If not specified, the worker will be try to shutdown gracefully
    #[arg(short, long)]
    pub force: bool,
}

#[derive(Serialize, Debug, Deserialize, Args)]
pub struct ReplaceWorkerTagsArgs {
    /// The tags to replace
    #[arg(num_args = 0..)]
    pub tags: Vec<String>,
}

#[derive(Serialize, Debug, Deserialize, Args)]
pub struct UpdateWorkerGroupArgs {
    /// The name and role of the group on the worker to update
    #[arg(num_args = 0.., value_parser = parse_key_val_colon::<String, GroupWorkerRole>)]
    pub roles: Vec<(String, GroupWorkerRole)>,
}

#[derive(Serialize, Debug, Deserialize, Args)]
pub struct RemoveWorkerGroupArgs {
    /// The name of the group to update
    #[arg(num_args = 0..)]
    pub groups: Vec<String>,
}

#[derive(Serialize, Debug, Deserialize, Args)]
pub struct ManageTaskArgs {
    /// The UUID of the task
    pub uuid: Uuid,
    #[command(subcommand)]
    pub command: ManageTaskCommands,
}

#[derive(Subcommand, Serialize, Debug, Deserialize)]
pub enum ManageTaskCommands {
    /// Cancel a task
    Cancel,
    /// Replace labels of a task
    UpdateLabels(UpdateTaskLabelsArgs),
    /// Update the spec of a task
    Change(ChangeTaskArgs),
}

#[derive(Serialize, Debug, Deserialize, Args)]
pub struct UpdateTaskLabelsArgs {
    /// The labels to replace
    #[arg(num_args = 0..)]
    pub tags: Vec<String>,
}

#[derive(Serialize, Debug, Deserialize, Args)]
pub struct ChangeTaskArgs {
    /// The tags of the task, used to filter workers to execute the task
    #[arg(short, long, num_args = 0.., value_delimiter = ',')]
    pub tags: Vec<String>,
    /// The timeout of the task.
    #[arg(long, value_parser = humantime_serde::re::humantime::parse_duration)]
    pub timeout: Option<std::time::Duration>,
    /// The priority of the task.
    #[arg(short, long)]
    pub priority: Option<i32>,
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

#[derive(Serialize, Debug, Deserialize, Args)]
pub struct ManageGroupArgs {
    /// The name of the group
    pub group: String,
    #[command(subcommand)]
    pub command: ManageGroupCommands,
}

#[derive(Serialize, Debug, Deserialize, Args)]
pub struct ManageUserArgs {
    /// The name of the user
    pub username: Option<String>,
    /// The original password of the user
    pub orig_password: Option<String>,
    /// The new password of the user
    pub new_password: Option<String>,
}

#[derive(Subcommand, Serialize, Debug, Deserialize)]
pub enum ManageGroupCommands {
    /// Update the roles of users to a group
    Update(UpdateUserGroupArgs),
    /// Remove the accessibility of users from a group
    Remove(RemoveUserGroupArgs),
}

#[derive(Serialize, Debug, Deserialize, Args)]
pub struct UpdateUserGroupArgs {
    /// The username and role of the user to the group
    #[arg(num_args = 0.., value_parser = parse_key_val_colon::<String, UserGroupRole>)]
    pub roles: Vec<(String, UserGroupRole)>,
}

#[derive(Serialize, Debug, Deserialize, Args)]
pub struct RemoveUserGroupArgs {
    /// The username of the user
    #[arg(num_args = 0..)]
    pub users: Vec<String>,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            coordinator_addr: Url::parse(&format!("http://{DEFAULT_COORDINATOR_ADDR}")).unwrap(),
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

impl From<AdminArgs> for ClientCommand {
    fn from(args: AdminArgs) -> Self {
        Self::Admin(args)
    }
}

impl From<AuthArgs> for ClientCommand {
    fn from(args: AuthArgs) -> Self {
        Self::Auth(args)
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

impl From<ChangePasswordArgs> for AdminCommands {
    fn from(args: ChangePasswordArgs) -> Self {
        Self::Password(args)
    }
}

impl From<ChangePasswordArgs> for AdminArgs {
    fn from(args: ChangePasswordArgs) -> Self {
        Self {
            command: AdminCommands::Password(args),
        }
    }
}

impl From<ChangePasswordArgs> for ClientCommand {
    fn from(args: ChangePasswordArgs) -> Self {
        Self::Admin(args.into())
    }
}

impl From<ShutdownArgs> for AdminCommands {
    fn from(args: ShutdownArgs) -> Self {
        Self::Shutdown(args)
    }
}

impl From<ShutdownArgs> for AdminArgs {
    fn from(args: ShutdownArgs) -> Self {
        Self {
            command: AdminCommands::Shutdown(args),
        }
    }
}

impl From<ShutdownArgs> for ClientCommand {
    fn from(args: ShutdownArgs) -> Self {
        Self::Admin(args.into())
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

impl From<ChangeTaskArgs> for ChangeTaskReq {
    fn from(args: ChangeTaskArgs) -> Self {
        let task_spec = if args.command.is_empty() {
            None
        } else {
            Some(TaskSpec::new(
                args.command,
                args.envs,
                args.resources,
                args.terminal_output,
                args.watch,
            ))
        };
        Self {
            tags: if args.tags.is_empty() {
                None
            } else {
                Some(args.tags.into_iter().collect())
            },
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

impl From<GetAttachmentsArgs> for GetCommands {
    fn from(args: GetAttachmentsArgs) -> Self {
        Self::Attachments(args)
    }
}

impl From<GetAttachmentsArgs> for GetArgs {
    fn from(args: GetAttachmentsArgs) -> Self {
        Self {
            command: GetCommands::Attachments(args),
        }
    }
}

impl From<GetAttachmentsArgs> for ClientCommand {
    fn from(args: GetAttachmentsArgs) -> Self {
        Self::Get(args.into())
    }
}

impl From<GetAttachmentsArgs> for AttachmentsQueryReq {
    fn from(args: GetAttachmentsArgs) -> Self {
        Self {
            group_name: args.group,
            key_prefix: args.key_prefix,
            limit: args.limit,
            offset: args.offset,
        }
    }
}

impl From<GetAttachmentMetaArgs> for GetCommands {
    fn from(args: GetAttachmentMetaArgs) -> Self {
        Self::AttachmentMeta(args)
    }
}

impl From<GetAttachmentMetaArgs> for GetArgs {
    fn from(args: GetAttachmentMetaArgs) -> Self {
        Self {
            command: GetCommands::AttachmentMeta(args),
        }
    }
}

impl From<GetAttachmentMetaArgs> for ClientCommand {
    fn from(args: GetAttachmentMetaArgs) -> Self {
        Self::Get(args.into())
    }
}

impl From<GetWorkersArgs> for GetCommands {
    fn from(args: GetWorkersArgs) -> Self {
        Self::Workers(args)
    }
}

impl From<GetWorkersArgs> for GetArgs {
    fn from(args: GetWorkersArgs) -> Self {
        Self {
            command: GetCommands::Workers(args),
        }
    }
}

impl From<GetWorkersArgs> for ClientCommand {
    fn from(args: GetWorkersArgs) -> Self {
        Self::Get(args.into())
    }
}

impl From<GetWorkersArgs> for WorkersQueryReq {
    fn from(args: GetWorkersArgs) -> Self {
        Self {
            group_name: args.group,
            role: if args.role.is_empty() {
                None
            } else {
                Some(args.role.into_iter().collect())
            },
            tags: if args.tags.is_empty() {
                None
            } else {
                Some(args.tags.into_iter().collect())
            },
            creator_username: args.creator,
        }
    }
}

impl From<GetGroupArgs> for GetCommands {
    fn from(args: GetGroupArgs) -> Self {
        Self::Group(args)
    }
}

impl From<GetGroupArgs> for GetArgs {
    fn from(args: GetGroupArgs) -> Self {
        Self {
            command: GetCommands::Group(args),
        }
    }
}

impl From<GetGroupArgs> for ClientCommand {
    fn from(args: GetGroupArgs) -> Self {
        Self::Get(args.into())
    }
}

impl From<GetWorkerArgs> for GetCommands {
    fn from(args: GetWorkerArgs) -> Self {
        Self::Worker(args)
    }
}

impl From<GetWorkerArgs> for GetArgs {
    fn from(args: GetWorkerArgs) -> Self {
        Self {
            command: GetCommands::Worker(args),
        }
    }
}

impl From<GetWorkerArgs> for ClientCommand {
    fn from(args: GetWorkerArgs) -> Self {
        Self::Get(args.into())
    }
}

impl From<UploadArtifactArgs> for UploadCommands {
    fn from(args: UploadArtifactArgs) -> Self {
        Self::Artifact(args)
    }
}

impl From<UploadArtifactArgs> for UploadArgs {
    fn from(args: UploadArtifactArgs) -> Self {
        Self {
            command: UploadCommands::Artifact(args),
        }
    }
}

impl From<UploadArtifactArgs> for ClientCommand {
    fn from(args: UploadArtifactArgs) -> Self {
        Self::Upload(args.into())
    }
}

impl From<UploadAttachmentArgs> for UploadCommands {
    fn from(args: UploadAttachmentArgs) -> Self {
        Self::Attachment(args)
    }
}

impl From<UploadAttachmentArgs> for UploadArgs {
    fn from(args: UploadAttachmentArgs) -> Self {
        Self {
            command: UploadCommands::Attachment(args),
        }
    }
}

impl From<UploadAttachmentArgs> for ClientCommand {
    fn from(args: UploadAttachmentArgs) -> Self {
        Self::Upload(args.into())
    }
}

impl From<ManageWorkerArgs> for ManageCommands {
    fn from(args: ManageWorkerArgs) -> Self {
        Self::Worker(args)
    }
}

impl From<CancelWorkerArgs> for ManageWorkerCommands {
    fn from(args: CancelWorkerArgs) -> Self {
        Self::Cancel(args)
    }
}

impl From<(Uuid, CancelWorkerArgs)> for ManageWorkerArgs {
    fn from(args: (Uuid, CancelWorkerArgs)) -> Self {
        Self {
            uuid: args.0,
            command: ManageWorkerCommands::Cancel(args.1),
        }
    }
}

impl From<(Uuid, CancelWorkerArgs)> for ManageCommands {
    fn from(args: (Uuid, CancelWorkerArgs)) -> Self {
        Self::Worker(args.into())
    }
}

impl From<(Uuid, CancelWorkerArgs)> for ManageArgs {
    fn from(args: (Uuid, CancelWorkerArgs)) -> Self {
        Self {
            command: ManageCommands::Worker(args.into()),
        }
    }
}

impl From<(Uuid, CancelWorkerArgs)> for ClientCommand {
    fn from(args: (Uuid, CancelWorkerArgs)) -> Self {
        Self::Manage(args.into())
    }
}

impl From<ReplaceWorkerTagsArgs> for ManageWorkerCommands {
    fn from(args: ReplaceWorkerTagsArgs) -> Self {
        Self::UpdateTags(args)
    }
}

impl From<(Uuid, ReplaceWorkerTagsArgs)> for ManageWorkerArgs {
    fn from(args: (Uuid, ReplaceWorkerTagsArgs)) -> Self {
        Self {
            uuid: args.0,
            command: ManageWorkerCommands::UpdateTags(args.1),
        }
    }
}

impl From<(Uuid, ReplaceWorkerTagsArgs)> for ManageCommands {
    fn from(args: (Uuid, ReplaceWorkerTagsArgs)) -> Self {
        Self::Worker(args.into())
    }
}

impl From<(Uuid, ReplaceWorkerTagsArgs)> for ManageArgs {
    fn from(args: (Uuid, ReplaceWorkerTagsArgs)) -> Self {
        Self {
            command: ManageCommands::Worker(args.into()),
        }
    }
}

impl From<(Uuid, ReplaceWorkerTagsArgs)> for ClientCommand {
    fn from(args: (Uuid, ReplaceWorkerTagsArgs)) -> Self {
        Self::Manage(args.into())
    }
}

impl From<ReplaceWorkerTagsArgs> for ReplaceWorkerTagsReq {
    fn from(args: ReplaceWorkerTagsArgs) -> Self {
        Self {
            tags: args.tags.into_iter().collect(),
        }
    }
}

impl From<UpdateWorkerGroupArgs> for ManageWorkerCommands {
    fn from(args: UpdateWorkerGroupArgs) -> Self {
        Self::UpdateRoles(args)
    }
}

impl From<(Uuid, UpdateWorkerGroupArgs)> for ManageWorkerArgs {
    fn from(args: (Uuid, UpdateWorkerGroupArgs)) -> Self {
        Self {
            uuid: args.0,
            command: ManageWorkerCommands::UpdateRoles(args.1),
        }
    }
}

impl From<(Uuid, UpdateWorkerGroupArgs)> for ManageCommands {
    fn from(args: (Uuid, UpdateWorkerGroupArgs)) -> Self {
        Self::Worker(args.into())
    }
}

impl From<(Uuid, UpdateWorkerGroupArgs)> for ManageArgs {
    fn from(args: (Uuid, UpdateWorkerGroupArgs)) -> Self {
        Self {
            command: ManageCommands::Worker(args.into()),
        }
    }
}

impl From<(Uuid, UpdateWorkerGroupArgs)> for ClientCommand {
    fn from(args: (Uuid, UpdateWorkerGroupArgs)) -> Self {
        Self::Manage(args.into())
    }
}

impl From<UpdateWorkerGroupArgs> for UpdateGroupWorkerRoleReq {
    fn from(args: UpdateWorkerGroupArgs) -> Self {
        Self {
            relations: args.roles.into_iter().collect(),
        }
    }
}

impl From<RemoveWorkerGroupArgs> for ManageWorkerCommands {
    fn from(args: RemoveWorkerGroupArgs) -> Self {
        Self::RemoveRoles(args)
    }
}

impl From<(Uuid, RemoveWorkerGroupArgs)> for ManageWorkerArgs {
    fn from(args: (Uuid, RemoveWorkerGroupArgs)) -> Self {
        Self {
            uuid: args.0,
            command: ManageWorkerCommands::RemoveRoles(args.1),
        }
    }
}

impl From<(Uuid, RemoveWorkerGroupArgs)> for ManageCommands {
    fn from(args: (Uuid, RemoveWorkerGroupArgs)) -> Self {
        Self::Worker(args.into())
    }
}

impl From<(Uuid, RemoveWorkerGroupArgs)> for ManageArgs {
    fn from(args: (Uuid, RemoveWorkerGroupArgs)) -> Self {
        Self {
            command: ManageCommands::Worker(args.into()),
        }
    }
}

impl From<(Uuid, RemoveWorkerGroupArgs)> for ClientCommand {
    fn from(args: (Uuid, RemoveWorkerGroupArgs)) -> Self {
        Self::Manage(args.into())
    }
}

impl From<RemoveWorkerGroupArgs> for RemoveGroupWorkerRoleReq {
    fn from(args: RemoveWorkerGroupArgs) -> Self {
        Self {
            groups: args.groups.into_iter().collect(),
        }
    }
}

impl From<ManageTaskArgs> for ManageCommands {
    fn from(args: ManageTaskArgs) -> Self {
        Self::Task(args)
    }
}

impl From<UpdateTaskLabelsArgs> for ManageTaskCommands {
    fn from(args: UpdateTaskLabelsArgs) -> Self {
        Self::UpdateLabels(args)
    }
}

impl From<(Uuid, UpdateTaskLabelsArgs)> for ManageTaskArgs {
    fn from(args: (Uuid, UpdateTaskLabelsArgs)) -> Self {
        Self {
            uuid: args.0,
            command: ManageTaskCommands::UpdateLabels(args.1),
        }
    }
}

impl From<(Uuid, UpdateTaskLabelsArgs)> for ManageCommands {
    fn from(args: (Uuid, UpdateTaskLabelsArgs)) -> Self {
        Self::Task(args.into())
    }
}

impl From<(Uuid, UpdateTaskLabelsArgs)> for ManageArgs {
    fn from(args: (Uuid, UpdateTaskLabelsArgs)) -> Self {
        Self {
            command: ManageCommands::Task(args.into()),
        }
    }
}

impl From<(Uuid, UpdateTaskLabelsArgs)> for ClientCommand {
    fn from(args: (Uuid, UpdateTaskLabelsArgs)) -> Self {
        Self::Manage(args.into())
    }
}

impl From<UpdateTaskLabelsArgs> for UpdateTaskLabelsReq {
    fn from(args: UpdateTaskLabelsArgs) -> Self {
        Self {
            labels: args.tags.into_iter().collect(),
        }
    }
}

impl From<ChangeTaskArgs> for ManageTaskCommands {
    fn from(args: ChangeTaskArgs) -> Self {
        Self::Change(args)
    }
}

impl From<(Uuid, ChangeTaskArgs)> for ManageTaskArgs {
    fn from(args: (Uuid, ChangeTaskArgs)) -> Self {
        Self {
            uuid: args.0,
            command: ManageTaskCommands::Change(args.1),
        }
    }
}

impl From<(Uuid, ChangeTaskArgs)> for ManageCommands {
    fn from(args: (Uuid, ChangeTaskArgs)) -> Self {
        Self::Task(args.into())
    }
}

impl From<(Uuid, ChangeTaskArgs)> for ManageArgs {
    fn from(args: (Uuid, ChangeTaskArgs)) -> Self {
        Self {
            command: ManageCommands::Task(args.into()),
        }
    }
}

impl From<(Uuid, ChangeTaskArgs)> for ClientCommand {
    fn from(args: (Uuid, ChangeTaskArgs)) -> Self {
        Self::Manage(args.into())
    }
}

impl From<UpdateUserGroupArgs> for ManageGroupCommands {
    fn from(args: UpdateUserGroupArgs) -> Self {
        Self::Update(args)
    }
}

impl From<(String, UpdateUserGroupArgs)> for ManageGroupArgs {
    fn from(args: (String, UpdateUserGroupArgs)) -> Self {
        Self {
            group: args.0,
            command: ManageGroupCommands::Update(args.1),
        }
    }
}

impl From<(String, UpdateUserGroupArgs)> for ManageCommands {
    fn from(args: (String, UpdateUserGroupArgs)) -> Self {
        Self::Group(args.into())
    }
}

impl From<(String, UpdateUserGroupArgs)> for ManageArgs {
    fn from(args: (String, UpdateUserGroupArgs)) -> Self {
        Self {
            command: ManageCommands::Group(args.into()),
        }
    }
}

impl From<(String, UpdateUserGroupArgs)> for ClientCommand {
    fn from(args: (String, UpdateUserGroupArgs)) -> Self {
        Self::Manage(args.into())
    }
}

impl From<UpdateUserGroupArgs> for UpdateUserGroupRoleReq {
    fn from(args: UpdateUserGroupArgs) -> Self {
        Self {
            relations: args.roles.into_iter().collect(),
        }
    }
}

impl From<RemoveUserGroupArgs> for ManageGroupCommands {
    fn from(args: RemoveUserGroupArgs) -> Self {
        Self::Remove(args)
    }
}

impl From<(String, RemoveUserGroupArgs)> for ManageGroupArgs {
    fn from(args: (String, RemoveUserGroupArgs)) -> Self {
        Self {
            group: args.0,
            command: ManageGroupCommands::Remove(args.1),
        }
    }
}

impl From<(String, RemoveUserGroupArgs)> for ManageCommands {
    fn from(args: (String, RemoveUserGroupArgs)) -> Self {
        Self::Group(args.into())
    }
}

impl From<(String, RemoveUserGroupArgs)> for ManageArgs {
    fn from(args: (String, RemoveUserGroupArgs)) -> Self {
        Self {
            command: ManageCommands::Group(args.into()),
        }
    }
}

impl From<(String, RemoveUserGroupArgs)> for ClientCommand {
    fn from(args: (String, RemoveUserGroupArgs)) -> Self {
        Self::Manage(args.into())
    }
}

impl From<RemoveUserGroupArgs> for RemoveUserGroupRoleReq {
    fn from(args: RemoveUserGroupArgs) -> Self {
        Self {
            users: args.users.into_iter().collect(),
        }
    }
}

impl From<ManageUserArgs> for ManageCommands {
    fn from(args: ManageUserArgs) -> Self {
        Self::User(args)
    }
}

impl From<ManageUserArgs> for ManageArgs {
    fn from(args: ManageUserArgs) -> Self {
        Self {
            command: ManageCommands::User(args),
        }
    }
}

impl From<ManageUserArgs> for ClientCommand {
    fn from(args: ManageUserArgs) -> Self {
        Self::Manage(args.into())
    }
}
