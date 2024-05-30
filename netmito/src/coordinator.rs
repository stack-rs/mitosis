use std::net::SocketAddr;

use tokio_util::sync::CancellationToken;

use crate::api::router;
use crate::config::{CoordinatorConfig, CoordinatorConfigCli, InfraPool};
use crate::migration::{Migrator, MigratorTrait};
use crate::service::s3::create_bucket;
use crate::service::worker::{restore_workers, WorkerHeartbeatQueue, WorkerTaskQueue};
use crate::signal::shutdown_signal;

pub struct MitoCoordinator {
    pub infra_pool: InfraPool,
    pub worker_task_queue: WorkerTaskQueue,
    pub worker_heartbeat_queue: WorkerHeartbeatQueue,
    pub cancel_token: CancellationToken,
}

impl MitoCoordinator {
    pub async fn setup(cli: &CoordinatorConfigCli) -> crate::error::Result<Self> {
        tracing::debug!("Coordinator is setting up");
        let config = CoordinatorConfig::new(cli)?;
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
            infra_pool.db.clone(),
            worker_heartbeat_queue_rx,
        );

        // Setup s3 storage
        let bucket_name = "mitosis-attachments";
        create_bucket(&infra_pool.s3, bucket_name).await?;
        let bucket_name = "mitosis-artifacts";
        create_bucket(&infra_pool.s3, bucket_name).await?;

        // Setup database
        Migrator::up(&infra_pool.db, None).await?;

        Ok(Self {
            infra_pool,
            worker_task_queue,
            worker_heartbeat_queue,
            cancel_token,
        })
    }

    pub async fn run(self) -> crate::error::Result<()> {
        tracing::debug!("Coordinator is running");
        let MitoCoordinator {
            infra_pool,
            mut worker_task_queue,
            mut worker_heartbeat_queue,
            cancel_token,
        } = self;
        let task_queue_hd = tokio::spawn(async move { worker_task_queue.run().await });
        let heartbeat_hd = tokio::spawn(async move { worker_heartbeat_queue.run().await });
        restore_workers(&infra_pool).await?;
        let app = router(infra_pool);
        let addr = crate::config::SERVER_CONFIG
            .get()
            .ok_or(crate::error::Error::Custom(
                "server config not found".to_string(),
            ))?
            .bind;
        let listener = tokio::net::TcpListener::bind(addr).await?;
        tracing::info!("Coordinator is listening on: {}", addr);
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(shutdown_signal())
        .await
        .unwrap();
        tracing::info!("Coordinator shutdown signal received");
        cancel_token.cancel();
        task_queue_hd.await?;
        heartbeat_hd.await?;
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        Ok(())
    }
}
