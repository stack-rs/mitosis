use std::net::SocketAddr;
use std::path::PathBuf;

use argon2::password_hash::rand_core::OsRng;
use tokio_util::{sync::CancellationToken, task::TaskTracker};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use crate::api::router;
use crate::config::{CoordinatorConfig, CoordinatorConfigCli, InfraPool};
use crate::migration::{Migrator, MigratorTrait};
use crate::service::agent::heartbeat::AgentHeartbeatQueue;
use crate::service::s3::setup_buckets;
use crate::service::suite::sweep_inactive_suites;
use crate::service::worker::{restore_workers, HeartbeatQueue, TaskDispatcher};
use crate::signal::shutdown_signal;
use crate::ws::AgentWsRouter;

/// How often the idle-suite sweep runs, given the configured idle window.
///
/// Half the window, so a drained suite lingers in `Open` at most about one and a
/// half windows past its last task — and an agent holds its job for no longer
/// than that. Scaling with the window is what keeps a small one meaningful: a
/// fixed period would swamp a 5-second window and waste queries on an hour-long
/// one. Clamped at both ends so neither extreme misbehaves.
fn suite_sweep_period(idle_window: std::time::Duration) -> std::time::Duration {
    (idle_window / 2).clamp(
        std::time::Duration::from_secs(1),
        std::time::Duration::from_secs(30),
    )
}

pub struct MitoCoordinator {
    pub infra_pool: InfraPool,
    pub worker_task_queue: TaskDispatcher,
    pub worker_heartbeat_queue: HeartbeatQueue,
    pub agent_heartbeat_queue: AgentHeartbeatQueue,
    pub ws_router: AgentWsRouter,
    pub cancel_token: CancellationToken,
    pub log_dir: PathBuf,
    /// How long a suite may go without a new task before the sweep settles it
    /// out of `Open`.
    pub suite_auto_close_timeout: std::time::Duration,
}

