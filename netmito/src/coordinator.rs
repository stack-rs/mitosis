use std::net::SocketAddr;
use std::path::PathBuf;

use argon2::password_hash::rand_core::OsRng;
use rand::Rng;
use tokio_util::{sync::CancellationToken, task::TaskTracker};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use crate::api::router;
use crate::config::{CoordinatorConfig, CoordinatorConfigCli, InfraPool};
use crate::migration::{Migrator, MigratorTrait};
use crate::service::agent_heartbeat::AgentHeartbeatQueue;
use crate::service::s3::setup_buckets;
use crate::service::suite::auto_close_inactive_suites;
use crate::service::suite_task_dispatcher::SuiteTaskDispatcher;
use crate::service::worker::{restore_workers, HeartbeatQueue, TaskDispatcher};
use crate::signal::shutdown_signal;
use crate::ws::connection::AgentWsRouter;

pub struct MitoCoordinator {
    pub infra_pool: InfraPool,
    pub worker_task_queue: TaskDispatcher,
    pub worker_heartbeat_queue: HeartbeatQueue,
    pub agent_heartbeat_queue: AgentHeartbeatQueue,
    pub suite_task_dispatcher: SuiteTaskDispatcher,
    pub ws_router: AgentWsRouter,
    pub cancel_token: CancellationToken,
    pub log_dir: PathBuf,
    pub suite_auto_close_timeout_secs: i64,
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

        let (worker_task_queue_tx, worker_task_queue_rx) = crossfire::mpsc::unbounded_async();
        let (worker_heartbeat_queue_tx, worker_heartbeat_queue_rx) =
            crossfire::mpsc::unbounded_async();
        let (agent_heartbeat_queue_tx, agent_heartbeat_queue_rx) =
            crossfire::mpsc::unbounded_async();
        let (ws_tx, ws_rx) = crossfire::mpsc::unbounded_async();
        let (suite_dispatcher_tx, suite_dispatcher_rx) = crossfire::mpsc::unbounded_async();

        // Setup worker task queue
        let worker_task_queue =
            config.build_worker_task_queue(cancel_token.clone(), worker_task_queue_rx);

        // Setup WS router
        let ws_router = config.build_agent_ws_router(cancel_token.clone(), ws_rx);

        // Setup suite task dispatcher
        let suite_task_dispatcher =
            config.build_suite_task_dispatcher(cancel_token.clone(), suite_dispatcher_rx);

        // Setup infra pool
        let infra_pool = config
            .build_infra_pool(
                worker_task_queue_tx,
                worker_heartbeat_queue_tx,
                agent_heartbeat_queue_tx,
                ws_tx,
                suite_dispatcher_tx,
            )
            .await?;

        // Setup worker heartbeat queue
        let worker_heartbeat_queue = config.build_worker_heartbeat_queue(
            cancel_token.clone(),
            infra_pool.clone(),
            worker_heartbeat_queue_rx,
        );

        // Setup agent heartbeat queue
        let agent_heartbeat_queue = config.build_agent_heartbeat_queue(
            cancel_token.clone(),
            infra_pool.clone(),
            agent_heartbeat_queue_rx,
        );

        let suite_auto_close_timeout_secs = config.suite_auto_close_timeout.as_secs() as i64;

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
            suite_task_dispatcher,
            ws_router,
            cancel_token,
            log_dir,
            suite_auto_close_timeout_secs,
        })
    }

    pub async fn run(self) -> crate::error::Result<()> {
        tracing::debug!("Coordinator is running");
        let MitoCoordinator {
            infra_pool,
            mut worker_task_queue,
            mut worker_heartbeat_queue,
            mut agent_heartbeat_queue,
            mut suite_task_dispatcher,
            mut ws_router,
            cancel_token,
            suite_auto_close_timeout_secs,
            ..
        } = self;

        // Create TaskTracker to manage background tasks
        let task_tracker = TaskTracker::new();

        // Spawn auto-close task suite background task
        {
            let db = infra_pool.db.clone();
            let cancel = cancel_token.clone();
            task_tracker.spawn(async move {
                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => {
                            tracing::info!("Auto-close task_suite job stopped");
                            break;
                        }
                        _ = tokio::time::sleep(std::time::Duration::from_secs(30 + rand::rng().random_range(0..30))) => {
                            if let Err(e) = auto_close_inactive_suites(&db, suite_auto_close_timeout_secs).await {
                                tracing::error!("Failed to auto-close inactive suites: {}", e);
                            }
                        }
                    }
                }
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
            agent_heartbeat_queue.run().await;
            tracing::info!("Agent heartbeat queue stopped");
        });

        task_tracker.spawn(async move {
            suite_task_dispatcher.run().await;
            tracing::info!("Suite task dispatcher stopped");
        });

        task_tracker.spawn(async move {
            ws_router.run().await;
            tracing::info!("Agent WebSocket Router stopped");
        });

        restore_workers(&infra_pool).await?;

        // Notify all agents about coordinator restart
        crate::service::agent::notify_all_agents_of_restart(&infra_pool).await?;

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
