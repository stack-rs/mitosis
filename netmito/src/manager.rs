use indicatif::{ProgressBar, ProgressState, ProgressStyle};
use std::env;
use std::process::Command;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use crate::config::{manager::ManagerCommand, ManagerConfigCli, WorkerConfigCli};
use crate::error;

pub struct MitoManager;

impl MitoManager {
    pub async fn main(cli: ManagerConfigCli) {
        tracing_subscriber::registry()
            .with(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| "netmito=info".into()),
            )
            .with(tracing_subscriber::fmt::layer())
            .init();

        match &cli.command {
            ManagerCommand::Status => {
                if let Err(e) = Self::status().await {
                    tracing::error!("Failed to get worker status: {}", e);
                    std::process::exit(1);
                }
            }
            ManagerCommand::Spawn {
                count,
                worker_config,
            } => {
                if let Err(e) = Self::spawn_workers(*count, (**worker_config).clone()).await {
                    tracing::error!("Failed to spawn workers: {}", e);
                    std::process::exit(1);
                }
            }
            ManagerCommand::Kill => {
                if let Err(e) = Self::kill_workers().await {
                    tracing::error!("Failed to kill workers: {}", e);
                    std::process::exit(1);
                }
            }
        }
    }

    pub async fn status() -> crate::error::Result<()> {
        // Use ps with grep to get worker processes and count them in one go
        let output = Command::new("ps")
            .args(["-aux"])
            .output()
            .map_err(|e| error::Error::Custom(format!("Failed to run ps: {}", e)))?;

        if !output.status.success() {
            println!("Total: 0");
            return Ok(());
        }

        let ps_output = String::from_utf8_lossy(&output.stdout);
        let mut worker_lines = Vec::new();

        for line in ps_output.lines() {
            if line.contains("mito worker") && !line.contains("grep") && !line.contains("ps -aux") {
                worker_lines.push(line);
            }
        }

        let worker_cnt = worker_lines.len();
        for line in worker_lines {
            println!("{}", line);
        }
        println!("Total: {}", worker_cnt);

        Ok(())
    }

    pub async fn spawn_workers(
        count: u32,
        worker_config: WorkerConfigCli,
    ) -> crate::error::Result<()> {
        if count == 0 {
            tracing::warn!("Cannot spawn 0 workers");
            return Ok(());
        }

        // Get the current executable path
        let current_exe = std::env::current_exe().map_err(|e| {
            error::Error::Custom(format!("Failed to get current executable path: {}", e))
        })?;

        let pb = ProgressBar::new(count as u64);
        pb.set_style(ProgressStyle::with_template("{msg}\n{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({eta})")
        .unwrap()
        .with_key("eta", |state: &ProgressState, w: &mut dyn std::fmt::Write| write!(w, "{:.1}s", state.eta().as_secs_f64()).unwrap())
            .progress_chars("=>-"),
        );
        pb.set_message("Spawning workers...");
        for _ in 0..count {
            pb.inc(1);
            let mut cmd = Command::new(&current_exe);
            cmd.arg("worker");

            // Add worker configuration arguments
            if let Some(config) = &worker_config.config {
                cmd.arg("--config").arg(config);
            }
            if let Some(coordinator) = &worker_config.coordinator_addr {
                cmd.arg("--coordinator").arg(coordinator);
            }
            if let Some(polling_interval) = &worker_config.polling_interval {
                cmd.arg("--polling-interval").arg(polling_interval);
            }
            if let Some(heartbeat_interval) = &worker_config.heartbeat_interval {
                cmd.arg("--heartbeat-interval").arg(heartbeat_interval);
            }
            if let Some(credential_path) = &worker_config.credential_path {
                cmd.arg("--credential-path").arg(credential_path);
            }
            if let Some(user) = &worker_config.user {
                cmd.arg("--user").arg(user);
            }
            if let Some(password) = &worker_config.password {
                cmd.arg("--password").arg(password);
            }
            if !worker_config.groups.is_empty() {
                cmd.arg("--groups").arg(worker_config.groups.join(","));
            }
            if !worker_config.tags.is_empty() {
                cmd.arg("--tags").arg(worker_config.tags.join(","));
            }
            if !worker_config.labels.is_empty() {
                cmd.arg("--labels").arg(worker_config.labels.join(","));
            }
            if let Some(log_path) = &worker_config.log_path {
                cmd.arg("--log-path").arg(log_path);
            }
            if worker_config.file_log {
                cmd.arg("--file-log");
            }
            if worker_config.shared_log {
                cmd.arg("--shared-log");
            }
            if let Some(lifetime) = &worker_config.lifetime {
                cmd.arg("--lifetime").arg(lifetime);
            }
            if worker_config.retain {
                cmd.arg("--retain");
            }
            if worker_config.skip_redis {
                cmd.arg("--skip-redis");
            }

            // Set environment variables for file logging
            cmd.env("NO_COLOR", "1");
            let file_log_level =
                env::var("MITO_FILE_LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
            cmd.env("MITO_FILE_LOG_LEVEL", file_log_level);

            // Spawn the process detached (no stdout/stderr capture)
            cmd.stdout(std::process::Stdio::null());
            cmd.stderr(std::process::Stdio::null());

            cmd.spawn().map_err(|e| {
                error::Error::Custom(format!("Failed to spawn worker process: {}", e))
            })?;

            std::io::Write::flush(&mut std::io::stdout()).unwrap();
        }

        pb.finish_with_message(format!("Spawned {} workers in total", count));

        // Check current worker count by counting processes
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await; // Give processes time to start
        let output = Command::new("pgrep")
            .args(["-f", "-c", "mito worker"])
            .output()
            .map_err(|e| error::Error::Custom(format!("Failed to run pgrep: {}", e)))?;

        if output.status.success() {
            let count_str = String::from_utf8_lossy(&output.stdout);
            let current_count = count_str.trim().parse::<u32>().unwrap_or(0);
            println!("Current worker numbers: {}", current_count);
        } else {
            println!("Failed to get current worker count");
        }

        Ok(())
    }

    pub async fn kill_workers() -> crate::error::Result<()> {
        let output = Command::new("pkill")
            .args(["-f", "mito worker"])
            .output()
            .map_err(|e| error::Error::Custom(format!("Failed to run pkill: {}", e)))?;

        if output.status.success() {
            println!("All mito workers have been killed");
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.is_empty() {
                println!("No mito workers found to kill");
            } else {
                tracing::error!("pkill failed: {}", stderr);
                return Err(error::Error::Custom(format!(
                    "Failed to kill workers: {}",
                    stderr
                )));
            }
        }

        Ok(())
    }
}
