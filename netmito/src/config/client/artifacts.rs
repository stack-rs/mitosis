use std::path::PathBuf;

use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::entity::content::ArtifactContentType;

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
