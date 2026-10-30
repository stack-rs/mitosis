//! The agent: a process that accepts task suites from the coordinator and runs them.
//!
//! ```text
//!  register ─▶ ws connect (unless --no-ws) ─┐
//!  heartbeat every N ───────────────────────┼─▶ main loop ─▶ (idle + work) ─▶ accept suite
//!  idle poll every N (if configured) ───────┘                                      │
//!                                                                                  ▼
//!                        complete ◀─ cleanup hook ◀─ cleanup ◀─ tasks ◀─ start ◀─ provision hook
//! ```
//!
//! The main loop only sends heartbeats and reads notifications. A [`SuiteRunner`] in a spawned
//! task runs the accepted suite. Thus the main loop and the suite do not block each other.

use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use crossfire::{AsyncTx, MAsyncRx, MAsyncTx};
use futures::{SinkExt as _, StreamExt as _};
use reqwest::StatusCode;
use speedy::{Readable as _, Writable as _};
use tokio::{
    sync::{watch, Notify},
    task::JoinHandle,
};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::layer::SubscriberExt;
use url::Url;
use uuid::Uuid;

use crate::{
    config::{AgentConfig, AgentConfigCli, TracingGuard},
    entity::{hook_tasks::HookType, state::AgentState},
    error::{self, Result},
    executor::{
        create_shared_dir, ensure_traversable_dir, reset_workspace, warn_unreachable_ancestors,
        ExecKind, Executor, RESOURCE_FETCH_BUDGET, SINGLE_DOWNLOAD_BUDGET, UPLOAD_BUDGET,
    },
    schema::*,
    service::auth::{
        cred::get_user_credential, credential_guard::CredentialGuard, get_and_prompt_username,
    },
};

/// The number of task slots of a job. A slot is a working directory and an [`Executor`]. Two
/// tasks must not use the same slot at the same time, because the `result/` directory of the
/// process becomes its artifact.
///
/// The value is `FixedWorkers.worker_count` of the suite. It is at least one, because a job with
/// no slot cannot run the tasks of the suite.
fn task_slots(suite: &TaskSuiteSpec) -> usize {
    match suite.worker_schedule {
        WorkerSchedulePlan::FixedWorkers { worker_count, .. } => (worker_count as usize).max(1),
    }
}

/// The progress of all the slots of a job, shared by the slots and the task buffer.
#[derive(Debug, Default)]
struct SlotProgress {
    /// The number of tasks that ended, in any way.
    completed: AtomicUsize,
    /// Notified when a task ends. The coordinator gives no task to a job that holds all the tasks
    /// that it can hold. Thus a free slot is a reason to fetch again, before the poll interval
    /// ends.
    freed: Notify,
}

impl SlotProgress {
    /// Record that a slot ended a task.
    fn task_ended(&self) {
        self.completed.fetch_add(1, Ordering::Release);
        self.freed.notify_one();
    }

    fn completed(&self) -> usize {
        self.completed.load(Ordering::Acquire)
    }
}

/// The interval at which a job with no work fetches tasks from its suite again. This is the
/// backup: the coordinator sends `TasksAvailable` for a new task in the suite. The poll is
/// necessary only when the notification cannot come: the WebSocket is down, or a slot of this job
/// submitted a downstream task.
const HOLD_POLL_INTERVAL: Duration = Duration::from_secs(1);

pub struct MitoAgent {
    coordinator_addr: Url,
    token: String,
    /// The highest notification id that the agent processed. Every heartbeat sends it.
    notification_counter: u64,
    /// The coordinator process that issued `notification_counter`. A different value means that
    /// the coordinator restarted and the counter starts again.
    coordinator_boot_id: Option<Uuid>,
    state: AgentState,
    assigned_suite_uuid: Option<Uuid>,
    has_pending_suite: bool,
    heartbeat_interval: Duration,
    connect_retry_interval: Duration,
    /// The interval at which an idle agent asks for a suite with no notification. `None` waits
    /// for a notification.
    idle_poll_interval: Option<Duration>,
    /// The time to wait before the agent opens a dropped notification WebSocket again.
    ws_reconnect_interval: Duration,
    /// Open no notification WebSocket. The heartbeat gives all notifications.
    no_ws: bool,
    /// The root of the working directories of this agent. Each job has one subdirectory.
    cache_path: PathBuf,
    http_client: reqwest::Client,
    redis_client: Option<redis::Client>,
    /// Stops the job that runs now. `None` while the agent is idle.
    job_token: Option<CancellationToken>,
    /// Stops the job from claiming tasks, but lets the job end correctly. `None` while the agent
    /// is idle.
    drain_token: Option<CancellationToken>,
    /// The job that runs now. Its result tells if more work is available for this agent.
    current_run: Option<JoinHandle<bool>>,
    /// Stops the main loop.
    shutdown_token: CancellationToken,
    /// Set by the coordinator when it rejects the credential of the agent.
    coordinator_force_exit: Arc<AtomicBool>,
    /// Set by a graceful `Shutdown` notification or by Ctrl+C: finish the current job, then stop.
    /// Do not accept a new suite.
    stop_after_current: bool,
    /// Increased when the coordinator notifies that the suite of the job has tasks. It is a counter
    /// and not a flag: the receiver only sees that it changed, and a change during a fetch stays
    /// visible for the next wait.
    fetch_wake: watch::Sender<u64>,
}

