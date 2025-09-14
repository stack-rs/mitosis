use clap::{Args, Subcommand};
use serde::Serialize;

use super::WorkerConfigCli;

#[derive(Args, Debug, Serialize)]
#[command(rename_all = "kebab-case")]
pub struct ManagerConfigCli {
    #[command(subcommand)]
    pub command: ManagerCommand,
}

#[derive(Subcommand, Debug, Clone, Serialize)]
pub enum ManagerCommand {
    /// Show status of all running workers
    Status,
    /// Spawn new workers
    Spawn {
        /// Number of workers to spawn
        count: u32,
        #[command(flatten)]
        worker_config: Box<WorkerConfigCli>,
    },
    /// Kill all running workers
    Kill,
}
