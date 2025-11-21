use crate::config::NodeManagerConfig;
use crate::entity::state::NodeManagerState;
use crate::schema::{
    CoordinatorMessage, ManagerMessage, ManagerMetrics, RegisterManagerReq,
    RegisterManagerResp,
};
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::signal;
use tokio::sync::RwLock;
use tokio::time::sleep;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{client::IntoClientRequest, Message},
};
use tracing::{error, info, warn};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use uuid::Uuid;

pub struct MitoNodeManager {
    config: NodeManagerConfig,
    manager_uuid: Option<Uuid>,
    token: Option<String>,
    state: Arc<RwLock<NodeManagerState>>,
    metrics: Arc<RwLock<ManagerMetrics>>,
    start_time: Instant,
    shutdown: Arc<RwLock<bool>>,
}

impl MitoNodeManager {
    pub async fn main(cli: crate::config::NodeManagerConfigCli) {
        // Initialize logging
        tracing_subscriber::registry()
            .with(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| format!("netmito={}", cli.log_level).into()),
            )
            .with(tracing_subscriber::fmt::layer())
            .init();

        info!("Starting Mitosis Node Manager");

        // Load configuration
        let config = match NodeManagerConfig::from_cli(&cli) {
            Ok(config) => config,
            Err(e) => {
                error!("Failed to load configuration: {}", e);
                std::process::exit(1);
            }
        };

        // Create manager instance
        let mut manager = Self {
            config,
            manager_uuid: None,
            token: None,
            state: Arc::new(RwLock::new(NodeManagerState::Idle)),
            metrics: Arc::new(RwLock::new(ManagerMetrics {
                active_workers: 0,
                total_tasks_completed: 0,
                total_tasks_failed: 0,
                current_suite_tasks_completed: 0,
                current_suite_tasks_failed: 0,
                uptime_seconds: 0,
                cpu_usage_percent: 0.0,
                memory_usage_mb: 0,
            })),
            start_time: Instant::now(),
            shutdown: Arc::new(RwLock::new(false)),
        };

        // Register with coordinator
        if let Err(e) = manager.register().await {
            error!("Failed to register with coordinator: {}", e);
            std::process::exit(1);
        }

        info!(
            "Successfully registered with coordinator. UUID: {}",
            manager.manager_uuid.unwrap()
        );

        // Setup shutdown handler
        let shutdown = manager.shutdown.clone();
        tokio::spawn(async move {
            signal::ctrl_c().await.expect("Failed to install Ctrl+C handler");
            info!("Shutdown signal received");
            *shutdown.write().await = true;
        });

        // Run main loop with reconnection logic
        if let Err(e) = manager.run().await {
            error!("Manager encountered fatal error: {}", e);
            std::process::exit(1);
        }