impl MitoAgent {
    pub async fn main(cli: AgentConfigCli) {
        // `set_default` and not `init`: this subscriber is only for the config and the
        // registration. The real subscriber needs the agent id from the coordinator, thus `setup`
        // installs it and drops this one.
        let bootstrap = tracing::dispatcher::set_default(&tracing::Dispatch::new(
            tracing_subscriber::registry()
                .with(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| "netmito=info".into()),
                )
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_file(true)
                        .with_line_number(true),
                ),
        ));
        match AgentConfig::new(&cli) {
            Ok(config) => match Self::setup(config, bootstrap).await {
                Ok((mut agent, _guards)) => {
                    if let Err(e) = agent.run().await {
                        tracing::error!("Agent failed: {e}");
                    }
                    agent.cleanup().await;
                }
                Err(e) => {
                    tracing::error!("{e}");
                }
            },
            Err(e) => {
                tracing::error!("{e}");
            }
        }
    }

    /// Register with the coordinator and prepare the working directory.
    ///
    /// `bootstrap` is the temporary subscriber of `main`. This function releases it when the real
    /// subscriber is installed. A `DefaultGuard` is thread-local. This is safe because `main` runs
    /// in the future of `block_on`, which always stays on the thread that calls it.
    pub async fn setup(
        mut config: AgentConfig,
        bootstrap: tracing::subscriber::DefaultGuard,
    ) -> Result<(Self, TracingGuard)> {
        tracing::info!("Starting agent against {}", config.coordinator_addr);
        tracing::info!(
            "groups={:?} tags={:?} labels={:?}",
            config.groups,
            config.tags,
            config.labels
        );

        let http_client = reqwest::Client::new();
        let mut credential_guard = CredentialGuard::new(
            config
                .credential_path
                .as_ref()
                .map(|credential_path| credential_path.relative()),
            &config.coordinator_addr,
        )
        .await;
        let username = match &config.user {
            Some(name) => name.to_string(),
            None => get_and_prompt_username(None, "Please input username")?,
        };
        // Like the worker: a restart of the agent must not change the auth signature of the user.
        // A new signature makes all the tokens issued before it invalid.
        let (_, user_credential) = get_user_credential(
            &mut credential_guard,
            &http_client,
            config.coordinator_addr.clone(),
            username,
            config.password.take(),
            false,
        )
        .await?;

        let machine_code = Self::resolve_machine_code(config.machine_code.take());
        tracing::info!("Machine code: {machine_code}");

        let mut register_url = config.coordinator_addr.clone();
        register_url.set_path("agents");
        let req = RegisterAgentReq {
            tags: config.tags.clone(),
            labels: config.labels.clone(),
            admin_group: config.admin_group.clone(),
            groups: config.groups.clone(),
            lifetime: config.lifetime,
            machine_code,
            metadata: Some(AgentMetadata {
                version: env!("CARGO_PKG_VERSION").to_string(),
                long_version: env!("CARGO_PKG_VERSION").to_string(),
            }),
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
        let register: RegisterAgentResp = parse_json(resp, "register agent").await?;

        let mut cache_path =
            dirs::cache_dir().ok_or(error::Error::Custom("Cache dir not found".to_string()))?;
        // Make one level at a time, and make each level traversable. `create_dir_all` applies the
        // umask to the levels that it creates, and one `0700` level hides the working directory
        // from a task that changed user.
        warn_unreachable_ancestors(&cache_path).await;
        cache_path.push("mitosis");
        ensure_traversable_dir(&cache_path).await?;
        cache_path.push("agent");
        ensure_traversable_dir(&cache_path).await?;
        cache_path.push(register.agent_uuid.to_string());
        ensure_traversable_dir(&cache_path).await?;

        let guards = config.setup_tracing_subscriber::<&Uuid, _>(&register.agent_uuid)?;
        // The subscriber of the agent is installed now. Remove the temporary one.
        drop(bootstrap);
        tracing::info!(
            "Registered as agent {} ({})",
            register.agent_uuid,
            if register.reused {
                "re-adopted this machine's existing agent"
            } else {
                "new agent"
            }
        );
        tracing::info!("Working directory: {}", cache_path.display());

        let redis_client = if config.skip_redis {
            None
        } else {
            register.redis_url.and_then(|url| {
                redis::Client::open(url)
                    .inspect_err(|e| tracing::warn!("Agent cannot setup redis conn: {}", e))
                    .ok()
            })
        };

        let mut coordinator_addr = config.coordinator_addr;
        coordinator_addr.set_path("");

        Ok((
            MitoAgent {
                coordinator_addr,
                token: register.token,
                notification_counter: register.notification_counter,
                coordinator_boot_id: None,
                state: AgentState::Idle,
                assigned_suite_uuid: None,
                // The coordinator cannot notify an agent before it exists. Thus the registration
                // is also a reason to look for work.
                has_pending_suite: true,
                heartbeat_interval: config.heartbeat_interval,
                connect_retry_interval: config.connect_retry_interval,
                idle_poll_interval: config.idle_poll_interval,
                ws_reconnect_interval: config.ws_reconnect_interval,
                no_ws: config.no_ws,
                cache_path,
                http_client,
                redis_client,
                job_token: None,
                drain_token: None,
                current_run: None,
                shutdown_token: CancellationToken::new(),
                coordinator_force_exit: Arc::new(AtomicBool::new(false)),
                stop_after_current: false,
                fetch_wake: watch::Sender::new(0),
            },
            guards,
        ))
    }

    /// Find a stable machine code for this host.
    ///
    /// The order: the config value, the cached value in the mitosis config directory,
    /// `/etc/machine-id`, a new UUID. The agent caches the code that it finds (but not a config
    /// value). Thus the code stays the same after a restart, also on a host with no
    /// `/etc/machine-id` (a container, a host that is not Linux). The coordinator finds the agent
    /// row by this code, thus a machine that comes back with a different code leaves its old row.
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
                tracing::info!("No /etc/machine-id available; generating a machine code");
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
                        "Failed to cache the machine code at {}: {e}; a new one may be generated next start",
                        path.display()
                    );
                }
            }
            None => tracing::warn!(
                "No config directory to cache the machine code in; a new one may be generated next start"
            ),
        }

        code
    }

    fn api_url(&self, path: &str) -> Url {
        let mut url = self.coordinator_addr.clone();
        url.set_path(path);
        url
    }

    pub async fn run(&mut self) -> Result<()> {
        let cancel_token = self.shutdown_token.clone();

        // Cancelled when the first Ctrl+C comes.
        let signal_token = CancellationToken::new();
        tokio::spawn({
            let shutdown_token = cancel_token.clone();
            let signal_token = signal_token.clone();
            async move {
                crate::signal::shutdown_signal(shutdown_token).await;
                signal_token.cancel();
            }
        });

        // With `no_ws`, the agent opens no socket and the branch that reads it is off. The
        // channel exists in all cases, because `select!` evaluates the expression of a branch
        // also when its guard is false.
        let ws_enabled = !self.no_ws;
        let (ws_tx, ws_rx) = crossfire::spsc::bounded_async::<WsNotificationEvent>(32);
        let ws_handle = if ws_enabled {
            Some(self.spawn_websocket_client(ws_tx, cancel_token.clone()))
        } else {
            tracing::info!("WebSocket disabled; Notifications rely purely on heartbeat");
            None
        };

        let mut heartbeat_timer = tokio::time::interval(self.heartbeat_interval);
        heartbeat_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        let (idle_poll_enabled, mut idle_poll_timer) = match self.idle_poll_interval {
            Some(interval) => {
                tracing::info!(
                    "Idle agent will poll for a suite every {}s",
                    interval.as_secs()
                );
                let mut timer = tokio::time::interval(interval);
                timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                (true, timer)
            }
            None => {
                // The branch of this timer is off when `idle_poll_enabled` is false. Thus this
                // timer never fires.
                let mut timer = tokio::time::interval(self.heartbeat_interval);
                timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                (false, timer)
            }
        };

        tracing::info!("Agent entering its main loop");
        loop {
            tokio::select! {
                biased;
                _ = cancel_token.cancelled() => {
                    tracing::info!("Shutdown signalled, leaving the main loop");
                    // Stop the job, thus its reports stop. The coordinator reclaims its tasks
                    // when the heartbeat of the agent stops.
                    if let Some(token) = &self.job_token {
                        token.cancel();
                    }
                    break;
                }

                // Off after a graceful stop starts. Thus this branch does not fire again on each
                // pass, and a second Ctrl+C goes to the shared handler, which stops the process.
                _ = signal_token.cancelled(), if !self.stop_after_current => {
                    tracing::info!(
                        "Signal received; finishing the current job before exiting \
                         (Ctrl+C again to exit now)"
                    );
                    self.begin_graceful_shutdown();
                }

                Ok(event) = ws_rx.recv(), if ws_enabled => {
                    self.notification_counter = self.notification_counter.max(event.id);
                    self.handle_notification(event.event);
                }

                _ = heartbeat_timer.tick() => {
                    if let Err(e) = self.send_heartbeat().await {
                        tracing::error!("Heartbeat failed: {e}");
                    }
                }

                _ = idle_poll_timer.tick(), if idle_poll_enabled => {
                    // The `advance` below then asks for a suite. A busy agent ignores the flag.
                    if self.state == AgentState::Idle && !self.stop_after_current {
                        self.has_pending_suite = true;
                    }
                }
            }

            if let Err(e) = self.advance().await {
                tracing::error!("Agent state handling failed: {e}");
            }
            if self.stop_after_current && self.current_run.is_none() {
                tracing::info!("Nothing left to run and a stop was requested; exiting");
                cancel_token.cancel();
                break;
            }
        }

        drop(ws_rx);
        // Let the job end, thus its last reports arrive.
        if let Some(handle) = self.current_run.take() {
            let _ = handle.await;
        }
        if let Some(handle) = ws_handle {
            let _ = handle.await;
        }
        if self.coordinator_force_exit.load(Ordering::Acquire) {
            tracing::info!("Agent exits because the coordinator rejected its credential");
        } else {
            tracing::info!("Agent stopped");
        }
        Ok(())
    }

    /// Tell the coordinator that this agent stops. Then the coordinator does not wait for the
    /// heartbeat timeout.
    ///
    /// The timeout is short, and a failure is only logged: a coordinator that does not answer must
    /// not keep the process open, and its heartbeat check retires the agent later.
    pub async fn cleanup(&self) {
        if let Err(e) = self
            .http_client
            .delete(self.api_url("agents").as_str())
            .bearer_auth(&self.token)
            .timeout(Duration::from_secs(3))
            .send()
            .await
        {
            tracing::warn!("Failed to tell the coordinator we are exiting: {e}");
        }
    }

    fn spawn_websocket_client(
        &self,
        mut notification_tx: AsyncTx<WsNotificationEvent>,
        cancel_token: CancellationToken,
    ) -> JoinHandle<()> {
        let mut url = self.coordinator_addr.clone();
        let _ = url.set_scheme(if url.scheme() == "https" { "wss" } else { "ws" });
        url.set_path("ws/agents");
        let ws_url = url.to_string();
        let token = self.token.clone();
        let reconnect_interval = self.ws_reconnect_interval;

        tokio::spawn(async move {
            while !cancel_token.is_cancelled() {
                tracing::debug!("Connecting to {ws_url}");
                match Self::websocket_session(&ws_url, &token, &mut notification_tx, &cancel_token)
                    .await
                {
                    Ok(()) => tracing::debug!("Agent WebSocket closed"),
                    // The heartbeat gives the same events, thus a failure here is not fatal.
                    Err(e) => tracing::warn!("Agent WebSocket error: {e}"),
                }
                tokio::select! {
                    _ = cancel_token.cancelled() => break,
                    _ = tokio::time::sleep(reconnect_interval) => {}
                }
            }
            tracing::debug!("Agent WebSocket task stopped");
        })
    }

    /// One WebSocket session: read the notifications, give each one to the main loop, then
    /// acknowledge it. Returns when the socket closes or the agent stops.
    async fn websocket_session(
        ws_url: &str,
        token: &str,
        notification_tx: &mut AsyncTx<WsNotificationEvent>,
        cancel_token: &CancellationToken,
    ) -> Result<()> {
        let host = Url::parse(ws_url)
            .ok()
            .and_then(|u| {
                u.host_str().map(|h| match u.port() {
                    Some(port) => format!("{h}:{port}"),
                    None => h.to_string(),
                })
            })
            .unwrap_or_default();
        let request = tokio_tungstenite::tungstenite::http::Request::builder()
            .uri(ws_url)
            .header("Host", host)
            .header("Authorization", format!("Bearer {token}"))
            .header("Sec-WebSocket-Version", "13")
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header(
                "Sec-WebSocket-Key",
                tokio_tungstenite::tungstenite::handshake::client::generate_key(),
            )
            .body(())
            .map_err(|e| error::Error::Custom(format!("Invalid WebSocket request: {e}")))?;

        let (ws_stream, _) = tokio_tungstenite::connect_async(request)
            .await
            .map_err(|e| error::Error::Custom(format!("WebSocket connect failed: {e}")))?;
        tracing::info!("Agent WebSocket connected");

        let (mut ws_write, mut ws_read) = ws_stream.split();
        loop {
            let msg = tokio::select! {
                _ = cancel_token.cancelled() => return Ok(()),
                msg = ws_read.next() => msg,
            };
            let Some(msg) = msg else { return Ok(()) };
            match msg {
                Ok(WsMessage::Binary(bytes)) => {
                    let event = match WsNotificationEvent::read_from_buffer(&bytes) {
                        Ok(event) => event,
                        Err(e) => {
                            tracing::warn!("Unparseable notification: {e}");
                            continue;
                        }
                    };
                    let id = event.id;
                    let send_result = tokio::select! {
                        biased;
                        _ = cancel_token.cancelled() => return Ok(()),
                        result = notification_tx.send(event) => result,
                    };
                    if send_result.is_err() {
                        return Ok(());
                    }
                    // Acknowledge only after the main loop has the event. Thus the coordinator
                    // keeps the event for a replay until the agent has it.
                    let ack = AgentWsMessage::Ack {
                        notification_id: id,
                    };
                    if let Ok(payload) = ack.write_to_vec() {
                        let _ = ws_write.send(WsMessage::Binary(payload.into())).await;
                    }
                }
                Ok(WsMessage::Ping(_)) | Ok(WsMessage::Pong(_)) | Ok(WsMessage::Frame(_)) => {}
                Ok(WsMessage::Text(text)) => {
                    tracing::debug!(%text, "Ignoring an unexpected text frame")
                }
                Ok(WsMessage::Close(frame)) => {
                    tracing::debug!(?frame, "Coordinator closed the WebSocket");
                    return Ok(());
                }
                Err(e) => return Err(error::Error::Custom(format!("WebSocket error: {e}"))),
            }
        }
    }

    /// Start a graceful stop: the tasks that run now finish and the cleanup hook runs, but the
    /// job claims no more tasks and the agent accepts no new suite. An idle agent leaves the main
    /// loop on the next pass.
    fn begin_graceful_shutdown(&mut self) {
        self.stop_after_current = true;
        self.has_pending_suite = false;
        if let Some(token) = &self.drain_token {
            token.cancel();
        }
    }

    fn handle_notification(&mut self, notification: AgentNotification) {
        match notification {
            AgentNotification::SuiteAvailable => {
                tracing::debug!("A suite is available");
                self.has_pending_suite = true;
            }
            AgentNotification::TasksAvailable { suite_uuid } => {
                // For the job that runs now, not for the idle loop: the coordinator keeps tasks
                // for this job and asks it to fetch now, before its poll interval ends.
                if self.assigned_suite_uuid == Some(suite_uuid) {
                    tracing::debug!(%suite_uuid, "Tasks are waiting for this job");
                    self.fetch_wake.send_modify(|n| *n = n.wrapping_add(1));
                } else {
                    tracing::debug!(%suite_uuid, "Ignoring a task offer for a suite we left");
                }
            }
            AgentNotification::PreemptSuite {
                new_suite_uuid,
                current_suite_uuid,
                ..
            } => {
                // The coordinator does not send this yet. The agent handles it now, thus only the
                // coordinator must change to start preemption.
                if self.assigned_suite_uuid == Some(current_suite_uuid) {
                    tracing::info!("Preempting suite {current_suite_uuid} for {new_suite_uuid}");
                    if let Some(token) = &self.job_token {
                        token.cancel();
                    }
                    self.has_pending_suite = true;
                }
            }
            AgentNotification::SuiteCancelled {
                suite_uuid,
                reason,
                graceful,
            } => {
                if self.assigned_suite_uuid == Some(suite_uuid) {
                    tracing::warn!(
                        "Suite {suite_uuid} was cancelled (graceful={graceful}): {reason}"
                    );
                    // A graceful cancel only drains. The cleanup hook runs under the job token,
                    // thus a cancel of the job token also stops the cleanup hook.
                    let token = if graceful {
                        &self.drain_token
                    } else {
                        &self.job_token
                    };
                    if let Some(token) = token {
                        token.cancel();
                    }
                }
            }
            AgentNotification::TasksCancelled { task_uuids } => {
                // The agent keeps no record for this: a report for a task that is gone gets 404,
                // and the slot continues with the next task.
                tracing::debug!(count = task_uuids.len(), "Tasks cancelled");
            }
            AgentNotification::StopJob {
                suite_uuid,
                graceful,
            } => {
                // Compare the suite: a stop that comes after the agent moved to a different suite
                // must not stop the new job.
                if self.assigned_suite_uuid == Some(suite_uuid) {
                    tracing::info!(
                        "A user asked this agent to stop its job on suite {suite_uuid} \
                         (graceful={graceful})"
                    );
                    // Do not set `stop_after_current`: after a stop, the agent picks a suite
                    // again. This is how the agent gets to work with a higher priority.
                    let token = if graceful {
                        &self.drain_token
                    } else {
                        &self.job_token
                    };
                    if let Some(token) = token {
                        token.cancel();
                    }
                }
            }
            AgentNotification::Shutdown { graceful } => {
                tracing::warn!("Coordinator asked this agent to shut down (graceful={graceful})");
                if graceful {
                    self.begin_graceful_shutdown();
                } else {
                    if let Some(token) = &self.job_token {
                        token.cancel();
                    }
                    self.shutdown_token.cancel();
                }
            }
            AgentNotification::CounterSync { counter, boot_id } => {
                if self.coordinator_boot_id != Some(boot_id) {
                    if let Some(previous) = self.coordinator_boot_id {
                        tracing::warn!("Coordinator restarted ({previous} → {boot_id})");
                    }
                    self.coordinator_boot_id = Some(boot_id);
                    self.notification_counter = counter;
                } else if counter > self.notification_counter {
                    self.notification_counter = counter;
                }
            }
        };
    }

    async fn send_heartbeat(&mut self) -> Result<()> {
        let req = AgentHeartbeatReq {
            state: self.state,
            last_notification_id: self.notification_counter,
        };
        let resp = self
            .http_client
            .post(self.api_url("agents/heartbeat").as_str())
            .bearer_auth(&self.token)
            .json(&req)
            .send()
            .await
            .map_err(error::map_reqwest_err)?;
        if resp.status() == StatusCode::UNAUTHORIZED {
            // The coordinator retired this agent: the heartbeat timed out, or a user stopped the
            // agent with force. The coordinator accepts no more calls with this token. Only a new
            // registration can continue, and the supervisor of the process does that. Thus stop
            // as a forced shutdown does.
            tracing::warn!("The coordinator no longer recognises this agent; exiting");
            self.coordinator_force_exit.store(true, Ordering::Release);
            if let Some(token) = &self.job_token {
                token.cancel();
            }
            self.shutdown_token.cancel();
            return Ok(());
        }
        let resp: AgentHeartbeatResp = parse_json(resp, "heartbeat").await?;

        // The notifications that the WebSocket did not give come here.
        for event in resp.notifications {
            self.notification_counter = self.notification_counter.max(event.id);
            self.handle_notification(event.event);
        }
        Ok(())
    }

    async fn advance(&mut self) -> Result<()> {
        match self.state {
            AgentState::Idle => {
                if self.stop_after_current {
                    return Ok(());
                }
                // Ask for a suite only after a signal: a notification, the registration, the end
                // of a job with more work, or the idle poll.
                if !self.has_pending_suite {
                    return Ok(());
                }
                // One request for each signal. If no suite is available, the agent stays idle
                // until the next signal and does not ask again in a loop.
                self.has_pending_suite = false;

                let Some((suite, job, job_id, hooks)) = self.fetch_suite().await? else {
                    return Ok(());
                };
                tracing::info!("Accepted suite {} as job {job_id}", suite.uuid);

                self.assigned_suite_uuid = Some(suite.uuid);
                let job_token = CancellationToken::new();
                self.job_token = Some(job_token.clone());
                let drain_token = CancellationToken::new();
                self.drain_token = Some(drain_token.clone());
                let runner = SuiteRunner {
                    http_client: self.http_client.clone(),
                    token: self.token.clone(),
                    coordinator_addr: self.coordinator_addr.clone(),
                    connect_retry_interval: self.connect_retry_interval,
                    redis_client: self.redis_client.clone(),
                    job_id,
                    hooks: Arc::new(hooks),
                    cache_path: self.cache_path.join("job"),
                    job_token,
                    shutdown_token: self.shutdown_token.clone(),
                    coordinator_force_exit: self.coordinator_force_exit.clone(),
                    drain_token,
                    fetch_wake: self.fetch_wake.subscribe(),
                };
                self.current_run = Some(tokio::spawn(runner.run(suite, job)));
                self.state = AgentState::Executing;
            }
            AgentState::Executing => {
                // Do not wait for the job here, thus the heartbeats continue.
                let finished = self
                    .current_run
                    .as_ref()
                    .map(JoinHandle::is_finished)
                    .unwrap_or(true);
                if finished {
                    if let Some(handle) = self.current_run.take() {
                        match handle.await {
                            // The answer to `Complete` says that more work waits for this agent.
                            Ok(true) => self.has_pending_suite = true,
                            Ok(false) => {}
                            Err(e) => tracing::error!("The suite runner task failed: {e}"),
                        }
                    }
                    self.assigned_suite_uuid = None;
                    self.job_token = None;
                    self.drain_token = None;
                    self.state = AgentState::Idle;
                    tracing::info!("Agent is idle again");
                }
            }
            // The coordinator owns these states. The agent only reports `Idle` and `Executing`;
            // the job reports `Start` and `EnterCleanup` mark its other phases.
            AgentState::Provisioning | AgentState::Cleaning | AgentState::Offline => {}
        }
        Ok(())
    }

    /// Ask for a suite and accept it in one call. `None` means that no suite is available.
    async fn fetch_suite(&self) -> Result<Option<(TaskSuiteSpec, i64, i32, Vec<JobHook>)>> {
        let resp = self
            .http_client
            .post(self.api_url("agents/suite").as_str())
            .bearer_auth(&self.token)
            .json(&AcceptSuiteReq { suite_uuid: None })
            .send()
            .await
            .map_err(error::map_reqwest_err)?;
        let resp: AcceptSuiteResp = parse_json(resp, "accept suite").await?;
        if !resp.accepted {
            tracing::debug!("No suite accepted: {}", resp.reason.unwrap_or_default());
            return Ok(None);
        }
        let Some(suite) = resp.suite else {
            return Err(error::Error::Custom(
                "the coordinator accepted a suite but sent no spec".to_string(),
            ));
        };
        let hooks = resp.hooks;
        Ok(resp
            .job
            .zip(resp.job_id)
            .map(|(job, id)| (suite, job, id, hooks)))
    }
}

