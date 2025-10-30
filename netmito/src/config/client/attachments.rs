use std::path::PathBuf;

use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};

use crate::schema::{
    AttachmentsDownloadByFilterReq, AttachmentsDownloadByKeysReq, AttachmentsQueryReq,
};

#[derive(Serialize, Debug, Deserialize, Args, derive_more::From, Clone)]
pub struct AttachmentsArgs {
    #[command(subcommand)]
    pub command: AttachmentsCommands,
}

#[derive(Subcommand, Serialize, Debug, Deserialize, derive_more::From, Clone)]
pub enum AttachmentsCommands {
    /// Delete an attachment from a group
    Delete(DeleteAttachmentArgs),
    /// Upload an attachment to a group
    Upload(UploadAttachmentArgs),
    /// Get the metadata of an attachment
    Get(GetAttachmentMetaArgs),
    /// Download an attachment of a group
    Download(DownloadAttachmentArgs),
    /// Query attachments subject to the filter
    Query(QueryAttachmentsArgs),
    /// Batch download attachments by filter criteria
    DownloadByFilter(DownloadAttachmentsByFilterArgs),
    /// Batch download attachments by keys
    DownloadByList(DownloadAttachmentsByListArgs),
}

#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct DeleteAttachmentArgs {
    /// The group of the attachment belongs to
    #[arg(short = 'g', long = "group")]
    pub group_name: Option<String>,
    /// The key of the attachment
    pub key: String,
    /// Parse the command with smart mode. Will try parse the first component before first `/` in
    /// key (if specified) as group-name (if not specified)
    #[arg(short, long)]
    pub smart: bool,
}

#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct UploadAttachmentArgs {
    /// The path of the local file to upload
    pub local_file: PathBuf,
    /// The group of the attachment uploaded to
    #[arg(short = 'g', long = "group")]
    pub group_name: Option<String>,
    /// The key of the attachment uploaded to. If not specified, the filename will be used.
    /// If the key specified is a directory (ends with '/'),
    /// the filename (final component of local file) will be appended to it.
    pub key: Option<String>,
    /// Whether to show progress bar when downloading
    #[arg(long)]
    pub pb: bool,
    /// Parse the command with smart mode. Will try parse the first component before first `/` in
    /// key (if specified) as group-name (if not specified)
    #[arg(short, long)]
    pub smart: bool,
}

#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct DownloadAttachmentArgs {
    /// The group of the attachment belongs to
    #[arg(short, long = "group")]
    pub group_name: Option<String>,
    /// The key of the attachment
    pub key: String,
    /// Specify the path to download the artifact
    #[arg(short, long = "output")]
    pub output_path: Option<String>,
    /// Only output the URL without downloading the attachment
    #[arg(long)]
    pub no_download: bool,
    /// Whether to show progress bar when downloading
    #[arg(long)]
    pub pb: bool,
    /// Try to use file name (last path component) of key
    /// as part of the output path
    #[arg(short, long)]
    pub smart: bool,
}

#[derive(Serialize, Debug, Deserialize, Clone)]
pub(crate) struct InnerDownloadAttachmentArgs {
    /// The group of the attachment belongs to
    pub(crate) group_name: String,
    /// The key of the attachment
    pub(crate) key: String,
    /// Specify the path to download the artifact
    pub(crate) output_path: PathBuf,
    /// Whether to show progress bar when downloading
    pub(crate) show_pb: bool,
}

#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct QueryAttachmentsArgs {
    /// The name of the group the attachments belong to
    #[arg(short, long)]
    pub group: Option<String>,
    /// The part of the key of the attachments
    #[arg(short, long = "key")]
    pub key: Option<String>,
    /// The limit of the tasks to query
    #[arg(long)]
    pub limit: Option<u64>,
    /// The offset of the tasks to query
    #[arg(long)]
    pub offset: Option<u64>,
    /// Only count the number of workers
    #[arg(long)]
    pub count: bool,
}

#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct GetAttachmentMetaArgs {
    /// The group of the attachment belongs to
    #[arg(short = 'g', long = "group")]
    pub group_name: Option<String>,
    /// The key of the attachment
    pub key: String,
    /// Parse the command with smart mode. Will try parse the first component before first `/` in
    /// key (if specified) as group-name (if not specified)
    #[arg(short, long)]
    pub smart: bool,
}

#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct DownloadAttachmentsByFilterArgs {
    /// The name of the group the attachments belong to
    #[arg(short, long)]
    pub group: Option<String>,
    /// The part of the key of the attachments
    #[arg(short, long = "key")]
    pub key: Option<String>,
    /// The limit of the attachments to download
    #[arg(long)]
    pub limit: Option<u64>,
    /// The offset of the attachments to download
    #[arg(long)]
    pub offset: Option<u64>,
    /// Specify the directory to download attachments
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
pub struct DownloadAttachmentsByListArgs {
    /// The name of the group the attachments belong to
    #[arg(short, long)]
    pub group: Option<String>,
    /// The keys of the attachments
    #[arg(num_args = 1..)]
    pub keys: Vec<String>,
    /// Specify the directory to download attachments
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

impl From<QueryAttachmentsArgs> for AttachmentsQueryReq {
    fn from(args: QueryAttachmentsArgs) -> Self {
        Self {
            key: args.key,
            limit: args.limit,
            offset: args.offset,
            count: args.count,
        }
    }
}

impl From<DownloadAttachmentsByFilterArgs> for AttachmentsDownloadByFilterReq {
    fn from(args: DownloadAttachmentsByFilterArgs) -> Self {
        Self {
            key: args.key,
            limit: args.limit,
            offset: args.offset,
        }
    }
}

impl From<DownloadAttachmentsByListArgs> for AttachmentsDownloadByKeysReq {
    fn from(args: DownloadAttachmentsByListArgs) -> Self {
        Self { keys: args.keys }
    }
}
