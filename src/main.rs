use crate::build::CLAP_LONG_VERSION;
use clap::{Parser, Subcommand};
use netmito::{
    agent::MitoAgent,
    client::MitoClient,
    config::{
        manager::ManagerCommand, AgentConfigCli, ClientConfigCli, CoordinatorConfigCli,
        ManagerConfigCli, WorkerConfigCli,
    },
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
    /// The path of the config file
    #[arg(long, global = true)]
    config: Option<String>,
    #[command(subcommand)]
    mode: Mode,
}

#[derive(Subcommand, Debug)]
enum Mode {
    /// Run the mitosis coordinator.
    Coordinator(CoordinatorConfigCli),
    /// Run a mitosis worker.
    Worker(WorkerConfigCli),
    /// Run a mitosis agent, which executes task suites.
    Agent(AgentConfigCli),
    /// Run a mitosis client.
    Client(ClientConfigCli),
    /// Manage mitosis workers.
    Manager(ManagerConfigCli),
}

impl Mode {
    /// Forward the root config path to the selected runtime mode.
    fn apply_config(&mut self, config: Option<String>) {
        match self {
            Self::Coordinator(cli) => cli.config = config,
            Self::Worker(cli) => cli.config = config,
            Self::Agent(cli) => cli.config = config,
            Self::Client(cli) => cli.config = config,
            Self::Manager(cli) => {
                if let ManagerCommand::Spawn { worker_config, .. } = &mut cli.command {
                    worker_config.config = config;
                }
            }
        }
    }
}

fn main() {
    let Arguments { config, mut mode } = Arguments::parse();
    mode.apply_config(config);

    match mode {
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
        Mode::Agent(agent_cli) => {
            // Multi-threaded: the agent runs its main loop, a WebSocket reader,
            // and a suite runner concurrently.
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    MitoAgent::main(agent_cli).await;
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