/// Runs one accepted suite from the provision hook to a terminal job state.
#[derive(Clone)]
struct SuiteRunner {
    http_client: reqwest::Client,
    token: String,
    coordinator_addr: Url,
    connect_retry_interval: Duration,
    redis_client: Option<redis::Client>,
    /// The number of the job in its suite. Use it in logs: `suites jobs <suite> --job N` takes
    /// it. The `job` handle in the calls below is `suite_agent_jobs.id`, an internal key that the
    /// user does not see.
    job_id: i32,
    /// The hook rows of the job, from the accept.
    hooks: Arc<Vec<JobHook>>,
    /// The working directory of this job.
    cache_path: PathBuf,
    /// Cancelled when the suite is cancelled or preempted, or when the agent stops. All that the
    /// job runs (tasks, hooks) runs under it.
    job_token: CancellationToken,
    /// The shutdown of the agent. Only this token stops the retries of a job report, because the
    /// agent must report a cancelled job too.
    shutdown_token: CancellationToken,
    coordinator_force_exit: Arc<AtomicBool>,
    /// Cancelled to stop the claims of tasks, but not the end of the job: the running tasks
    /// finish and commit, then the cleanup hook runs under `job_token`. Thus the two tokens are
    /// different. The job drops the tasks that it claimed but did not start, and the coordinator
    /// reclaims them at `Complete`.
    drain_token: CancellationToken,
    /// Changes when the coordinator notifies that tasks of this suite wait for this job. With it, the
    /// job does not wait for the next poll. The poll stays as the backup for a dropped socket.
    fetch_wake: watch::Receiver<u64>,
}

