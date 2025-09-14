use crate::build::CLAP_LONG_VERSION;
use clap::{Parser, Subcommand};
use netmito::{
    client::MitoClient,
    config::{ClientConfigCli, CoordinatorConfigCli, ManagerConfigCli, WorkerConfigCli},
    coordinator::MitoCoordinator,
    manager::MitoManager,
    worker::MitoWorker,
};
use shadow_rs::shadow;

shadow!(build);

/// Main entry point for the mitosis command-line tool.
#[derive(Parser, Debug)]
#[command(version, about, long_about = None, long_version = CLAP_LONG_VERSION)]
#[command(propagate_version = true)]
struct Arguments {
    #[command(subcommand)]
    mode: Mode,
}

#[derive(Subcommand, Debug)]
enum Mode {
    /// Run the mitosis coordinator.
    Coordinator(CoordinatorConfigCli),
    /// Run a mitosis worker.
    Worker(WorkerConfigCli),
    /// Run a mitosis client.
    Client(ClientConfigCli),
    /// Manage mitosis workers.
    Manager(ManagerConfigCli),
}

fn main() {
    let args = Arguments::parse();
    match args.mode {
        Mode::Coordinator(coordinator_cli) => {
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    MitoCoordinator::main(coordinator_cli).await;
                });
        }
        Mode::Worker(worker_cli) => {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    MitoWorker::main(worker_cli).await;
                });
        }
        Mode::Client(client_cli) => {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    MitoClient::main(client_cli).await;
                });
        }
        Mode::Manager(manager_cli) => {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    MitoManager::main(manager_cli).await;
                });
        }
    }
}
