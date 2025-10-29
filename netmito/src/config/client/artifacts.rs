use std::path::PathBuf;

use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    entity::{content::ArtifactContentType, state::TaskState},
    schema::{ArtifactsDownloadByFilterReq, ArtifactsDownloadByUuidsReq},
};

#[derive(Serialize, Debug, Deserialize, Args, derive_more::From, Clone)]
pub struct ArtifactsArgs {
    #[command(subcommand)]
    pub command: ArtifactsCommands,
}

#[derive(Subcommand, Serialize, Debug, Deserialize, derive_more::From, Clone)]
pub enum ArtifactsCommands {
    /// Delete an artifact of a task
    Delete(DeleteArtifactArgs),
    /// Upload an artifact to a task
    Upload(UploadArtifactArgs),
    /// Download an artifact of a task
    Download(DownloadArtifactArgs),
    /// Batch download artifacts by filter criteria
    DownloadByFilter(DownloadArtifactsByFilterArgs),
    /// Batch download artifacts by task UUIDs
    DownloadByList(DownloadArtifactsByListArgs),
}

#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct DeleteArtifactArgs {
    /// The UUID of the artifact
    pub uuid: Uuid,
    /// The content type of the artifact
    #[arg(value_enum)]
    pub content_type: ArtifactContentType,
}

#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct UploadArtifactArgs {
    /// The path of the local file to upload
    pub local_file: PathBuf,
    /// The UUID of the artifact
    pub uuid: Uuid,
    /// The content type of the artifact
    #[arg(value_enum)]
    pub content_type: ArtifactContentType,
    /// Whether to show progress bar when downloading
    #[arg(long)]
    pub pb: bool,
}

#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct DownloadArtifactArgs {
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
    /// Whether to show progress bar when downloading
    #[arg(long)]
    pub pb: bool,
}

#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct DownloadArtifactsByFilterArgs {
    /// The content type of the artifacts
    #[arg(value_enum)]
    pub content_type: ArtifactContentType,
    /// The username of the creator who submitted the tasks
    #[arg(short, long, num_args = 0.., value_delimiter = ',')]
    pub creators: Vec<String>,
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
    #[arg(short, long, num_args = 0.., value_delimiter = ',')]
    pub state: Vec<TaskState>,
    /// The exit status of the tasks, support operators like `=`(default), `!=`, `<`, `<=`, `>`, `>=`
    #[arg(short, long)]
    pub exit_status: Option<String>,
    /// The priority of the tasks, support operators like `=`(default), `!=`, `<`, `<=`, `>`, `>=`
    #[arg(short, long)]
    pub priority: Option<String>,
    /// Specify the directory to download artifacts
    #[arg(short, long = "output")]
    pub output_dir: Option<PathBuf>,
    /// Only output the URLs without downloading
    #[arg(long)]
    pub no_download: bool,
    /// Whether to show progress bar when downloading
    #[arg(long)]
    pub pb: bool,
    /// Number of concurrent downloads (default: 1)
    #[arg(long, default_value_t = 1)]
    pub concurrent: usize,
}

#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct DownloadArtifactsByListArgs {
    /// The content type of the artifacts
    #[arg(value_enum)]
    pub content_type: ArtifactContentType,
    /// The UUIDs of the tasks
    #[arg(num_args = 1..)]
    pub uuids: Vec<Uuid>,
    /// Specify the directory to download artifacts
    #[arg(short, long = "output")]
    pub output_dir: Option<PathBuf>,
    /// Only output the URLs without downloading
    #[arg(long)]
    pub no_download: bool,
    /// Whether to show progress bar when downloading
    #[arg(long)]
    pub pb: bool,
    /// Number of concurrent downloads (default: 1)
    #[arg(long, default_value_t = 1)]
    pub concurrent: usize,
}

impl From<DownloadArtifactsByFilterArgs> for ArtifactsDownloadByFilterReq {
    fn from(args: DownloadArtifactsByFilterArgs) -> Self {
        Self {
            creator_usernames: if args.creators.is_empty() {
                None
            } else {
                Some(args.creators.into_iter().collect())
            },
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
            states: if args.state.is_empty() {
                None
            } else {
                Some(args.state.into_iter().collect())
            },
            exit_status: args.exit_status,
            priority: args.priority,
            content_type: args.content_type,
        }
    }
}

impl From<DownloadArtifactsByListArgs> for ArtifactsDownloadByUuidsReq {
    fn from(args: DownloadArtifactsByListArgs) -> Self {
        Self {
            uuids: args.uuids,
            content_type: args.content_type,
        }
    }
}