impl SuiteRunner {
    fn api_url(&self, path: &str) -> Url {
        let mut url = self.coordinator_addr.clone();
        url.set_path(path);
        url
    }

    /// Each phase logs an error and continues, because only the agent can move the job to a
    /// terminal state and free itself.
    ///
    /// Returns true if the answer to `Complete` says that more work waits for this agent.
    async fn run(self, suite: TaskSuiteSpec, job: i64) -> bool {
        let suite_uuid = suite.uuid;
        let mut failure: Option<JobFailureReason> = None;

        let provisioned = match self.prepare_workspace().await {
            Ok(()) => self.run_hook(&suite, HookType::Provision).await,
            Err(e) => Err(e),
        };
        if let Err(e) = provisioned {
            tracing::error!("Provisioning failed for suite {suite_uuid}: {e}");
            failure = Some(JobFailureReason {
                kind: JobFailureKind::ProvisionFailed,
                message: e.to_string(),
            });
        }

        if let Err(e) = self.report_job(job, JobReportOp::Start).await {
            tracing::error!("Failed to start suite {suite_uuid}: {e}");
        }

        let background_stop_token = CancellationToken::new();
        let mut background = None;

        // A stopped job runs no task and no background hook.
        if failure.is_none() && !self.job_token.is_cancelled() {
            background = self.spawn_background_hook(&suite, background_stop_token.clone());
            if let Err(e) = self.execute_tasks(&suite, job).await {
                tracing::error!("Task execution failed for suite {suite_uuid}: {e}");
                failure = Some(JobFailureReason {
                    kind: JobFailureKind::ExecutionError,
                    message: e.to_string(),
                });
            }
        }

        if let Some(handle) = background {
            // A background hook must run until the tasks end. If it stops first, the job failed.
            // The hook cannot know when the job has no more tasks, thus the agent stops it. The
            // executor then reports the hook with its output.
            let exited_early = handle.is_finished();
            background_stop_token.cancel();
            if let Err(e) = handle.await {
                tracing::error!("The background hook task failed for suite {suite_uuid}: {e}");
            }
            if exited_early {
                failure.get_or_insert(JobFailureReason {
                    kind: JobFailureKind::BackgroundExited,
                    message: "the background hook exited before the tasks drained".to_string(),
                });
            }
        }

        if let Err(e) = self.report_job(job, JobReportOp::EnterCleanup).await {
            tracing::error!("Failed to enter cleanup for suite {suite_uuid}: {e}");
        }

        if let Err(e) = self.run_hook(&suite, HookType::Cleanup).await {
            tracing::error!("Cleanup hook failed for suite {suite_uuid}: {e}");
            failure.get_or_insert(JobFailureReason {
                kind: JobFailureKind::CleanupFailed,
                message: e.to_string(),
            });
        }

        let outcome = match failure {
            None => SuiteJobOutcome::Completed,
            Some(reason) => SuiteJobOutcome::Failed { reason },
        };
        let next_available = match self
            .report_job(job, JobReportOp::Complete { outcome })
            .await
        {
            Ok(Some(resp)) => {
                let next_available = resp.next_suite_available.unwrap_or(false);
                tracing::info!(
                    "Finished suite {suite_uuid} (job {}); more work waiting: {next_available}",
                    self.job_id
                );
                next_available
            }
            Ok(None) => false,
            // A `Complete` that did not arrive tells nothing about the next work.
            Err(e) => {
                tracing::error!("Failed to complete suite {suite_uuid}: {e}");
                false
            }
        };

        // All the outputs are uploaded now.
        let _ = tokio::fs::remove_dir_all(&self.cache_path).await;
        next_available
    }

