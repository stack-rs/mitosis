use std::path::PathBuf;

use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};

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
}

#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct DeleteAttachmentArgs {
    /// The group of the attachment belongs to
    pub group_name: String,
    /// The key of the attachment
    pub key: String,
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
    /// The prefix of the key of the attachments
    #[arg(short, long = "key")]
    pub key_prefix: Option<String>,
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
    pub group_name: String,
    /// The key of the attachment
    pub key: String,
}
