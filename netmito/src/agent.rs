//! Agent implementation for executing task suites
//!
//! This module implements an agent client that connects to the coordinator
//! to fetch and execute task suites. It handles:
//! - Registration with the coordinator
//! - WebSocket connection for real-time notifications
//! - HTTP API calls for suite lifecycle management
//! - Heartbeat mechanism for health reporting
//! - State machine for suite execution with real task fetch/report

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use reqwest::StatusCode;
use speedy::{Readable, Writable};
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use url::Url;
use uuid::Uuid;

use crate::config::{AgentConfig, AgentConfigCli};
use crate::entity::content::ArtifactContentType;
use crate::entity::state::{AgentState, TaskExecState};
use crate::error;
use crate::executor::{execute_task, CoordinatorClient, TaskExecutor};
use crate::schema::*;
use crate::service::auth::cred::get_user_credential;

pub struct MitoAgent;

/// What to fetch next, recorded by a `SuiteAvailable`/`PreemptSuite`
/// notification and consumed (taken) by `process_state` in the Idle branch.
/// Distinct from "nothing pending" so an idle agent never polls `fetch_suite`
/// unless the coordinator actually signaled available work.
#[derive(Debug, Clone, Copy)]
enum PendingSuite {
    /// `SuiteAvailable { suite_uuid: None }` — work is available but unspecified;
    /// fetch whatever the coordinator hands back (`fetch_suite(None)`).
    Any,
    /// A specific suite was named (`SuiteAvailable { Some }` or a preemption);
    /// target it directly (`fetch_suite(Some(uuid))`).
    Specific(Uuid),
}

/// Agent client that connects to coordinator
struct AgentClient {
    coordinator_addr: Url,
    agent_uuid: Uuid,
    token: String,
    notification_counter: u64,
    coordinator_boot_id: Option<Uuid>,
    state: AgentState,
    assigned_suite_uuid: Option<Uuid>,
    /// Next suite to fetch, set by a `SuiteAvailable`/`PreemptSuite` notification
    /// and consumed in the Idle branch of `process_state`. `None` means nothing
    /// to fetch — the agent stays idle instead of polling the coordinator.
    pending_suite: Option<PendingSuite>,
    heartbeat_interval: Duration,
    /// Retry/poll cadence handed to executor slots (from `AgentConfig`).
    polling_interval: Duration,
    http_client: reqwest::Client,
    /// Base cache directory (`<cache>/mitosis/<agent_uuid>`); a suite run's
    /// executor slots get per-slot subdirs under `<cache_path>/<run>/<slot>`.
    cache_path: PathBuf,
    /// Cancellation token for the in-flight suite run's executor pool. Cancelled
    /// on SuiteCancelled / PreemptSuite / shutdown to stop the whole pool; `None`
    /// while Idle.
    pool_token: Option<CancellationToken>,
    /// Handle to the spawned suite-lifecycle task (accept→…→complete) while a
    /// suite is executing, so the main loop stays free for heartbeats and
    /// notifications. Reaped (non-blocking) once it finishes.
    current_execution: Option<JoinHandle<()>>,
    /// Token used to signal shutdown from coordinator Shutdown notifications.
    /// Cancelling this token exits the main run loop.
    shutdown_token: CancellationToken,
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

        let machine_code = resolve_machine_code(config.machine_code.take());
        tracing::info!("Machine code: {}", machine_code);