    /// The directory that the provision hook, the tasks and the cleanup hook all get as
    /// `MITO_SUITE_SHARED`.
    fn shared_path(&self) -> PathBuf {
        self.cache_path.join("share")
    }

    /// Create the file tree of a job. The processes only go through the job root. They get
    /// `share/` as `MITO_SUITE_SHARED`, thus it is open to the user that they run as.
    async fn prepare_workspace(&self) -> Result<()> {
        let _ = tokio::fs::remove_dir_all(&self.cache_path).await;
        ensure_traversable_dir(&self.cache_path).await?;
        create_shared_dir(&self.shared_path()).await?;
        Ok(())
    }

    /// Make an executor for one slot or one hook of this job, with its own working directory.
    async fn executor(
        &self,
        cache_path: PathBuf,
        cancel_token: CancellationToken,
        stop_token: CancellationToken,
    ) -> Executor {
        let task_redis_conn = if let Some(ref client) = self.redis_client {
            client
                .get_multiplexed_tokio_connection()
                .await
                .inspect_err(|e| tracing::warn!("{}", e))
                .ok()
        } else {
            None
        };
        let task_redis_pubsub = if let Some(ref client) = self.redis_client {
            client
                .get_async_pubsub()
                .await
                .inspect_err(|e| tracing::warn!("{}", e))
                .ok()
        } else {
            None
        };
        Executor {
            http_client: self.http_client.clone(),
            task_credential: self.token.clone(),
            http_url: self.coordinator_addr.clone(),
            task_cancel_token: cancel_token,
            task_stop_token: stop_token,
            runner_cancel_token: self.shutdown_token.clone(),
            coordinator_force_exit: self.coordinator_force_exit.clone(),
            polling_interval: self.connect_retry_interval,
            task_cache_path: cache_path,
            extra_envs: vec![(
                "MITO_SUITE_SHARED".to_string(),
                self.shared_path().to_string_lossy().into_owned(),
            )],
            task_redis_conn,
            task_redis_pubsub,
            resource_fetch_budget: RESOURCE_FETCH_BUDGET,
            single_download_budget: SINGLE_DOWNLOAD_BUDGET,
            upload_budget: UPLOAD_BUDGET,
        }
    }