impl MitoCoordinator {
    pub async fn main(cli: CoordinatorConfigCli) {
        tracing_subscriber::registry()
            .with(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| "netmito=info".into()),
            )
            .with(tracing_subscriber::fmt::layer())
            .init();
        match CoordinatorConfig::new(&cli) {
            Ok(config) => {
                let _guards = config.setup_tracing_subscriber().inspect_err(|e| {
                    tracing::error!("{}", e);
                });
                match Self::setup(config).await {
                    Ok(coordinator) => {
                        if let Err(e) = coordinator.run().await {
                            tracing::error!("{}", e);
                        }
                    }
                    Err(e) => {
                        tracing::error!("{}", e);
                    }
                }
            }
            Err(e) => {
                tracing::error!("{}", e);
            }
        }
    }

    pub async fn setup(config: CoordinatorConfig) -> crate::error::Result<Self> {
        tracing::debug!("Coordinator is setting up");
        // Setup configurations
        let server_config = config.build_server_config()?;
        crate::config::SERVER_CONFIG
            .set(server_config)
            .map_err(|_| crate::error::Error::Custom("set server config failed".to_string()))?;
        let decoding_key = config.build_jwt_decoding_key().await?;
        crate::config::DECODING_KEY
            .set(decoding_key)
            .map_err(|_| crate::error::Error::Custom("set decoding key failed".to_string()))?;
        let encoding_key = config.build_jwt_encoding_key().await?;
        crate::config::ENCODING_KEY
            .set(encoding_key)
            .map_err(|_| crate::error::Error::Custom("set encoding key failed".to_string()))?;
        let init_admin_user = config.build_admin_user()?;
        crate::config::INIT_ADMIN_USER
            .set(init_admin_user)
            .map_err(|_| crate::error::Error::Custom("set init admin user failed".to_string()))?;
        let redis_connection_info = config.build_redis_connection_info().await?;
        if let Some(info) = redis_connection_info {
            crate::config::REDIS_CONNECTION_INFO
                .set(info)
                .map_err(|_| {
                    crate::error::Error::Custom("set redis connection info failed".to_string())
                })?;
        }
        let shutdown_secret = argon2::password_hash::SaltString::generate(&mut OsRng).to_string();
        tracing::warn!("Set random shutdown secret: {}", shutdown_secret);
        crate::config::SHUTDOWN_SECRET
            .set(shutdown_secret)
            .map_err(|_| crate::error::Error::Custom("set shutdown secret failed".to_string()))?;
        let cancel_token = CancellationToken::new();

        #[cfg(not(feature = "crossfire-channel"))]
        let (worker_task_queue_tx, worker_task_queue_rx) = tokio::sync::mpsc::unbounded_channel();
        #[cfg(feature = "crossfire-channel")]
        let (worker_task_queue_tx, worker_task_queue_rx) = crossfire::mpsc::unbounded_async();
        #[cfg(not(feature = "crossfire-channel"))]
        let (worker_heartbeat_queue_tx, worker_heartbeat_queue_rx) =
            tokio::sync::mpsc::unbounded_channel();
        #[cfg(feature = "crossfire-channel")]
        let (worker_heartbeat_queue_tx, worker_heartbeat_queue_rx) =
            crossfire::mpsc::unbounded_async();

        let (agent_heartbeat_queue_tx, agent_heartbeat_queue_rx) = crate::channel::unbounded();
        let (ws_router_tx, ws_router_rx) = crate::channel::unbounded();

        // Setup worker task queue
        let worker_task_queue =
            config.build_worker_task_queue(cancel_token.clone(), worker_task_queue_rx);

        // Setup infra pool
        let infra_pool = config
            .build_infra_pool(
                worker_task_queue_tx,
                worker_heartbeat_queue_tx,
                agent_heartbeat_queue_tx,
                ws_router_tx,
            )
            .await?;

        // Setup worker heartbeat queue
        let worker_heartbeat_queue = config.build_worker_heartbeat_queue(
            cancel_token.clone(),
            infra_pool.clone(),
            worker_heartbeat_queue_rx,
        );

        // Setup the agent-side actors: liveness tracking and the notification router
        let agent_heartbeat_queue = config.build_agent_heartbeat_queue(
            cancel_token.clone(),
            infra_pool.clone(),
            agent_heartbeat_queue_rx,
        );
        let ws_router = config.build_ws_router(cancel_token.clone(), ws_router_rx);
        let suite_auto_close_timeout = config.suite_auto_close_timeout;

        // Setup s3 storage
        // List all buckets and create if not exist
        setup_buckets(
            &infra_pool.s3,
            vec![
                infra_pool.attachments_bucket.clone(),
                infra_pool.artifacts_bucket.clone(),
            ],
        )
        .await?;

        // Setup database
        Migrator::up(&infra_pool.db, None).await?;

        let mut log_dir = dirs::cache_dir().ok_or(crate::error::Error::Custom(
            "Cache dir not found".to_string(),
        ))?;
        log_dir.push("mitosis");
        log_dir.push("coordinator");
        tokio::fs::create_dir_all(&log_dir).await?;

        Ok(Self {
            infra_pool,
            worker_task_queue,
            worker_heartbeat_queue,
            agent_heartbeat_queue,
            ws_router,
            cancel_token,
            log_dir,
            suite_auto_close_timeout,
        })
    }

    pub async fn run(self) -> crate::error::Result<()> {
        tracing::debug!("Coordinator is running");
        let MitoCoordinator {
            infra_pool,
            mut worker_task_queue,
            mut worker_heartbeat_queue,
            mut agent_heartbeat_queue,
            mut ws_router,
            cancel_token,
            suite_auto_close_timeout,
            ..
        } = self;

        // Create TaskTracker to manage background tasks
        let task_tracker = TaskTracker::new();

        // Settle idle suites out of `Open`. Jittered so a fleet of coordinators
        // against one database does not sweep in lockstep.
        {
            let db = infra_pool.db.clone();
            let cancel = cancel_token.clone();
            let period = suite_sweep_period(suite_auto_close_timeout);
            task_tracker.spawn(async move {
                loop {
                    // Up to half a period of jitter, so several coordinators on
                    // one database do not all sweep on the same tick.
                    let delay = period
                        + std::time::Duration::from_millis(rand::Rng::random_range(
                            &mut rand::rng(),
                            0..=(period.as_millis() as u64 / 2),
                        ));
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => break,
                        _ = tokio::time::sleep(delay) => {
                            if let Err(e) = sweep_inactive_suites(&db, suite_auto_close_timeout).await {
                                tracing::error!("Failed to sweep inactive suites: {e}");
                            }
                        }
                    }
                }
                tracing::info!("Suite sweep stopped");
            });
        }

        // Spawn background tasks using TaskTracker
        task_tracker.spawn(async move {
            worker_task_queue.run().await;
            tracing::info!("Task dispatcher stopped");
        });

        task_tracker.spawn(async move {
            worker_heartbeat_queue.run().await;
            tracing::info!("Heartbeat queue stopped");
        });

        task_tracker.spawn(async move {
            ws_router.run().await;
        });

        task_tracker.spawn(async move {
            agent_heartbeat_queue.run().await;
        });

        restore_workers(&infra_pool).await?;
        // Agents keep their rows across a coordinator restart, so tell them this
        // is a new boot and their notification sequence starts over.
        crate::service::agent::notify_agents_of_restart(&infra_pool).await?;
        let app = router(infra_pool, cancel_token.clone());
        let addr = crate::config::SERVER_CONFIG
            .get()
            .ok_or(crate::error::Error::Custom(
                "server config not found".to_string(),
            ))?
            .bind;
        let listener = tokio::net::TcpListener::bind(addr).await?;
        tracing::info!("Coordinator is listening on: {}", addr);

        // Create shutdown signal future that cancels the token
        let shutdown_future = async move {
            shutdown_signal(cancel_token.clone()).await;
            tracing::info!("Shutdown signal received, cancelling tasks...");
            cancel_token.cancel();
        };

        // Run the HTTP server with graceful shutdown
        if let Err(e) = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(shutdown_future)
        .await
        {
            tracing::error!("Server error: {}", e);
        }

        tracing::info!("HTTP server stopped, waiting for background tasks...");

        // Close the TaskTracker to indicate no more tasks will be added
        task_tracker.close();

        // Wait for all background tasks to complete with timeout
        let wait_result =
            tokio::time::timeout(std::time::Duration::from_secs(30), task_tracker.wait()).await;

        match wait_result {
            Ok(()) => {
                tracing::info!("All background tasks completed successfully");
            }
            Err(_) => {
                tracing::warn!(
                    "Background tasks did not complete within 30 seconds, proceeding with shutdown"
                );
            }
        }

        tracing::info!("Coordinator shutdown complete");
        Ok(())
    }
}
