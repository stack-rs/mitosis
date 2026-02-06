//! Agent implementation for executing task suites
//!
//! This module implements an agent client that connects to the coordinator
//! to fetch and execute task suites. It handles:
//! - Registration with the coordinator
//! - WebSocket connection for real-time notifications
//! - HTTP API calls for suite lifecycle management
//! - Heartbeat mechanism for health reporting
//! - Basic state machine for suite execution

use std::time::Duration;

use futures::{SinkExt, StreamExt};
use speedy::{Readable, Writable};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use url::Url;
use uuid::Uuid;

use crate::config::{AgentConfig, AgentConfigCli};
use crate::entity::state::AgentState;
use crate::error;
use crate::schema::*;
use crate::service::auth::cred::get_user_credential;

pub struct MitoAgent;

/// Agent client that connects to coordinator
struct AgentClient {
    coordinator_addr: Url,
    agent_uuid: Uuid,
    token: String,
    notification_counter: u64,
    coordinator_boot_id: Option<Uuid>,
    state: AgentState,
    assigned_suite_uuid: Option<Uuid>,
    heartbeat_interval: Duration,
    http_client: reqwest::Client,
}

impl MitoAgent {
    pub async fn main(cli: AgentConfigCli) {
        tracing_subscriber::registry()
            .with(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| "netmito=info".into()),
            )
            .with(tracing_subscriber::fmt::layer())
            .init();

        match AgentConfig::new(&cli) {
            Ok(config) => {
                if let Err(e) = Self::run_agent(config).await {
                    tracing::error!("Failed to run agent: {}", e);
                    std::process::exit(1);
                }
            }
            Err(e) => {
                tracing::error!("{}", e);
                std::process::exit(1);
            }
        }
    }

    async fn run_agent(mut config: AgentConfig) -> crate::error::Result<()> {
        tracing::info!("Starting agent client");
        tracing::info!("Coordinator: {}", config.coordinator_addr);
        tracing::info!("Groups: {:?}", config.groups);
        tracing::info!("Tags: {:?}", config.tags);
        tracing::info!("Labels: {:?}", config.labels);

        // Authenticate using the shared credential system (credential file + interactive prompt)
        let http_client = reqwest::Client::new();
        let (_, user_credential) = get_user_credential(
            config.credential_path.as_ref(),
            &http_client,
            config.coordinator_addr.clone(),
            config.user.take(),
            config.password.take(),
            config.retain,
        )
        .await?;

        // Register as an agent
        let mut register_url = config.coordinator_addr.clone();
        register_url.set_path("agents");
        let req = RegisterAgentReq {
            tags: config.tags.clone(),
            labels: config.labels.clone(),
            groups: config.groups.clone(),
            lifetime: config.lifetime,
        };
        let resp = http_client
            .post(register_url.as_str())
            .bearer_auth(&user_credential)
            .json(&req)
            .send()
            .await
            .map_err(|e| {
                if e.is_request() && e.is_connect() {
                    error::RequestError::ConnectionError(config.coordinator_addr.to_string())
                } else {
                    e.into()
                }
            })?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(error::Error::Custom(format!(
                "Agent registration failed: {} - {}",
                status, body
            )));
        }

        let register_resp: RegisterAgentResp = resp.json().await.map_err(|e| {
            error::Error::Custom(format!("Failed to parse registration response: {}", e))
        })?;

        tracing::info!("Registered as agent: {}", register_resp.agent_uuid);
        tracing::info!(
            "Initial notification counter: {}",
            register_resp.notification_counter
        );

        // Build the base coordinator URL (without trailing path)
        let mut coordinator_url = config.coordinator_addr;
        coordinator_url.set_path("");

        // Create client instance
        let mut client = AgentClient {
            coordinator_addr: coordinator_url,
            agent_uuid: register_resp.agent_uuid,
            token: register_resp.token,
            notification_counter: register_resp.notification_counter,
            coordinator_boot_id: None,
            state: AgentState::Idle,
            assigned_suite_uuid: None,
            heartbeat_interval: config.heartbeat_interval,
            http_client,
        };

        // Run the main loop
        client.run().await
    }
}

impl AgentClient {
    /// Build a URL for the given API path
    fn api_url(&self, path: &str) -> Url {
        let mut url = self.coordinator_addr.clone();
        url.set_path(path);
        url
    }

    /// Build a WebSocket URL for the given API path
    fn ws_url(&self, path: &str) -> String {
        let mut url = self.coordinator_addr.clone();
        let ws_scheme = match url.scheme() {
            "https" => "wss",
            _ => "ws",
        };
        let _ = url.set_scheme(ws_scheme);
        url.set_path(path);
        url.to_string()
    }

