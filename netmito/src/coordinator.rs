use std::net::SocketAddr;
use std::path::PathBuf;

use argon2::password_hash::rand_core::OsRng;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use crate::api::router;
use crate::config::{CoordinatorConfig, CoordinatorConfigCli, InfraPool};
use crate::migration::{Migrator, MigratorTrait};
use crate::service::s3::{setup_buckets, ARTIFACTS_BUCKET, ATTACHMENTS_BUCKET};
use crate::service::worker::{restore_workers, HeartbeatQueue, TaskDispatcher};
use crate::signal::shutdown_signal;

pub struct MitoCoordinator {
    pub infra_pool: InfraPool,
    pub worker_task_queue: TaskDispatcher,
    pub worker_heartbeat_queue: HeartbeatQueue,
    pub cancel_token: CancellationToken,
    pub log_dir: PathBuf,
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

        let (worker_task_queue_tx, worker_task_queue_rx) = tokio::sync::mpsc::unbounded_channel();
        let (worker_heartbeat_queue_tx, worker_heartbeat_queue_rx) =
            tokio::sync::mpsc::unbounded_channel();

        // Setup worker task queue
        let worker_task_queue =
            config.build_worker_task_queue(cancel_token.clone(), worker_task_queue_rx);

        // Setup infra pool
        let infra_pool = config
            .build_infra_pool(worker_task_queue_tx, worker_heartbeat_queue_tx)
            .await?;

        // Setup worker heartbeat queue
        let worker_heartbeat_queue = config.build_worker_heartbeat_queue(
            cancel_token.clone(),
            infra_pool.clone(),
            worker_heartbeat_queue_rx,
        );

        // Setup s3 storage
        // List all buckets and create if not exist
        setup_buckets(
            &infra_pool.s3,
            [ATTACHMENTS_BUCKET, ARTIFACTS_BUCKET]
                .into_iter()
                .map(String::from)
                .collect(),
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
            cancel_token,
            log_dir,
        })
    }

    pub async fn run(self) -> crate::error::Result<()> {
        tracing::debug!("Coordinator is running");
        let MitoCoordinator {
            infra_pool,
            mut worker_task_queue,
            mut worker_heartbeat_queue,
            cancel_token,
            ..
        } = self;
        let task_queue_hd = tokio::spawn(async move { worker_task_queue.run().await });
        let heartbeat_hd = tokio::spawn(async move { worker_heartbeat_queue.run().await });
        restore_workers(&infra_pool).await?;
        let app = router(infra_pool, cancel_token.clone());
        let addr = crate::config::SERVER_CONFIG
            .get()
            .ok_or(crate::error::Error::Custom(
                "server config not found".to_string(),
            ))?
            .bind;
        let listener = tokio::net::TcpListener::bind(addr).await?;
        tracing::info!("Coordinator is listening on: {}", addr);
        if let Err(e) = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(shutdown_signal(cancel_token.clone()))
        .await
        {
            tracing::error!("Server error: {}", e);
        }
        tracing::info!("Coordinator shutdown signal received");
        cancel_token.cancel();
        task_queue_hd.await?;
        heartbeat_hd.await?;
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        Ok(())
    }
}