        info!("Node Manager shutting down gracefully");
    }

    async fn register(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        info!("Registering with coordinator at {}", self.config.coordinator_url);

        let client = reqwest::Client::new();
        let url = format!("{}/managers", self.config.coordinator_url);

        let req = RegisterManagerReq {
            tags: self.config.tags.clone(),
            labels: self.config.labels.clone(),
            groups: self.config.groups.clone(),
            lifetime: Some(self.config.token_lifetime),
        };

        let mut request = client.post(&url).json(&req);

        // Add authentication if provided
        if let (Some(user), Some(password)) = (&self.config.user, &self.config.password) {
            request = request.basic_auth(user, Some(password));
        }

        let response = request.send().await?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response.text().await.unwrap_or_else(|_| "Unknown error".to_string());
            return Err(format!("Registration failed with status {}: {}", status, error_text).into());
        }

        let resp: RegisterManagerResp = response.json().await?;

        self.manager_uuid = Some(resp.manager_uuid);
        self.token = Some(resp.token);

        info!("Registration successful. Manager UUID: {}", resp.manager_uuid);

        Ok(())
    }

    async fn run(&self) -> Result<(), Box<dyn std::error::Error>> {
        let mut backoff = self.config.reconnect_min_backoff;

        loop {
            // Check for shutdown
            if *self.shutdown.read().await {
                info!("Shutdown requested, exiting main loop");
                break;
            }

            match self.connect_and_run().await {
                Ok(_) => {
                    info!("WebSocket connection closed normally");
                    // Reset backoff on successful connection
                    backoff = self.config.reconnect_min_backoff;
                }
                Err(e) => {
                    error!("WebSocket connection error: {}. Reconnecting in {:?}", e, backoff);
                    sleep(backoff).await;

                    // Exponential backoff
                    backoff = std::cmp::min(backoff * 2, self.config.reconnect_max_backoff);
                }
            }

            // Check for shutdown again before reconnecting
            if *self.shutdown.read().await {
                break;
            }
        }

        Ok(())
    }

    async fn connect_and_run(&self) -> Result<(), Box<dyn std::error::Error>> {
        let manager_uuid = self.manager_uuid.ok_or("Manager not registered")?;
        let token = self.token.as_ref().ok_or("No auth token available")?;

        // Build WebSocket URL
        let ws_url = self.config.coordinator_url
            .replace("http://", "ws://")
            .replace("https://", "wss://");
        let ws_url = format!("{}/ws/managers", ws_url);

        info!("Connecting to WebSocket at {}", ws_url);

        // Create WebSocket request with auth header
        let mut request = ws_url.into_client_request()?;
        request.headers_mut().insert(
            "Authorization",
            format!("Bearer {}", token).parse()?,
        );

        // Connect
        let (ws_stream, _) = connect_async(request).await?;
        info!("WebSocket connected successfully");

        let (write, mut read) = ws_stream.split();
        let write = Arc::new(tokio::sync::Mutex::new(write));

        // Send initial heartbeat
        {
            let mut write_guard = write.lock().await;
            self.send_heartbeat(&mut *write_guard, manager_uuid).await?;
        }

        // Setup heartbeat task
        let heartbeat_interval = self.config.heartbeat_interval;
        let state = self.state.clone();
        let metrics = self.metrics.clone();
        let start_time = self.start_time;
        let shutdown = self.shutdown.clone();
        let write_clone = write.clone();

        let mut heartbeat_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(heartbeat_interval);
            loop {
                interval.tick().await;

                if *shutdown.read().await {
                    break;
                }

                // Update uptime
                let mut metrics_guard = metrics.write().await;
                metrics_guard.uptime_seconds = start_time.elapsed().as_secs();
                drop(metrics_guard);

                // Send heartbeat
                let current_state = *state.read().await;
                let current_metrics = metrics.read().await.clone();

                let heartbeat = ManagerMessage::Heartbeat {
                    manager_uuid,
                    state: current_state,
                    metrics: current_metrics,
                };

                let msg_json = match serde_json::to_string(&heartbeat) {
                    Ok(json) => json,
                    Err(e) => {
                        error!("Failed to serialize heartbeat: {}", e);
                        continue;
                    }
                };

                let mut write_guard = write_clone.lock().await;
                if let Err(e) = write_guard.send(Message::Text(msg_json)).await {
                    error!("Failed to send heartbeat: {}", e);
                    break;
                }
            }
        });

        // Message receive loop
        while let Some(message) = read.next().await {
            match message {
                Ok(Message::Text(text)) => {
                    if let Err(e) = self.handle_message(&text).await {
                        error!("Error handling message: {}", e);
                    }
                }
                Ok(Message::Close(_)) => {
                    info!("Received close message from coordinator");
                    break;
                }
                Ok(Message::Ping(data)) => {
                    let mut write_guard = write.lock().await;
                    if let Err(e) = write_guard.send(Message::Pong(data)).await {
                        error!("Failed to send pong: {}", e);
                        break;
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    error!("WebSocket error: {}", e);
                    break;
                }
            }

            // Check for shutdown
            if *self.shutdown.read().await {
                break;
            }
        }

        // Cleanup
        heartbeat_handle.abort();
        Ok(())
    }

    async fn send_heartbeat(
        &self,
        write: &mut futures_util::stream::SplitSink<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
            Message,
        >,
        manager_uuid: Uuid,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let current_state = *self.state.read().await;
        let mut current_metrics = self.metrics.read().await.clone();
        current_metrics.uptime_seconds = self.start_time.elapsed().as_secs();

        let heartbeat = ManagerMessage::Heartbeat {
            manager_uuid,
            state: current_state,
            metrics: current_metrics,
        };

        let msg_json = serde_json::to_string(&heartbeat)?;
        write.send(Message::Text(msg_json)).await?;
        info!("Sent initial heartbeat");

        Ok(())
    }

    async fn handle_message(&self, text: &str) -> Result<(), Box<dyn std::error::Error>> {
        let message: CoordinatorMessage = serde_json::from_str(text)?;

        match message {
            CoordinatorMessage::SuiteAssigned { suite_uuid, suite_spec } => {
                self.handle_suite_assigned(suite_uuid, suite_spec).await?;
            }
            CoordinatorMessage::TaskAvailable { request_id, task } => {
                self.handle_task_available(request_id, task).await?;
            }
            CoordinatorMessage::TaskReportAck { request_id, success, url } => {
                self.handle_task_report_ack(request_id, success, url).await?;
            }
            CoordinatorMessage::CancelTask { task_uuid, reason } => {
                self.handle_cancel_task(task_uuid, reason).await?;
            }
            CoordinatorMessage::CancelSuite { suite_uuid, reason, cancel_running_tasks } => {
                self.handle_cancel_suite(suite_uuid, reason, cancel_running_tasks).await?;
            }
            CoordinatorMessage::ConfigUpdate { lease_duration, heartbeat_interval } => {
                self.handle_config_update(lease_duration, heartbeat_interval).await?;
            }
            CoordinatorMessage::Shutdown { graceful } => {
                self.handle_shutdown(graceful).await?;
            }
        }

        Ok(())
    }

    async fn handle_suite_assigned(
        &self,
        suite_uuid: Uuid,
        suite_spec: crate::schema::TaskSuiteSpec,
    ) -> Result<(), Box<dyn std::error::Error>> {
        info!("Received suite assignment: {} ({})",
            suite_spec.name.as_deref().unwrap_or("unnamed"),
            suite_uuid
        );

        // Check if we're in Idle state
        let current_state = *self.state.read().await;
        if !current_state.is_available() {
            warn!("Received suite assignment while not idle (current state: {:?}). Ignoring.", current_state);
            return Ok(());
        }

        // Transition to Preparing state
        *self.state.write().await = NodeManagerState::Preparing;
        info!("Transitioned to Preparing state for suite {}", suite_uuid);

        // TODO: In the future, implement env_preparation hook execution here
        // For now, just transition directly to Executing
        info!("Suite preparation would happen here. Transitioning to Executing state.");
        *self.state.write().await = NodeManagerState::Executing;

        // TODO: In the future, spawn workers here based on suite_spec.worker_schedule

        Ok(())
    }

    async fn handle_task_available(
        &self,
        request_id: u64,
        task: Option<crate::schema::WorkerTaskResp>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        info!("Received task available response for request {}: {}",
            request_id,
            if task.is_some() { "task provided" } else { "no task" }
        );
        // TODO: Forward to worker via IPC
        Ok(())
    }

    async fn handle_task_report_ack(
        &self,
        request_id: u64,
        success: bool,
        url: Option<String>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        info!("Received task report ack for request {}: success={}, url={:?}",
            request_id, success, url
        );
        // TODO: Forward to worker via IPC
        Ok(())
    }

    async fn handle_cancel_task(
        &self,
        task_uuid: Uuid,
        reason: String,
    ) -> Result<(), Box<dyn std::error::Error>> {
        info!("Received cancel task request for {}: {}", task_uuid, reason);
        // TODO: Forward to worker via IPC
        Ok(())
    }

    async fn handle_cancel_suite(
        &self,
        suite_uuid: Uuid,
        reason: String,
        cancel_running_tasks: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        info!("Received cancel suite request for {}: {} (cancel_running={})",
            suite_uuid, reason, cancel_running_tasks
        );

        // Transition to Cleanup state
        *self.state.write().await = NodeManagerState::Cleanup;

        // TODO: In the future:
        // - Stop accepting new tasks
        // - Cancel running tasks if requested
        // - Run cleanup hook

        // For now, just transition back to Idle
        info!("Suite cleanup would happen here. Transitioning to Idle state.");
        *self.state.write().await = NodeManagerState::Idle;

        Ok(())
    }

    async fn handle_config_update(
        &self,
        lease_duration: Option<Duration>,
        heartbeat_interval: Option<Duration>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        info!("Received config update: lease_duration={:?}, heartbeat_interval={:?}",
            lease_duration, heartbeat_interval
        );
        // TODO: Update configuration dynamically
        Ok(())
    }

    async fn handle_shutdown(
        &self,
        graceful: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        info!("Received shutdown request (graceful={})", graceful);

        if graceful {
            // Graceful shutdown: finish current work
            info!("Performing graceful shutdown...");
            *self.state.write().await = NodeManagerState::Cleanup;
            // TODO: Finish current tasks, run cleanup
        }

        // Signal shutdown
        *self.shutdown.write().await = true;

        Ok(())
    }
}