        // Register as an agent
        let mut register_url = config.coordinator_addr.clone();
        register_url.set_path("agents");
        let metadata = Some(AgentMetadata {
            version: option_env!("CARGO_PKG_VERSION").unwrap_or("").to_string(),
            long_version: option_env!("CARGO_PKG_VERSION").unwrap_or("").to_string(),
        });
        let req = RegisterAgentReq {
            tags: config.tags.clone(),
            labels: config.labels.clone(),
            groups: config.groups.clone(),
            lifetime: config.lifetime,
            machine_code,
            metadata,
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

        // Per-agent cache root for executor slot directories.
        let mut cache_path = dirs::cache_dir()
            .ok_or_else(|| error::Error::Custom("Cache dir not found".to_string()))?;
        cache_path.push("mitosis");
        cache_path.push(register_resp.agent_uuid.to_string());
        tokio::fs::create_dir_all(&cache_path).await?;

        // Create client instance
        let mut client = AgentClient {
            coordinator_addr: coordinator_url,
            agent_uuid: register_resp.agent_uuid,
            token: register_resp.token,
            notification_counter: register_resp.notification_counter,
            coordinator_boot_id: None,
            state: AgentState::Idle,
            assigned_suite_uuid: None,
            pending_suite: None,
            heartbeat_interval: config.heartbeat_interval,
            polling_interval: config.polling_interval,
            http_client,
            cache_path,
            pool_token: None,
            current_execution: None,
            shutdown_token: tokio_util::sync::CancellationToken::new(),
        };

        // Run the main loop
        client.run().await
    }
}