    /// Main run loop for the agent
    async fn run(&mut self) -> crate::error::Result<()> {
        // Create cancellation token for graceful shutdown
        let cancel_token = tokio_util::sync::CancellationToken::new();
        let cancel_token_clone = cancel_token.clone();

        // Setup SIGINT handler
        tokio::spawn(async move {
            tokio::signal::ctrl_c().await.ok();
            tracing::info!("Received SIGINT, shutting down...");
            cancel_token_clone.cancel();
        });

        // Start WebSocket connection in background
        let (ws_tx, mut ws_rx) = tokio::sync::mpsc::channel::<AgentNotification>(32);
        let ws_handle = self.spawn_websocket_client(ws_tx, cancel_token.clone());

        // Heartbeat timer
        let mut heartbeat_timer = tokio::time::interval(self.heartbeat_interval);
        heartbeat_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        tracing::info!("Agent entering main loop (state: {:?})", self.state);

        loop {
            tokio::select! {
                biased;
                _ = cancel_token.cancelled() => {
                    tracing::info!("Shutdown signal received, exiting main loop");
                    break;
                }

                // Handle WebSocket notifications
                Some(notification) = ws_rx.recv() => {
                    self.handle_notification(notification).await;
                }

                // Periodic heartbeat
                _ = heartbeat_timer.tick() => {
                    if let Err(e) = self.send_heartbeat().await {
                        tracing::error!("Failed to send heartbeat: {}", e);
                    }
                }
            }

            // State machine logic
            if let Err(e) = self.process_state().await {
                tracing::error!("Error processing state: {}", e);
            }
        }

        // Cleanup: wait for websocket task to finish
        if let Some(handle) = ws_handle {
            let _ = handle.await;
        }

        tracing::info!("Agent stopped");
        Ok(())
    }

    /// Spawn WebSocket client task
    fn spawn_websocket_client(
        &self,
        notification_tx: tokio::sync::mpsc::Sender<AgentNotification>,
        cancel_token: tokio_util::sync::CancellationToken,
    ) -> Option<tokio::task::JoinHandle<()>> {
        let ws_url = self.ws_url("api/ws/agents");
        let token = self.token.clone();
        let agent_uuid = self.agent_uuid;

        Some(tokio::spawn(async move {
            loop {
                if cancel_token.is_cancelled() {
                    break;
                }

                tracing::info!("Connecting to WebSocket: {}", ws_url);

                match Self::websocket_connect(&ws_url, &token, agent_uuid, &notification_tx).await {
                    Ok(_) => {
                        tracing::info!("WebSocket connection closed normally");
                    }
                    Err(e) => {
                        tracing::error!("WebSocket connection error: {}", e);
                    }
                }

                // Reconnect after delay
                tokio::select! {
                    _ = cancel_token.cancelled() => break,
                    _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                }
            }
            tracing::info!("WebSocket client task stopped");
        }))
    }

    /// Connect to WebSocket and handle messages
    async fn websocket_connect(
        ws_url: &str,
        token: &str,
        _agent_uuid: Uuid,
        notification_tx: &tokio::sync::mpsc::Sender<AgentNotification>,
    ) -> crate::error::Result<()> {
        let (ws_stream, _) = tokio_tungstenite::connect_async(
            tokio_tungstenite::tungstenite::http::Request::builder()
                .uri(ws_url)
                .header("Authorization", format!("Bearer {}", token))
                .header("Sec-WebSocket-Version", "13")
                .header("Connection", "Upgrade")
                .header("Upgrade", "websocket")
                .header(
                    "Sec-WebSocket-Key",
                    tokio_tungstenite::tungstenite::handshake::client::generate_key(),
                )
                .body(())
                .map_err(|e| {
                    error::Error::Custom(format!("Failed to build WebSocket request: {}", e))
                })?,
        )
        .await
        .map_err(|e| error::Error::Custom(format!("WebSocket connection failed: {}", e)))?;

        tracing::info!("WebSocket connected");

        let (mut ws_write, mut ws_read) = ws_stream.split();

        // Send initial ping
        let pong_msg = AgentWsMessage::Pong {
            client_time: time::OffsetDateTime::now_utc().unix_timestamp(),
        };
        let pong_bytes = pong_msg.write_to_vec().map_err(|e| {
            error::Error::Custom(format!("Failed to serialize pong message: {}", e))
        })?;
        ws_write
            .send(WsMessage::Binary(pong_bytes.into()))
            .await
            .map_err(|e| error::Error::Custom(format!("Failed to send pong: {}", e)))?;

        // Process incoming messages
        while let Some(msg_result) = ws_read.next().await {
            match msg_result {
                Ok(WsMessage::Binary(bytes)) => {
                    match WsNotificationEvent::read_from_buffer(&bytes) {
                        Ok(event) => {
                            tracing::debug!(
                                "Received notification: id={}, type={:?}",
                                event.id,
                                event.event
                            );

                            // Send ACK
                            let ack_msg = AgentWsMessage::Ack {
                                notification_id: event.id,
                            };
                            if let Ok(ack_bytes) = ack_msg.write_to_vec() {
                                let _ = ws_write.send(WsMessage::Binary(ack_bytes.into())).await;
                            }

                            // Forward notification to main loop
                            if notification_tx.send(event.event).await.is_err() {
                                tracing::error!("Failed to send notification to main loop");
                                break;
                            }
                        }
                        Err(e) => {
                            tracing::error!("Failed to parse notification: {}", e);
                        }
                    }
                }
                Ok(WsMessage::Ping(_)) => {
                    tracing::trace!("Received WebSocket ping");
                }
                Ok(WsMessage::Pong(_)) => {
                    tracing::trace!("Received WebSocket pong");
                }
                Ok(WsMessage::Close(frame)) => {
                    tracing::info!("WebSocket closed by server: {:?}", frame);
                    break;
                }
                Ok(msg) => {
                    tracing::debug!("Received unexpected WebSocket message: {:?}", msg);
                }
                Err(e) => {
                    tracing::error!("WebSocket error: {}", e);
                    break;
                }
            }
        }

        Ok(())
    }