    /// Claim the tasks of the suite into a buffer and run them in `slots` slots.
    ///
    /// This function keeps the buffer: it does all the claims, all the fetches of a job that
    /// waits for work, and the decision to end the job. The slots only take the tasks that it
    /// gives them. Thus this work is not done once for each slot.
    ///
    /// The coordinator decides how many tasks the job can claim at a time: it knows the schedule
    /// of the suite and the tasks that this job holds. The channel holds one task for each slot,
    /// thus `send` waits when each slot has its next task. `staged` holds the remaining tasks of a
    /// batch.
    async fn execute_tasks(&self, suite: &TaskSuiteSpec, job: i64) -> Result<()> {
        let slots = task_slots(suite);
        tracing::info!(
            "Running the tasks of suite {} across {slots} slots",
            suite.uuid,
        );

        let (task_tx, task_rx) = crossfire::mpmc::bounded_async::<WorkerTaskResp>(slots);
        // Compared with the count of tasks that the buffer gave. Equal counts mean that all the
        // tasks ended. A check of idle slots cannot tell this, because a slot can take a task
        // during the check.
        let progress = Arc::new(SlotProgress::default());

        let mut running = tokio::task::JoinSet::new();
        for slot in 0..slots {
            let runner = self.clone();
            let task_rx = task_rx.clone();
            let progress = progress.clone();
            running.spawn(async move { runner.run_slot(job, slot, task_rx, progress).await });
        }
        // Only the slots keep a receiver.
        drop(task_rx);

        let filled = self.fill_buffer(suite.uuid, task_tx, progress).await;

        while let Some(joined) = running.join_next().await {
            if let Err(e) = joined {
                tracing::error!("A task slot of suite {} panicked: {e}", suite.uuid);
            }
        }
        filled
    }

