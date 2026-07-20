use std::{collections::HashMap, path::PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::entity::{content::ArtifactContentType, state::TaskExecState};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ExecSpec {
    pub args: Vec<String>,
    #[serde(default)]
    pub envs: HashMap<String, String>,
    #[serde(default)]
    pub resources: Vec<RemoteResourceDownload>,
    /// Execution timeout in seconds. If unset the task runs without a deadline
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<i64>,
    #[serde(default)]
    pub terminal_output: bool,
}

/// Task-specific execution options (not part of the core `ExecSpec`).
#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct TaskExecOptions {
    /// Watch another task's state before executing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watch: Option<(Uuid, TaskExecState)>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RemoteResourceDownloadResp {
    pub url: String,
    pub size: i64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ResourceDownloadInfo {
    pub size: i64,
    pub local_path: PathBuf,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum RemoteResource {
    Artifact {
        uuid: Uuid,
        content_type: ArtifactContentType,
    },
    Attachment {
        key: String,
    },
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RemoteResourceDownload {
    pub remote_file: RemoteResource,
    /// The relative local file path of the resource downloaded to at the cache directory.
    /// Will append the path to the worker's working directory.
    pub local_path: PathBuf,
}

impl ExecSpec {
    pub fn new<T, I, P, Q, V, D>(
        args: I,
        envs: P,
        files: V,
        timeout: D,
        terminal_output: bool,
    ) -> Self
    where
        I: IntoIterator<Item = T>,
        T: Into<String>,
        P: IntoIterator<Item = Q>,
        Q: Into<(String, String)>,
        V: IntoIterator<Item = RemoteResourceDownload>,
        D: Into<Option<std::time::Duration>>,
    {
        Self {
            args: args.into_iter().map(Into::into).collect(),
            envs: envs.into_iter().map(Into::into).collect(),
            resources: files.into_iter().collect(),
            timeout: timeout.into().map(|d| d.as_secs() as i64),
            terminal_output,
        }
    }
}