    /// Send heartbeat to coordinator
    async fn send_heartbeat(&mut self) -> crate::error::Result<()> {
        let url = self.api_url("api/agents/heartbeat");

        let req = AgentHeartbeatReq {
            state: self.state,
            assigned_suite_uuid: self.assigned_suite_uuid,
            last_notification_id: self.notification_counter,
            metrics: None,
        };

        let resp = self
            .http_client
            .post(url.as_str())
            .bearer_auth(&self.token)
            .json(&req)
            .send()
            .await
            .map_err(|e| error::Error::Custom(format!("Failed to send heartbeat: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(error::Error::Custom(format!(
                "Heartbeat failed: {} - {}",
                status, body
            )));
        }

        let heartbeat_resp: AgentHeartbeatResp = resp.json().await.map_err(|e| {
            error::Error::Custom(format!("Failed to parse heartbeat response: {}", e))
        })?;

        // Process any missed notifications from heartbeat
        for event in heartbeat_resp.notifications {
            tracing::debug!(
                "Received missed notification via heartbeat: id={}, type={:?}",
                event.id,
                event.event
            );
            self.handle_notification(event.event).await;
        }

        tracing::trace!("Heartbeat sent successfully (state: {:?})", self.state);
        Ok(())
    }

    /// Handle incoming notification from WebSocket
    async fn handle_notification(&mut self, notification: AgentNotification) {
        match notification {
            AgentNotification::SuiteAvailable {
                suite_uuid,
                priority,
            } => {
                tracing::info!(
                    "Received SuiteAvailable notification (suite: {:?}, priority: {})",
                    suite_uuid,
                    priority
                );
                // In Idle state, we'll fetch the suite in process_state
            }
            AgentNotification::PreemptSuite {
                new_suite_uuid,
                new_priority,
                current_suite_uuid,
            } => {
                tracing::warn!(
                    "Received PreemptSuite notification (current: {}, new: {}, priority: {})",
                    current_suite_uuid,
                    new_suite_uuid,
                    new_priority
                );

                // Idempotency guard: Only preempt if actually executing current suite
                if self.assigned_suite_uuid == Some(current_suite_uuid) {
                    tracing::info!(
                        "Preempting current suite {} for higher priority suite {}",
                        current_suite_uuid,
                        new_suite_uuid
                    );
                    // TODO: Implement preemption logic
                } else {
                    tracing::debug!(
                        "Ignoring PreemptSuite - not executing expected current suite (expected: {}, actual: {:?})",
                        current_suite_uuid,
                        self.assigned_suite_uuid
                    );
                }
            }
            AgentNotification::SuiteCancelled { suite_uuid, reason } => {
                tracing::warn!(
                    "Received SuiteCancelled notification (suite: {}, reason: {})",
                    suite_uuid,
                    reason
                );

                // Idempotency guard: Only cancel if actually assigned to this suite
                if self.assigned_suite_uuid == Some(suite_uuid) {
                    tracing::info!("Cancelling suite {} (reason: {})", suite_uuid, reason);
                    // TODO: Implement cancellation logic
                } else {
                    tracing::debug!(
                        "Ignoring SuiteCancelled - not executing expected suite (expected: {}, actual: {:?})",
                        suite_uuid,
                        self.assigned_suite_uuid
                    );
                }
            }
            AgentNotification::TasksCancelled { task_uuids } => {
                tracing::warn!(
                    "Received TasksCancelled notification ({} tasks)",
                    task_uuids.len()
                );
                // TODO: Implement task cancellation logic
            }
            AgentNotification::Shutdown { graceful } => {
                tracing::warn!("Received Shutdown notification (graceful: {})", graceful);
                // TODO: Implement shutdown logic
            }
            AgentNotification::Ping { server_time } => {
                tracing::trace!("Received Ping notification (server_time: {})", server_time);
            }
            AgentNotification::CounterSync { counter, boot_id } => {
                tracing::debug!(
                    "Received CounterSync notification (counter: {}, boot_id: {})",
                    counter,
                    boot_id
                );

                // Check if coordinator has restarted
                if self.coordinator_boot_id.is_none() || self.coordinator_boot_id != Some(boot_id) {
                    if let Some(old_boot_id) = self.coordinator_boot_id {
                        tracing::warn!(
                            "Coordinator restart detected: old_boot_id={}, new_boot_id={}",
                            old_boot_id,
                            boot_id
                        );
                    } else {
                        tracing::info!("Initial coordinator boot_id: {}", boot_id);
                    }

                    // Coordinator restarted - reset counter to new value
                    self.coordinator_boot_id = Some(boot_id);
                    self.notification_counter = counter;

                    tracing::info!("Reset notification counter to {}", counter);
                } else {
                    // Same coordinator - just update counter if it's higher
                    if counter > self.notification_counter {
                        self.notification_counter = counter;
                        tracing::debug!("Updated notification counter to {}", counter);
                    }
                }
            }
        }
    }

    /// Process current state and take actions
    async fn process_state(&mut self) -> crate::error::Result<()> {
        match self.state {
            AgentState::Idle => {
                // Try to fetch a suite
                if let Some(suite) = self.fetch_suite(None).await? {
                    tracing::info!(
                        "Fetched suite: {} ({})",
                        suite.uuid,
                        suite.name.as_ref().unwrap_or(&"<unnamed>".to_string())
                    );

                    // Accept the suite
                    if self.accept_suite(suite.uuid).await? {
                        tracing::info!("Accepted suite: {}", suite.uuid);
                        self.assigned_suite_uuid = Some(suite.uuid);
                        self.state = AgentState::Provision;

                        // Fake env preparation
                        self.fake_env_preparation(&suite).await?;

                        // Start suite execution
                        self.start_suite(suite.uuid).await?;
                        self.state = AgentState::Executing;

                        // Fake execution
                        self.fake_suite_execution(&suite).await?;

                        // Complete suite
                        self.state = AgentState::Cleanup;
                        self.fake_env_cleanup(&suite).await?;

                        let next_available = self.complete_suite(suite.uuid, 0, 0).await?;
                        self.assigned_suite_uuid = None;
                        self.state = AgentState::Idle;

                        tracing::info!(
                            "Suite {} completed. Next available: {}",
                            suite.uuid,
                            next_available
                        );
                    } else {
                        tracing::warn!("Failed to accept suite: {}", suite.uuid);
                    }
                }
            }
            AgentState::Provision => {
                // Waiting for env preparation to complete
                tracing::trace!("In Preparing state");
            }
            AgentState::Executing => {
                // Executing tasks
                tracing::trace!("In Executing state");
            }
            AgentState::Cleanup => {
                // Cleaning up
                tracing::trace!("In Cleanup state");
            }
            AgentState::Offline => {
                tracing::warn!("Agent is offline");
            }
        }

        Ok(())
    }

    /// Fetch an available suite from coordinator
    async fn fetch_suite(
        &self,
        suite_uuid: Option<Uuid>,
    ) -> crate::error::Result<Option<TaskSuiteSpec>> {
        let mut url = self.api_url("api/agents/suite");
        if let Some(uuid) = suite_uuid {
            url.set_query(Some(&format!("suite_uuid={}", uuid)));
        }

        let resp = self
            .http_client
            .get(url.as_str())
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| error::Error::Custom(format!("Failed to fetch suite: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(error::Error::Custom(format!(
                "Fetch suite failed: {} - {}",
                status, body
            )));
        }

        let fetch_resp: FetchSuiteResp = resp
            .json()
            .await
            .map_err(|e| error::Error::Custom(format!("Failed to parse fetch response: {}", e)))?;

        Ok(fetch_resp.suite)
    }

    /// Accept a suite for execution
    async fn accept_suite(&self, suite_uuid: Uuid) -> crate::error::Result<bool> {
        let url = self.api_url("api/agents/suite/accept");

        let req = AcceptSuiteReq { suite_uuid };

        let resp = self
            .http_client
            .post(url.as_str())
            .bearer_auth(&self.token)
            .json(&req)
            .send()
            .await
            .map_err(|e| error::Error::Custom(format!("Failed to accept suite: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(error::Error::Custom(format!(
                "Accept suite failed: {} - {}",
                status, body
            )));
        }

        let accept_resp: AcceptSuiteResp = resp
            .json()
            .await
            .map_err(|e| error::Error::Custom(format!("Failed to parse accept response: {}", e)))?;

        if !accept_resp.accepted {
            tracing::warn!(
                "Suite not accepted: {}",
                accept_resp.reason.unwrap_or_default()
            );
        }

        Ok(accept_resp.accepted)
    }

    /// Report suite execution started
    async fn start_suite(&self, suite_uuid: Uuid) -> crate::error::Result<()> {
        let url = self.api_url("api/agents/suite/start");

        let req = StartSuiteReq { suite_uuid };

        let resp = self
            .http_client
            .post(url.as_str())
            .bearer_auth(&self.token)
            .json(&req)
            .send()
            .await
            .map_err(|e| error::Error::Custom(format!("Failed to start suite: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(error::Error::Custom(format!(
                "Start suite failed: {} - {}",
                status, body
            )));
        }

        Ok(())
    }

    /// Report suite execution completed
    async fn complete_suite(
        &self,
        suite_uuid: Uuid,
        tasks_completed: u64,
        tasks_failed: u64,
    ) -> crate::error::Result<bool> {
        let url = self.api_url("api/agents/suite/complete");

        let req = CompleteSuiteReq {
            suite_uuid,
            tasks_completed,
            tasks_failed,
            completion_reason: SuiteCompletionReason::Normal,
        };

        let resp = self
            .http_client
            .post(url.as_str())
            .bearer_auth(&self.token)
            .json(&req)
            .send()
            .await
            .map_err(|e| error::Error::Custom(format!("Failed to complete suite: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(error::Error::Custom(format!(
                "Complete suite failed: {} - {}",
                status, body
            )));
        }

        let complete_resp: CompleteSuiteResp = resp.json().await.map_err(|e| {
            error::Error::Custom(format!("Failed to parse complete response: {}", e))
        })?;

        Ok(complete_resp.next_suite_available)
    }

    // Fake handlers for testing

    async fn fake_env_preparation(&self, suite: &TaskSuiteSpec) -> crate::error::Result<()> {
        tracing::info!(
            "=== FAKE: Running environment preparation for suite {} ===",
            suite.uuid
        );
        if let Some(ref hooks) = suite.exec_hooks {
            if let Some(ref provision) = hooks.provision {
                tracing::info!("Provision spec: {:?}", provision);
            } else {
                tracing::info!("No provision hook specified");
            }
        } else {
            tracing::info!("No exec_hooks specified");
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
        tracing::info!("=== FAKE: Environment preparation completed ===");
        Ok(())
    }

    async fn fake_suite_execution(&self, suite: &TaskSuiteSpec) -> crate::error::Result<()> {
        tracing::info!("=== FAKE: Executing suite {} ===", suite.uuid);
        tracing::info!(
            "Total tasks: {}, Pending tasks: {}",
            suite.total_tasks,
            suite.pending_tasks
        );
        tracing::info!("Worker schedule: {:?}", suite.worker_schedule);

        // Simulate some work
        for i in 0..3 {
            tokio::time::sleep(Duration::from_secs(2)).await;
            tracing::info!("FAKE: Processing batch {} of tasks...", i + 1);
        }

        tracing::info!("=== FAKE: Suite execution completed ===");
        Ok(())
    }

    async fn fake_env_cleanup(&self, suite: &TaskSuiteSpec) -> crate::error::Result<()> {
        tracing::info!(
            "=== FAKE: Running environment cleanup for suite {} ===",
            suite.uuid
        );
        if let Some(ref hooks) = suite.exec_hooks {
            if let Some(ref cleanup) = hooks.cleanup {
                tracing::info!("Cleanup spec: {:?}", cleanup);
            } else {
                tracing::info!("No cleanup hook specified");
            }
        } else {
            tracing::info!("No exec_hooks specified");
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
        tracing::info!("=== FAKE: Environment cleanup completed ===");
        Ok(())
    }
}