    /// Keep tasks in the buffer until the suite has no more tasks for this job.
    ///
    /// Returns when the job stops claiming: the suite has no more tasks, the job was cancelled,
    /// or the job drains. The job drops the tasks that are still staged then, and `Complete`
    /// gives them back to `Ready`.
    async fn fill_buffer(
        &self,
        suite_uuid: Uuid,
        task_tx: MAsyncTx<WorkerTaskResp>,
        progress: Arc<SlotProgress>,
    ) -> Result<()> {
        let mut staged: VecDeque<WorkerTaskResp> = VecDeque::new();
        // Cloned once for the full loop, not for each wait. Thus a change that comes during a fetch
        // ends the next wait.
        let mut wake = self.fetch_wake.clone();
        // The number of tasks given to the slots.
        let mut pushed: usize = 0;
        // Only for the logs: the job logs the start of a wait once, not on each poll.
        let mut holding = false;

        loop {
            if self.job_token.is_cancelled() {
                tracing::info!("Suite {suite_uuid} cancelled; claiming no more tasks");
                return Ok(());
            }
            // Check before the claim, not only during a wait: a drain must stop a job that still
            // has tasks to claim. This is the difference between a drain and the normal end of
            // the suite.
            if self.drain_token.is_cancelled() {
                tracing::info!(
                    "Asked to wind down; job {} claims no more tasks",
                    self.job_id
                );
                return Ok(());
            }

            if staged.is_empty() {
                let resp = self.fetch_tasks(suite_uuid).await?;
                if resp.tasks.is_empty() {
                    // The job ends when no slot runs a task (`pushed == completed`) and the
                    // suite gives no more tasks.
                    if !resp.hold_job_open && progress.completed() == pushed {
                        tracing::debug!("Suite {suite_uuid} is idle and has no more tasks");
                        return Ok(());
                    }
                    if !holding {
                        // The suite has no ready task, or this job holds all the tasks that it
                        // can hold. In both cases the wait ends when something changes.
                        tracing::debug!(
                            "Nothing to claim from suite {suite_uuid} for now; job {} stays up \
                             for more work",
                            self.job_id
                        );
                        holding = true;
                    }
                    // The coordinator notifies this job about a new task in the suite. Thus the
                    // wake usually ends the wait. The poll is for a dropped socket and for a task
                    // that a slot of this job submitted during the notification.
                    tokio::select! {
                        _ = self.job_token.cancelled() => return Ok(()),
                        // The checks at the top of the loop also see this. The branch makes
                        // sure that a drain does not wait for the poll interval first.
                        _ = self.drain_token.cancelled() => return Ok(()),
                        _ = wake.changed() => {
                            tracing::debug!(
                                "Job {} was notified that suite {suite_uuid} has tasks",
                                self.job_id
                            );
                        }
                        // A slot is free. The coordinator possibly gives a task now.
                        _ = progress.freed.notified() => {}
                        _ = tokio::time::sleep(HOLD_POLL_INTERVAL) => {}
                    }
                    continue;
                }
                if holding {
                    tracing::debug!(
                        "Suite {suite_uuid} has work for job {} again; it resumes",
                        self.job_id
                    );
                    holding = false;
                }
                staged.extend(resp.tasks);
            }

            // Give the staged tasks one at a time. `send` waits when the channel is full and
            // continues when a slot takes a task. Thus the fetch above runs only when the buffer
            // is empty.
            while let Some(task) = staged.pop_front() {
                tokio::select! {
                    _ = self.job_token.cancelled() => return Ok(()),
                    _ = self.drain_token.cancelled() => return Ok(()),
                    sent = task_tx.send(task) => {
                        if sent.is_err() {
                            // All the slots stopped.
                            return Ok(());
                        }
                        pushed += 1;
                    }
                }
            }
        }
    }

    /// One task slot: run the tasks from the buffer in `task-{slot}` until the channel closes or
    /// the job drains.
    ///
    /// A drain finishes only the task in hand. To close the channel is not sufficient, because a
    /// receiver still gets the tasks that were in the channel before the close. Thus the slot
    /// checks the drain before each task, and `Complete` reclaims the tasks that stay in the
    /// channel.
    async fn run_slot(
        &self,
        job: i64,
        slot: usize,
        task_rx: MAsyncRx<WorkerTaskResp>,
        progress: Arc<SlotProgress>,
    ) {
        let dir = self.cache_path.join(format!("task-{slot}"));
        let mut executor = self
            .executor(
                dir.clone(),
                self.job_token.clone(),
                CancellationToken::new(),
            )
            .await;
        loop {
            let task = tokio::select! {
                // Biased, thus a drain that is set wins over a task in the channel.
                biased;
                _ = self.job_token.cancelled() => break,
                _ = self.drain_token.cancelled() => break,
                task = task_rx.recv() => match task {
                    Ok(task) => task,
                    Err(_) => break,
                },
            };

            tracing::info!("Slot {} running task {}", slot, task.uuid);
            tracing::debug!("Slot {} task {} spec: {:?}", slot, task.uuid, task.spec);
            if let Err(e) = reset_workspace(&dir).await {
                tracing::error!(
                    "Slot {} error prepare workspace for task {}: {}",
                    slot,
                    task.uuid,
                    e
                );
                progress.task_ended();
                continue;
            }
            let task_uuid = task.uuid;
            if let Err(e) = executor
                .execute(ExecKind::Task { job: Some(job) }, task)
                .await
            {
                tracing::error!("Slot {} error executing task {}: {}", slot, task_uuid, e);
            }

            // After the task, in all cases: the buffer uses this to know if work still runs, and
            // if a slot is free.
            progress.task_ended();
        }
        tracing::debug!("Slot {slot} of job {} released", self.job_id);
    }

    /// Run a hook of the suite until it ends. A hook that the suite does not have is not a
    /// failure.
    async fn run_hook(&self, suite: &TaskSuiteSpec, hook_type: HookType) -> Result<()> {
        // A stopped job starts no more hooks.
        if self.job_token.is_cancelled() {
            return Ok(());
        }
        let Some(hook) = self.hook_task(suite, hook_type)? else {
            tracing::debug!("No {hook_type} hook configured for suite {}", suite.uuid);
            return Ok(());
        };
        self.run_hook_task(hook_type, hook, CancellationToken::new())
            .await
    }