/// Resolve a stable machine code for this host, so registration always
/// carries one.
///
/// Precedence: explicit config override → cached value in the mitosis config
/// dir (`<config_dir>/mitosis/machine-id`) → `/etc/machine-id` → freshly
/// generated UUID. The resolved value (unless explicitly overridden) is
/// persisted to the cache file so the identity stays stable across restarts
/// even where `/etc/machine-id` is unavailable (containers, non-Linux hosts).
fn resolve_machine_code(explicit: Option<String>) -> String {
    if let Some(code) = explicit
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        return code;
    }

    let cache_path = dirs::config_dir().map(|mut p| {
        p.push("mitosis");
        p.push("machine-id");
        p
    });

    if let Some(ref path) = cache_path {
        if let Ok(cached) = std::fs::read_to_string(path) {
            let cached = cached.trim().to_string();
            if !cached.is_empty() {
                return cached;
            }
        }
    }

    let code = std::fs::read_to_string("/etc/machine-id")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            tracing::info!("No /etc/machine-id available, generating a machine code");
            Uuid::new_v4().simple().to_string()
        });

    match cache_path {
        Some(path) => {
            let res = path
                .parent()
                .map(std::fs::create_dir_all)
                .unwrap_or(Ok(()))
                .and_then(|_| std::fs::write(&path, &code));
            if let Err(e) = res {
                tracing::warn!(
                    "Failed to cache machine code at {}: {}; a new code may be generated on next start",
                    path.display(),
                    e
                );
            }
        }
        None => tracing::warn!(
            "No config directory available to cache machine code; a new code may be generated on next start"
        ),
    }

    code
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
        // Clone shutdown_token for use in the select loop and spawned tasks.
        // Cancelling either self.shutdown_token or this clone cancels both.
        let cancel_token = self.shutdown_token.clone();
        let cancel_token_clone = cancel_token.clone();

        // Setup SIGINT handler
        tokio::spawn(async move {
            tokio::signal::ctrl_c().await.ok();
            tracing::info!("Received SIGINT, shutting down...");
            cancel_token_clone.cancel();
        });

        // Start WebSocket connection in background.
        // The channel carries WsNotificationEvent so the main loop can update the
        // notification counter from the event's sequence ID.
        let (ws_tx, mut ws_rx) = tokio::sync::mpsc::channel::<WsNotificationEvent>(32);
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
                    // Stop any in-flight suite pool so its executors wind down
                    // (reports cease; teardown reclaims uncommitted tasks).
                    if let Some(token) = &self.pool_token {
                        token.cancel();
                    }
                    break;
                }

                // Handle WebSocket notifications; advance the notification counter.
                Some(event) = ws_rx.recv() => {
                    self.notification_counter = self.notification_counter.max(event.id);
                    self.handle_notification(event.event).await;
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

        // Drain the in-flight suite lifecycle task (pool already cancelled above)
        // so its final reports flush before the process exits.
        if let Some(handle) = self.current_execution.take() {
            let _ = handle.await;
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
        notification_tx: tokio::sync::mpsc::Sender<WsNotificationEvent>,
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
        notification_tx: &tokio::sync::mpsc::Sender<WsNotificationEvent>,
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

        // Send initial pong to confirm connection
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

                            // Forward the full event (with sequence ID) to the main loop
                            if notification_tx.send(event).await.is_err() {
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

        // Process any missed notifications from heartbeat; advance the counter for each.
        for event in heartbeat_resp.notifications {
            tracing::debug!(
                "Received missed notification via heartbeat: id={}, type={:?}",
                event.id,
                event.event
            );
            self.notification_counter = self.notification_counter.max(event.id);
            self.handle_notification(event.event).await;
        }

        tracing::trace!("Heartbeat sent successfully (state: {:?})", self.state);
        Ok(())
    }

    /// Record that a suite is available to fetch. A named suite (`Some`) always
    /// wins, since a specific target is more useful than the generic signal; the
    /// generic `Any` only fills an empty slot so it never clobbers a pending hint.
    fn note_pending_suite(&mut self, suite_uuid: Option<Uuid>) {
        match suite_uuid {
            Some(uuid) => self.pending_suite = Some(PendingSuite::Specific(uuid)),
            None => {
                if self.pending_suite.is_none() {
                    self.pending_suite = Some(PendingSuite::Any);
                }
            }
        }
    }

    /// Handle incoming notification from WebSocket or heartbeat catch-up
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
                // Record the work; the Idle branch of `process_state` consumes it.
                // Setting the slot while Executing is fine — it is only acted on
                // once we return to Idle, giving back-to-back pickup without
                // waiting for the next heartbeat.
                self.note_pending_suite(suite_uuid);
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

                // Idempotency guard: only react if we're actually executing the specified suite
                if self.assigned_suite_uuid == Some(current_suite_uuid) {
                    tracing::info!(
                        "Preempting current suite {} for higher priority suite {}",
                        current_suite_uuid,
                        new_suite_uuid
                    );
                    // Cancel the executor pool to stop task execution; the
                    // lifecycle task then runs cleanup, reports completion, and the
                    // agent returns to Idle. Record the new suite so the Idle
                    // branch fetches it directly instead of waiting for a re-notify.
                    if let Some(token) = &self.pool_token {
                        token.cancel();
                    }
                    self.note_pending_suite(Some(new_suite_uuid));
                } else {
                    tracing::debug!(
                        "Ignoring PreemptSuite - not executing expected suite \
                         (expected: {}, actual: {:?})",
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

                // Idempotency guard: only react if we're assigned to this suite
                if self.assigned_suite_uuid == Some(suite_uuid) {
                    tracing::info!("Cancelling executor pool for suite {}", suite_uuid);
                    if let Some(token) = &self.pool_token {
                        token.cancel();
                    }
                } else {
                    tracing::debug!(
                        "Ignoring SuiteCancelled - not executing expected suite \
                         (expected: {}, actual: {:?})",
                        suite_uuid,
                        self.assigned_suite_uuid
                    );
                }
            }
            AgentNotification::TasksCancelled { task_uuids } => {
                tracing::warn!(
                    "Received TasksCancelled notification ({} tasks) — \
                     cancellation will be detected at Commit time",
                    task_uuids.len()
                );
                // Individual task cancellation is detected when the agent tries to
                // Commit: the coordinator returns an error if the task was already
                // cancelled by the user. No client-side tracking is needed.
            }
            AgentNotification::Shutdown { graceful } => {
                tracing::warn!("Received Shutdown notification (graceful: {})", graceful);
                self.shutdown_token.cancel();
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

                // Check if coordinator has restarted (different boot_id)
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

                    self.coordinator_boot_id = Some(boot_id);
                    self.notification_counter = counter;
                    tracing::info!("Reset notification counter to {}", counter);
                } else {
                    // Same coordinator — only update counter if it's higher
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
                // Only fetch when a notification signaled work; otherwise stay
                // idle (no redundant polling — the coordinator already runs the
                // availability check on every heartbeat and re-notifies us).
                let target = match self.pending_suite.take() {
                    None => return Ok(()),
                    Some(PendingSuite::Any) => None,
                    Some(PendingSuite::Specific(uuid)) => Some(uuid),
                };

                // Each notification yields at most one fetch attempt: the slot is
                // already taken above, so a stale hint or failed accept just
                // returns to Idle and waits for the next re-notify (no spin).
                if let Some(suite) = self.fetch_suite(target).await? {
                    tracing::info!(
                        "Fetched suite: {} ({})",
                        suite.uuid,
                        suite.name.as_ref().unwrap_or(&"<unnamed>".to_string())
                    );

                    // Accept the suite — the coordinator returns the run handle.
                    if let Some(run) = self.accept_suite(suite.uuid).await? {
                        tracing::info!("Accepted suite: {} (run {})", suite.uuid, run);
                        self.assigned_suite_uuid = Some(suite.uuid);

                        // Spawn the suite lifecycle (provision → start → execute
                        // pool → cleanup → complete) as a background task so the
                        // main loop keeps servicing heartbeats and notifications
                        // while the suite runs. SuiteCancelled / PreemptSuite /
                        // shutdown cancel `pool_token` to stop it.
                        let pool_token = CancellationToken::new();
                        self.pool_token = Some(pool_token.clone());
                        let runner = SuiteRunner {
                            http_client: self.http_client.clone(),
                            token: self.token.clone(),
                            coordinator_addr: self.coordinator_addr.clone(),
                            polling_interval: self.polling_interval,
                            cache_path: self.cache_path.clone(),
                            pool_token,
                        };
                        self.current_execution = Some(tokio::spawn(runner.run(suite, run)));
                        self.state = AgentState::Executing;
                    } else {
                        tracing::warn!("Failed to accept suite: {}", suite.uuid);
                    }
                }
            }
            AgentState::Executing => {
                // Reap the lifecycle task without blocking the loop; once it
                // finishes, release the run and return to Idle.
                let finished = self
                    .current_execution
                    .as_ref()
                    .map(JoinHandle::is_finished)
                    .unwrap_or(true);
                if finished {
                    if let Some(handle) = self.current_execution.take() {
                        if let Err(e) = handle.await {
                            tracing::error!("Suite lifecycle task failed: {}", e);
                        }
                    }
                    self.assigned_suite_uuid = None;
                    self.pool_token = None;
                    self.state = AgentState::Idle;
                    tracing::info!("Agent returned to Idle");
                }
            }
            AgentState::Provision => {
                tracing::trace!("In Provision state");
            }
            AgentState::Cleanup => {
                tracing::trace!("In Cleanup state");
            }
            AgentState::Offline => {
                tracing::warn!("Agent is offline");
            }
        }

        Ok(())
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Suite lifecycle API calls
    // ─────────────────────────────────────────────────────────────────────────

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

    /// Accept a suite for execution. Returns the opaque run handle on success.
    async fn accept_suite(&self, suite_uuid: Uuid) -> crate::error::Result<Option<i64>> {
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

        Ok(accept_resp.run)
    }
}

/// Connection context for one spawned suite run. The agent's main loop spawns
/// `SuiteRunner::run` so it stays free to service heartbeats and notifications
/// while the suite executes; `pool_token` (shared with every executor slot) is
/// cancelled to stop the whole pool on cancel / preempt / shutdown.
struct SuiteRunner {
    http_client: reqwest::Client,
    token: String,
    coordinator_addr: Url,
    polling_interval: Duration,
    /// Per-agent cache root; this run's slots use `<cache_path>/<run>/<slot>`.
    cache_path: PathBuf,
    pool_token: CancellationToken,
}

impl SuiteRunner {
    /// Build a URL for the given API path.
    fn api_url(&self, path: &str) -> Url {
        let mut url = self.coordinator_addr.clone();
        url.set_path(path);
        url
    }

    /// Drive the full suite lifecycle: provision → start → execute pool →
    /// cleanup → complete. Each step logs and proceeds on error so a half-run
    /// still transitions the coordinator run row toward a terminal state (real
    /// hook failure → `Failed` outcome comes with Phase 4). Provision/cleanup are
    /// still the fake stubs for now.
    async fn run(self, suite: TaskSuiteSpec, run: i64) {
        let suite_uuid = suite.uuid;

        // Provision hook (fake) → already in Provision from accept.
        if let Err(e) = self.fake_env_preparation(&suite).await {
            tracing::error!(
                "Provision failed for suite {} (run {}): {}",
                suite_uuid,
                run,
                e
            );
        }

        // Provision done → Executing. Report to the coordinator
        if let Err(e) = self.report_run_start(run).await {
            tracing::error!("Failed to start suite {} (run {}): {}", suite_uuid, run, e);
        }

        // Fetch and execute the suite's tasks via the shared executor core.
        self.execute_pool(&suite, run).await;

        // Execution done → Cleanup.
        if let Err(e) = self.report_run_cleanup(run).await {
            tracing::error!(
                "Failed to enter cleanup for suite {} (run {}): {}",
                suite_uuid,
                run,
                e
            );
        }

        // Cleanup hook (fake).
        if let Err(e) = self.fake_env_cleanup(&suite).await {
            tracing::error!(
                "Cleanup failed for suite {} (run {}): {}",
                suite_uuid,
                run,
                e
            );
        }

        // Cleanup done → terminal Completed (Phase 4 adds Failed on hook/exec failure).
        match self.report_run_complete(run).await {
            Ok(next_available) => tracing::info!(
                "Suite {} completed (run {}). Next available: {}",
                suite_uuid,
                run,
                next_available
            ),
            Err(e) => {
                tracing::error!(
                    "Failed to complete suite {} (run {}): {}",
                    suite_uuid,
                    run,
                    e
                )
            }
        }

        // Best-effort cleanup of this run's per-slot cache dirs.
        let run_dir = self.cache_path.join(run.to_string());
        if let Err(e) = tokio::fs::remove_dir_all(&run_dir).await {
            tracing::debug!("Failed to remove run cache dir {:?}: {}", run_dir, e);
        }
    }

    /// Run the suite's tasks through an in-process executor pool that reuses the
    /// worker's `execute_task` core. A producer batch-fetches tasks and feeds a
    /// bounded channel; `worker_count` consumers pull from it and execute,
    /// overlapping fetch with execution. The pool stops when the producer
    /// exhausts the suite or `pool_token` is cancelled.
    async fn execute_pool(&self, suite: &TaskSuiteSpec, run: i64) {
        let (worker_count, prefetch) = match suite.worker_schedule {
            WorkerSchedulePlan::FixedWorkers {
                worker_count,
                task_prefetch_count,
                ..
            } => (worker_count.max(1), task_prefetch_count.max(1)),
        };
        // Matches the dispatcher `UpdateCapacity` formula sent on accept.
        let capacity = worker_count.saturating_mul(prefetch).max(1);

        tracing::info!(
            "Starting executor pool for suite {} (run {}): {} workers, capacity {}",
            suite.uuid,
            run,
            worker_count,
            capacity
        );

        let (tx, rx) = mpsc::channel::<WorkerTaskResp>(capacity as usize);
        let rx = Arc::new(Mutex::new(rx));

        // Producer: batch-fetch and feed the channel until empty or cancelled.
        let producer = {
            let http_client = self.http_client.clone();
            let token = self.token.clone();
            let base_url = self.coordinator_addr.clone();
            let pool_token = self.pool_token.clone();
            let suite_uuid = suite.uuid;
            tokio::spawn(async move {
                const MAX_EMPTY_RETRIES: u32 = 3;
                let mut empty_retries: u32 = 0;
                loop {
                    if pool_token.is_cancelled() {
                        break;
                    }
                    let tasks = match agent_fetch_tasks(
                        &http_client,
                        &token,
                        &base_url,
                        suite_uuid,
                        capacity,
                    )
                    .await
                    {
                        Ok(tasks) => tasks,
                        Err(e) => {
                            tracing::error!(
                                "Failed to fetch tasks from suite {}: {}",
                                suite_uuid,
                                e
                            );
                            break;
                        }
                    };

                    if tasks.is_empty() {
                        empty_retries += 1;
                        if empty_retries >= MAX_EMPTY_RETRIES {
                            tracing::info!(
                                "No more tasks for suite {} after {} retries, producer stopping",
                                suite_uuid,
                                MAX_EMPTY_RETRIES
                            );
                            break;
                        }
                        tokio::select! {
                            biased;
                            _ = pool_token.cancelled() => break,
                            _ = tokio::time::sleep(Duration::from_millis(500)) => {}
                        }
                        continue;
                    }

                    empty_retries = 0;
                    for task in tasks {
                        tokio::select! {
                            biased;
                            _ = pool_token.cancelled() => return,
                            res = tx.send(task) => {
                                if res.is_err() {
                                    // All consumers gone — nothing left to feed.
                                    return;
                                }
                            }
                        }
                    }
                }
                // `tx` dropped here → consumers see the channel close and exit.
            })
        };

        // Consumers: each owns a cache slot and an agent coordinator client.
        let mut consumers = tokio::task::JoinSet::new();
        for slot in 0..worker_count {
            let rx = rx.clone();
            let http_client = self.http_client.clone();
            let token = self.token.clone();
            let coordinator_addr = self.coordinator_addr.clone();
            let pool_token = self.pool_token.clone();
            let polling_interval = self.polling_interval;
            let slot_dir = self.cache_path.join(run.to_string()).join(slot.to_string());
            consumers.spawn(async move {
                // `execute_task` assumes the result/exec/resource dirs exist
                // (the worker creates them at setup); make them per slot.
                if let Err(e) = ensure_slot_dirs(&slot_dir).await {
                    tracing::error!("Failed to create cache dirs for slot {}: {}", slot, e);
                    return;
                }
                let client = AgentCoordinatorClient {
                    http_client: http_client.clone(),
                    token,
                    coordinator_addr,
                    run,
                    polling_interval,
                    cancel_token: pool_token.clone(),
                };
                let mut executor = TaskExecutor {
                    task_cancel_token: pool_token,
                    // The agent has no force-exit concept; give each slot its own.
                    coordinator_force_exit: Arc::new(AtomicBool::new(false)),
                    polling_interval,
                    task_cache_path: slot_dir,
                    http_client,
                    client: Box::new(client),
                };
                loop {
                    let task = {
                        let mut guard = rx.lock().await;
                        guard.recv().await
                    };
                    match task {
                        Some(task) => {
                            let task_uuid = task.uuid;
                            if let Err(e) = execute_task(task, &mut executor).await {
                                tracing::error!(
                                    "Failed to execute task {} on slot {}: {}",
                                    task_uuid,
                                    slot,
                                    e
                                );
                            }
                        }
                        None => break,
                    }
                }
            });
        }

        let _ = producer.await;
        // Drain to completion. Cancellation stays cooperative via `pool_token`;
        // we never drop the set early, so `JoinSet`'s abort-on-drop (a hard
        // cancel at an await point) never fires and teardown stays graceful.
        // Don't use `join_all()` as a failing task will panic the call entire call.
        while let Some(consumer_result) = consumers.join_next().await {
            if let Err(e) = consumer_result {
                tracing::warn!("Task execution failed: {}", e);
            }
        }
        tracing::info!(
            "Executor pool for suite {} (run {}) finished",
            suite.uuid,
            run
        );
    }

    /// Notify coordinator that provision is done and execution is starting (→ Executing)
    async fn report_run_start(&self, run: i64) -> crate::error::Result<()> {
        let url = self.api_url("api/agents/suite/start");

        let req = StartSuiteReq { run };

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

    /// Notify coordinator that execution is done and cleanup is starting (→ Cleanup)
    async fn report_run_cleanup(&self, run: i64) -> crate::error::Result<()> {
        let url = self.api_url("api/agents/suite/cleanup");

        let req = EnterCleanupReq { run };

        let resp = self
            .http_client
            .post(url.as_str())
            .bearer_auth(&self.token)
            .json(&req)
            .send()
            .await
            .map_err(|e| error::Error::Custom(format!("Failed to enter cleanup: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(error::Error::Custom(format!(
                "Enter cleanup failed: {} - {}",
                status, body
            )));
        }

        Ok(())
    }

    /// Notify coordinator that cleanup is done and agent is going Idle
    async fn report_run_complete(&self, run: i64) -> crate::error::Result<bool> {
        let url = self.api_url("api/agents/suite/complete");

        let req = CompleteSuiteReq {
            run,
            // The agent does not tally tasks; the coordinator's commit-derived
            // run counters are authoritative. Fields kept on the wire so a
            // cross-check can be reinstated later without a protocol change.
            tasks_completed: 0,
            tasks_failed: 0,
            // The fake hooks always complete cleanly; emitting `Failed` on real
            // hook/exec failure comes with real hooks (Phase 4).
            outcome: SuiteRunOutcome::Completed,
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

    // ─────────────────────────────────────────────────────────────────────────
    // Fake execution stubs (provision / cleanup hooks — real hooks in Phase 4)
    // ─────────────────────────────────────────────────────────────────────────

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

/// Batch-fetch up to `max_count` tasks from `suite_uuid` via the agent endpoint.
/// A free function (not a method) so the pool's producer task can call it after
/// the lifecycle context's connection fields are cloned into it.
async fn agent_fetch_tasks(
    http_client: &reqwest::Client,
    token: &str,
    base_url: &Url,
    suite_uuid: Uuid,
    max_count: u32,
) -> crate::error::Result<Vec<WorkerTaskResp>> {
    let mut url = base_url.clone();
    url.set_path("api/agents/tasks/fetch");

    let req = FetchTasksReq {
        suite_uuid,
        max_count,
    };

    let resp = http_client
        .post(url.as_str())
        .bearer_auth(token)
        .json(&req)
        .send()
        .await
        .map_err(|e| error::Error::Custom(format!("Failed to fetch tasks: {}", e)))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(error::Error::Custom(format!(
            "Fetch tasks failed: {} - {}",
            status, body
        )));
    }

    let fetch_resp: FetchTasksResp = resp.json().await.map_err(|e| {
        error::Error::Custom(format!("Failed to parse fetch tasks response: {}", e))
    })?;

    Ok(fetch_resp.tasks)
}

/// Create the per-slot cache subdirectories `execute_task` expects to exist
/// (the worker creates these in `setup`).
async fn ensure_slot_dirs(slot_dir: &Path) -> std::io::Result<()> {
    tokio::fs::create_dir_all(slot_dir.join("result")).await?;
    tokio::fs::create_dir_all(slot_dir.join("exec")).await?;
    tokio::fs::create_dir_all(slot_dir.join("resource")).await?;
    Ok(())
}

/// Agent impl of [`CoordinatorClient`]: talks to the `agents/*` endpoints with
/// the agent token and the fixed suite `run` handle. It has no redis, so
/// `announce_state`/`unsubscribe` are no-ops and `watch` polls. Used by the
/// agent's executor pool to drive the shared `execute_task` core.
pub(crate) struct AgentCoordinatorClient {
    http_client: reqwest::Client,
    token: String,
    coordinator_addr: Url,
    /// Suite run handle, fixed for this executor's lifetime; sent with every report.
    run: i64,
    polling_interval: Duration,
    /// Cancelled when the run is observed closed (a report returns 409), so the
    /// agent's executor pool can stop.
    cancel_token: CancellationToken,
}

impl AgentCoordinatorClient {
    fn api_url(&self, path: &str) -> Url {
        let mut url = self.coordinator_addr.clone();
        url.set_path(path);
        url
    }

    /// GET a task's current state (the watch poll). `None` on any error (logged).
    async fn query_task(&self, uuid: &Uuid) -> Option<TaskQueryResp> {
        let url = self.api_url(&format!("api/agents/tasks/{uuid}"));
        match self
            .http_client
            .get(url.as_str())
            .bearer_auth(&self.token)
            .send()
            .await
        {
            Ok(resp) => {
                if resp.status().is_success() {
                    resp.json::<TaskQueryResp>().await.ok()
                } else {
                    let resp: error::ErrorMsg = resp
                        .json()
                        .await
                        .unwrap_or_else(|e| error::ErrorMsg { msg: e.to_string() });
                    tracing::error!("Get task failed with error: {}", resp.msg);
                    None
                }
            }
            Err(e) => {
                tracing::error!("Get task failed with error: {}", e);
                None
            }
        }
    }
}

#[async_trait]
impl CoordinatorClient for AgentCoordinatorClient {
    async fn report(&mut self, id: i64, op: ReportTaskOp) -> crate::error::Result<Option<String>> {
        let is_upload = matches!(op, ReportTaskOp::Upload { .. });
        let req = ReportAgentTaskReq {
            run: self.run,
            id,
            op,
        };
        let url = self.api_url("api/agents/tasks/report");
        loop {
            let resp = self
                .http_client
                .post(url.as_str())
                .bearer_auth(&self.token)
                .json(&req)
                .send()
                .await;
            match resp {
                Ok(resp) => {
                    if resp.status().is_success() {
                        let resp = resp
                            .json::<ReportTaskResp>()
                            .await
                            .map_err(error::RequestError::from)?;
                        return Ok(resp.url);
                    } else if resp.status() == StatusCode::CONFLICT {
                        // The run is terminal/closed — further reports are pointless.
                        // Signal the pool to stop, and treat this as "task gone".
                        tracing::info!("Report rejected: run {} is closed", self.run);
                        self.cancel_token.cancel();
                        return Ok(None);
                    } else if resp.status() == StatusCode::NOT_FOUND {
                        tracing::debug!("Task not found, ignore and go on for next cycle");
                        return Ok(None);
                    } else if is_upload && resp.status() == StatusCode::FORBIDDEN {
                        let resp: error::ErrorMsg = resp
                            .json()
                            .await
                            .unwrap_or_else(|e| error::ErrorMsg { msg: e.to_string() });
                        tracing::info!(
                            "Request upload url failed with permission denied: {}",
                            resp.msg
                        );
                        return Ok(None);
                    } else {
                        let resp: error::ErrorMsg = resp
                            .json()
                            .await
                            .unwrap_or_else(|e| error::ErrorMsg { msg: e.to_string() });
                        if is_upload {
                            self.cancel_token.cancel();
                        }
                        return Err(error::Error::Custom(format!(
                            "Report task failed with error: {}",
                            resp.msg
                        )));
                    }
                }
                Err(e) => {
                    if e.is_connect() && e.is_request() {
                        tracing::error!(
                            "Report task failed with connection error: {}. Retry after {:?}",
                            e,
                            self.polling_interval
                        );
                        tokio::select! {
                            biased;
                            _ = self.cancel_token.cancelled() => return Ok(None),
                            _ = tokio::time::sleep(self.polling_interval) => {},
                        }
                        continue;
                    } else {
                        return Err(error::RequestError::from(e).into());
                    }
                }
            }
        }
    }

    fn artifact_download_req(
        &self,
        uuid: Uuid,
        content_type: ArtifactContentType,
    ) -> reqwest::RequestBuilder {
        let ct = serde_json::to_value(content_type)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_else(|| "result".to_owned());
        let url = self.api_url(&format!("api/agents/tasks/{uuid}/artifacts/{ct}"));
        self.http_client.get(url).bearer_auth(&self.token)
    }

    fn attachment_download_req(&self, task_uuid: &Uuid, key: &str) -> reqwest::RequestBuilder {
        let url = self.api_url(&format!("api/agents/tasks/{task_uuid}/attachments/{key}"));
        self.http_client.get(url).bearer_auth(&self.token)
    }

    async fn watch(&mut self, uuid: &Uuid, target: TaskExecState) {
        // Poll-only (no redis): the wrapping select in `execute_task` provides the
        // cancel + overall timeout, so this just loops until the target is reached.
        let mut wait_until = Instant::now();
        loop {
            tokio::time::sleep_until(wait_until).await;
            wait_until = Instant::now() + Duration::from_secs(30);
            if let Some(task) = self.query_task(uuid).await {
                if task.info.state.is_reach(&target, task.info.result) {
                    break;
                }
            }
        }
    }

    fn can_watch(&self) -> bool {
        true
    }

    async fn unsubscribe(&mut self, _uuid: &Uuid) {}

    async fn announce_state(&mut self, _uuid: &Uuid, _state: i32, _ex: Option<u64>) {}
}