    /// Run a hook with the data from [`Self::hook_task`] under the job token. `stop_token` stops
    /// the hook and reports its output. Returns an error if the hook did not complete
    /// successfully.
    async fn run_hook_task(
        &self,
        hook_type: HookType,
        hook: WorkerTaskResp,
        stop_token: CancellationToken,
    ) -> Result<()> {
        tracing::info!("Running the {hook_type} hook of job {}", self.job_id);
        let dir = self.cache_path.join(format!("hook-{hook_type}"));
        reset_workspace(&dir).await?;
        let mut executor = self.executor(dir, self.job_token.clone(), stop_token).await;
        match executor.execute(ExecKind::Hook, hook).await? {
            // Stopped before the commit: the job stops, and the hook did not fail.
            None => Ok(()),
            // The same rule as the coordinator: a hook that exited with 0 completed. A stopped
            // background hook must also exit with 0. A different message means that the process
            // did not run or ran out of time.
            Some(result)
                if result.exit_status == 0
                    && matches!(
                        result.msg,
                        None | Some(TaskResultMessage::UploadResultTimeout)
                            | Some(TaskResultMessage::StoppedByAgent)
                    ) =>
            {
                Ok(())
            }
            Some(result) => Err(error::Error::Custom(format!(
                "the {hook_type} hook ended with status {} ({:?})",
                result.exit_status, result.msg
            ))),
        }
    }

    /// Start the background hook if the suite has one. The caller does not wait for it: it runs
    /// as long as the tasks run, and `stop_token` stops it.
    fn spawn_background_hook(
        &self,
        suite: &TaskSuiteSpec,
        stop_token: CancellationToken,
    ) -> Option<JoinHandle<()>> {
        let hook = match self.hook_task(suite, HookType::Background) {
            Ok(hook) => hook?,
            Err(e) => {
                tracing::error!("Background hook failed for suite {}: {e}", suite.uuid);
                return None;
            }
        };
        let runner = self.clone();
        let suite_uuid = suite.uuid;
        Some(tokio::spawn(async move {
            if let Err(e) = runner
                .run_hook_task(HookType::Background, hook, stop_token)
                .await
            {
                tracing::error!("Background hook failed for suite {suite_uuid}: {e}");
            }
        }))
    }

    /// The data to run a hook: the spec from the suite, and the id and uuid of its hook row. The
    /// hook uses the group of the suite for its attachments. `None` if the suite has no such hook.
    fn hook_task(
        &self,
        suite: &TaskSuiteSpec,
        hook_type: HookType,
    ) -> Result<Option<WorkerTaskResp>> {
        let spec = suite.exec_hooks.as_ref().and_then(|hooks| match hook_type {
            HookType::Provision => hooks.provision.clone(),
            HookType::Cleanup => hooks.cleanup.clone(),
            HookType::Background => hooks.background.clone(),
        });
        let Some(spec) = spec else {
            return Ok(None);
        };
        let row = self
            .hooks
            .iter()
            .find(|hook| hook.hook_type == hook_type)
            .ok_or_else(|| {
                error::Error::Custom(format!(
                    "the coordinator sent no row for the {hook_type} hook"
                ))
            })?;
        Ok(Some(WorkerTaskResp {
            id: row.id,
            uuid: row.uuid,
            upstream_task_uuid: None,
            group_name: suite.group_name.clone(),
            spec,
            exec_options: None,
        }))
    }

    async fn fetch_tasks(&self, suite_uuid: Uuid) -> Result<FetchTasksResp> {
        let resp = self
            .http_client
            .get(self.api_url("agents/tasks").as_str())
            .bearer_auth(&self.token)
            .query(&FetchTasksReq { suite_uuid })
            .send()
            .await
            .map_err(error::map_reqwest_err)?;
        // The suite stops giving tasks (400), or it is no longer for this agent (404). This is a
        // normal end of the job, not a failure: stop the claims and continue to the cleanup. A
        // drain and not a cancel of the job, because a cancel also stops the cleanup hook. A
        // forced stop comes as its own notification.
        if resp.status() == StatusCode::BAD_REQUEST || resp.status() == StatusCode::NOT_FOUND {
            tracing::info!("Suite {suite_uuid} stopped handing out tasks");
            self.drain_token.cancel();
            return Ok(FetchTasksResp {
                tasks: Vec::new(),
                hold_job_open: false,
            });
        }
        parse_json(resp, "fetch tasks").await
    }

    /// Report a phase of the job. Retry until the coordinator gives an answer that ends the
    /// report.
    ///
    /// The agent frees itself from the job also when the report fails. If a report is lost, the
    /// coordinator keeps the agent on the job. Thus, different from the task reports, this
    /// function ignores `job_token`, and it retries a 5xx and a failed send, which is the answer
    /// of a coordinator that restarts. A 4xx ends it.
    ///
    /// Returns `None` on 404: the job is closed (a user stopped it with force, or a first
    /// `Complete` arrived but its answer was lost). Then the job stops, and the cleanup hook does
    /// not run.
    async fn report_job(&self, job: i64, op: JobReportOp) -> Result<Option<JobReportResp>> {
        let url = self.api_url("agents/job");
        let req = JobReportReq { job, op };
        loop {
            let resp = self
                .http_client
                .post(url.as_str())
                .bearer_auth(&self.token)
                .json(&req)
                .send()
                .await;
            let failure = match resp {
                Ok(resp) if resp.status() == StatusCode::NOT_FOUND => {
                    tracing::info!("Job {} is closed; stopping it", self.job_id);
                    self.job_token.cancel();
                    return Ok(None);
                }
                Ok(resp) if resp.status().is_server_error() => {
                    format!("coordinator answered {}", resp.status())
                }
                Ok(resp) => return parse_json(resp, "job report").await.map(Some),
                Err(e) if e.is_builder() => return Err(error::map_reqwest_err(e).into()),
                Err(e) => e.to_string(),
            };
            tracing::warn!(
                "Job report failed ({failure}); retrying in {:?}",
                self.connect_retry_interval
            );
            tokio::select! {
                _ = self.shutdown_token.cancelled() => {
                    return Err(error::Error::Custom(format!(
                        "job report abandoned on shutdown: {failure}"
                    )));
                }
                _ = tokio::time::sleep(self.connect_retry_interval) => {}
            }
        }
    }
}

/// Decode a successful JSON response. A status that is not 2xx becomes an error with the message
/// of the coordinator.
async fn parse_json<T: serde::de::DeserializeOwned>(
    resp: reqwest::Response,
    what: &str,
) -> Result<T> {
    if !resp.status().is_success() {
        return Err(error::Error::Custom(format!(
            "{what} failed: {}",
            error::get_error_from_resp(resp).await
        )));
    }
    resp.json::<T>()
        .await
        .map_err(|e| error::Error::Custom(format!("Unreadable {what} response: {e}")))
}
