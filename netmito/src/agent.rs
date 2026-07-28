//! The agent: an orchestrator process that claims task suites from the
//! coordinator and runs them.
//!
//! ```text
//!  register ─▶ ws connect ─┐
//!                          ├─▶ main loop ─▶ (idle + work available) ─▶ accept suite
//!  heartbeat every N ──────┘                                                     │
//!                                                                                ▼
//!                        complete ◀─ cleanup hook ◀─ cleanup ◀─ tasks ◀─ start ◀─ provision hook
//! ```
//!
//! The main loop only ever services heartbeats and notifications; a claimed
//! suite runs in a spawned [`SuiteRunner`] so neither starves the other.
//!

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use reqwest::StatusCode;
use speedy::{Readable, Writable};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use url::Url;
use uuid::Uuid;

use crate::config::{AgentConfig, AgentConfigCli};
use crate::entity::content::ArtifactContentType;
use crate::entity::hook_tasks::HookType;
use crate::entity::state::{AgentState, TaskExecState};
use crate::error::{self, Result};
use crate::executor::{execute, reset_workspace, ExecClient, Executor, UploadTarget};
use crate::schema::*;
use crate::service::auth::cred::get_user_credential;

/// How many task slots one job runs at a time. A slot is a working directory
/// plus an [`Executor`]; two concurrent tasks must never share one, since the
/// process's `result/` directory is what becomes its artifact.
const TASK_SLOTS: usize = 1;

/// How often a poll-based `watch` re-asks the coordinator.
const WATCH_POLL_INTERVAL: Duration = Duration::from_secs(30);

/// How often a job with nothing to do re-checks its suite for late work. This is
/// the latency a task submitted into a warm job pays before it starts.
const HOLD_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// How long to wait before retrying a dropped WebSocket.
const WS_RECONNECT_DELAY: Duration = Duration::from_secs(5);

pub struct MitoAgent;

/// What the agent should claim next, recorded by a notification and consumed in
/// the idle branch of the main loop. Distinct from "nothing pending", so an idle
/// agent never polls unless the coordinator signalled work.
#[derive(Debug, Clone, Copy)]
enum PendingSuite {
    /// Work exists but was not named — take whatever the coordinator offers.
    Any,
    /// A specific suite was named; target it directly.
    Specific(Uuid),
}

struct AgentClient {
    coordinator_addr: Url,
    token: String,
    /// Highest notification id processed; echoed on every heartbeat
    notification_counter: u64,
    /// Which coordinator boot our counter belongs to. A different one means the
    /// coordinator restarted and our sequence is meaningless.
    coordinator_boot_id: Option<Uuid>,
    state: AgentState,
    assigned_suite_uuid: Option<Uuid>,
    pending_suite: Option<PendingSuite>,
    heartbeat_interval: Duration,
    polling_interval: Duration,
    /// Root of this agent's working directories; one subtree per job.
    cache_path: PathBuf,
    http_client: reqwest::Client,
    /// Cancels the running suite (notification-driven); `None` while idle.
    job_token: Option<CancellationToken>,
    /// Releases a *held* suite without aborting its wind-down; `None` while idle.
    drain_token: Option<CancellationToken>,
    /// The suite in flight. Resolves to `complete`'s answer to "is there more
    /// for this agent".
    current_run: Option<JoinHandle<bool>>,
    /// Cancelling this exits the main loop.
    shutdown_token: CancellationToken,
    /// Set by a graceful `Shutdown` notification or `--run-once`: finish the
    /// current suite, then stop instead of taking another.
    stop_after_current: bool,
    run_once: bool,
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

        let run_once = cli.run_once;
        match AgentConfig::new(&cli) {
            Ok(config) => {
                if let Err(e) = Self::run(config, run_once).await {
                    tracing::error!("Agent failed: {e}");
                    std::process::exit(1);
                }
            }
            Err(e) => {
                tracing::error!("{e}");
                std::process::exit(1);
            }
        }
    }

    /// Register with the coordinator and drive the agent loop until shutdown.
    /// Public so tests (and embedders) can run an agent in-process.
    pub async fn run(mut config: AgentConfig, run_once: bool) -> Result<()> {
        tracing::info!("Starting agent against {}", config.coordinator_addr);
        tracing::info!(
            "groups={:?} tags={:?} labels={:?}",
            config.groups,
            config.tags,
            config.labels
        );

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

        tracing::info!(
            "Registered as agent {} ({})",
            register.agent_uuid,
            if register.reused {
                "re-adopted this machine's existing agent"
            } else {
                "new agent"
            }
        );

        let mut coordinator_addr = config.coordinator_addr;
        coordinator_addr.set_path("");

        let mut cache_path =
            dirs::cache_dir().ok_or(error::Error::Custom("Cache dir not found".to_string()))?;
        cache_path.push("mitosis");
        cache_path.push("agent");
        cache_path.push(register.agent_uuid.to_string());
        tokio::fs::create_dir_all(&cache_path).await?;
        tracing::info!("Working directory: {}", cache_path.display());

        let mut client = AgentClient {
            coordinator_addr,
            token: register.token,
            notification_counter: register.notification_counter,
            coordinator_boot_id: None,
            state: AgentState::Idle,
            assigned_suite_uuid: None,
            // Registration is itself a reason to look for work: the coordinator
            // could not have notified us before we existed.
            pending_suite: Some(PendingSuite::Any),
            heartbeat_interval: config.heartbeat_interval,
            polling_interval: config.polling_interval,
            cache_path,
            http_client,
            job_token: None,
            drain_token: None,
            current_run: None,
            shutdown_token: CancellationToken::new(),
            stop_after_current: false,
            run_once,
        };

        client.run_loop().await
    }
}

/// Resolve a stable machine code for this host.
///
/// Precedence: explicit config → the cached value under the mitosis config dir →
/// `/etc/machine-id` → a fresh UUID. Whatever is resolved (unless explicitly
/// overridden) is cached, so the identity survives restarts even where
/// `/etc/machine-id` does not exist — containers, non-Linux hosts. The identity
/// matters: the coordinator keys the agent row on it, and a machine that comes
/// back with a different code strands its old row.
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

impl AgentClient {
    fn api_url(&self, path: &str) -> Url {
        let mut url = self.coordinator_addr.clone();
        url.set_path(path);
        url
    }

    async fn run_loop(&mut self) -> Result<()> {
        let cancel_token = self.shutdown_token.clone();

        let signal_token = cancel_token.clone();
        tokio::spawn(async move {
            tokio::signal::ctrl_c().await.ok();
            tracing::info!("Received SIGINT, shutting down");
            signal_token.cancel();
        });

        let (ws_tx, mut ws_rx) = mpsc::channel::<WsNotificationEvent>(32);
        let ws_handle = self.spawn_websocket_client(ws_tx, cancel_token.clone());

        let mut heartbeat_timer = tokio::time::interval(self.heartbeat_interval);
        heartbeat_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        tracing::info!("Agent entering its main loop");
        loop {
            tokio::select! {
                biased;
                _ = cancel_token.cancelled() => {
                    tracing::info!("Shutdown signalled, leaving the main loop");
                    // Stop the in-flight suite so its reports cease; whatever it
                    // was holding is reclaimed once our heartbeat lapses.
                    if let Some(token) = &self.job_token {
                        token.cancel();
                    }
                    break;
                }

                Some(event) = ws_rx.recv() => {
                    self.notification_counter = self.notification_counter.max(event.id);
                    self.handle_notification(event.event);
                }

                _ = heartbeat_timer.tick() => {
                    if let Err(e) = self.send_heartbeat().await {
                        tracing::error!("Heartbeat failed: {e}");
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

        // Let the in-flight suite drain so its last reports land.
        if let Some(handle) = self.current_run.take() {
            let _ = handle.await;
        }
        if let Some(handle) = ws_handle {
            let _ = handle.await;
        }
        tracing::info!("Agent stopped");
        Ok(())
    }

    // ── notifications ──

    fn spawn_websocket_client(
        &self,
        notification_tx: mpsc::Sender<WsNotificationEvent>,
        cancel_token: CancellationToken,
    ) -> Option<JoinHandle<()>> {
        let mut url = self.coordinator_addr.clone();
        let _ = url.set_scheme(if url.scheme() == "https" { "wss" } else { "ws" });
        url.set_path("ws/agents");
        let ws_url = url.to_string();
        let token = self.token.clone();

        Some(tokio::spawn(async move {
            while !cancel_token.is_cancelled() {
                tracing::debug!("Connecting to {ws_url}");
                match websocket_session(&ws_url, &token, &notification_tx, &cancel_token).await {
                    Ok(()) => tracing::debug!("Agent WebSocket closed"),
                    // Notifications are an optimization — the heartbeat carries
                    // the same events — so a failure here is never fatal.
                    Err(e) => tracing::warn!("Agent WebSocket error: {e}"),
                }
                tokio::select! {
                    _ = cancel_token.cancelled() => break,
                    _ = tokio::time::sleep(WS_RECONNECT_DELAY) => {}
                }
            }
            tracing::debug!("Agent WebSocket task stopped");
        }))
    }

    fn note_pending_suite(&mut self, suite_uuid: Option<Uuid>) {
        match suite_uuid {
            // A named suite always wins: a specific target beats the generic hint.
            Some(uuid) => self.pending_suite = Some(PendingSuite::Specific(uuid)),
            None => {
                if self.pending_suite.is_none() {
                    self.pending_suite = Some(PendingSuite::Any);
                }
            }
        }
    }

    fn handle_notification(&mut self, notification: AgentNotification) {
        match notification {
            AgentNotification::SuiteAvailable {
                suite_uuid,
                priority,
            } => {
                tracing::debug!(?suite_uuid, priority, "Suite available");
                // Recording this while busy is fine — it is only acted on once
                // we are idle again, which gives back-to-back pickup without
                // waiting for the next heartbeat.
                self.note_pending_suite(suite_uuid);
            }
            AgentNotification::PreemptSuite {
                new_suite_uuid,
                current_suite_uuid,
                ..
            } => {
                // The coordinator does not emit this today (a running agent is
                // never interrupted). Handled anyway, so turning preemption on
                // is a coordinator-side change only.
                if self.assigned_suite_uuid == Some(current_suite_uuid) {
                    tracing::info!("Preempting suite {current_suite_uuid} for {new_suite_uuid}");
                    if let Some(token) = &self.job_token {
                        token.cancel();
                    }
                    self.note_pending_suite(Some(new_suite_uuid));
                }
            }
            AgentNotification::SuiteCancelled { suite_uuid, reason } => {
                if self.assigned_suite_uuid == Some(suite_uuid) {
                    tracing::warn!("Suite {suite_uuid} was cancelled: {reason}");
                    if let Some(token) = &self.job_token {
                        token.cancel();
                    }
                }
            }
            AgentNotification::TasksCancelled { task_uuids } => {
                // No client-side bookkeeping needed: a report against a task
                // that is gone answers 404 and the runner moves on.
                tracing::debug!(count = task_uuids.len(), "Tasks cancelled");
            }
            AgentNotification::Shutdown { graceful } => {
                tracing::warn!("Coordinator asked this agent to shut down (graceful={graceful})");
                if graceful {
                    self.stop_after_current = true;
                    self.pending_suite = None;
                    // Stop waiting for more work, but let the job finish its
                    // cleanup — that is exactly what `drain_token` separates.
                    if let Some(token) = &self.drain_token {
                        token.cancel();
                    }
                } else {
                    if let Some(token) = &self.job_token {
                        token.cancel();
                    }
                    self.shutdown_token.cancel();
                }
            }
            AgentNotification::Ping { .. } => {}
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
        }
    }

    async fn send_heartbeat(&mut self) -> Result<()> {
        let req = AgentHeartbeatReq {
            state: self.state,
            assigned_suite_uuid: self.assigned_suite_uuid,
            last_notification_id: self.notification_counter,
            metrics: None,
        };
        let resp = self
            .http_client
            .post(self.api_url("agents/heartbeat").as_str())
            .bearer_auth(&self.token)
            .json(&req)
            .send()
            .await
            .map_err(error::map_reqwest_err)?;
        let resp: AgentHeartbeatResp = parse_json(resp, "heartbeat").await?;

        // Catch-up path: whatever the WebSocket did not deliver arrives here.
        for event in resp.notifications {
            self.notification_counter = self.notification_counter.max(event.id);
            self.handle_notification(event.event);
        }
        Ok(())
    }

    // ── state machine ──

    async fn advance(&mut self) -> Result<()> {
        match self.state {
            AgentState::Idle => {
                if self.stop_after_current {
                    return Ok(());
                }
                // Act only on a signal: the coordinator re-checks availability
                // on every heartbeat and re-notifies.
                let target = match self.pending_suite.take() {
                    None => return Ok(()),
                    Some(PendingSuite::Any) => None,
                    Some(PendingSuite::Specific(uuid)) => Some(uuid),
                };

                // One attempt per signal: the slot was cleared above, so
                // "nothing available" leaves us idle until the next
                // notification rather than spinning.
                let Some((suite, job, job_id)) = self.accept_suite(target).await? else {
                    return Ok(());
                };
                tracing::info!("Accepted suite {} as job {job_id}", suite.uuid);

                self.assigned_suite_uuid = Some(suite.uuid);
                let job_token = CancellationToken::new();
                self.job_token = Some(job_token.clone());
                let drain_token = CancellationToken::new();
                self.drain_token = Some(drain_token.clone());
                // `--run-once` exits after this suite, so it never holds a
                // drained one for the idle window.
                if self.run_once {
                    drain_token.cancel();
                }
                let runner = SuiteRunner {
                    http_client: self.http_client.clone(),
                    token: self.token.clone(),
                    coordinator_addr: self.coordinator_addr.clone(),
                    polling_interval: self.polling_interval,
                    cache_path: self.cache_path.join("job"),
                    job_token,
                    drain_token,
                };
                self.current_run = Some(tokio::spawn(runner.run(suite, job)));
                self.state = AgentState::Executing;
            }
            AgentState::Executing => {
                // Reap without blocking the loop, so heartbeats keep flowing.
                let finished = self
                    .current_run
                    .as_ref()
                    .map(JoinHandle::is_finished)
                    .unwrap_or(true);
                if finished {
                    if let Some(handle) = self.current_run.take() {
                        match handle.await {
                            // `complete` said more work is waiting for us.
                            Ok(true) => self.note_pending_suite(None),
                            Ok(false) => {}
                            Err(e) => tracing::error!("The suite runner task failed: {e}"),
                        }
                    }
                    self.assigned_suite_uuid = None;
                    self.job_token = None;
                    self.drain_token = None;
                    self.state = AgentState::Idle;
                    if self.run_once {
                        tracing::info!("--run-once: one suite done, stopping");
                        self.stop_after_current = true;
                    }
                    tracing::info!("Agent is idle again");
                }
            }
            // Coordinator-owned: the agent only ever reports Idle/Executing,
            // its other phases being bracketed by the start/cleanup calls.
            AgentState::Provisioning | AgentState::Cleaning | AgentState::Offline => {}
        }
        Ok(())
    }

    /// Ask for a suite and claim it in one call, naming the one we were
    /// notified about if there was one. `None` means nothing was available.
    ///
    /// The suite handed back need not be the one asked for: a stale hint is
    /// answered with the best available instead.
    async fn accept_suite(
        &self,
        suite_uuid: Option<Uuid>,
    ) -> Result<Option<(TaskSuiteSpec, i64, i32)>> {
        let resp = self
            .http_client
            .post(self.api_url("agents/suite").as_str())
            .bearer_auth(&self.token)
            .json(&AcceptSuiteReq { suite_uuid })
            .send()
            .await
            .map_err(error::map_reqwest_err)?;
        let resp: AcceptSuiteResp = parse_json(resp, "accept suite").await?;
        if !resp.accepted {
            tracing::info!("No suite accepted: {}", resp.reason.unwrap_or_default());
            return Ok(None);
        }
        let Some(suite) = resp.suite else {
            return Err(error::Error::Custom(
                "the coordinator accepted a suite but sent no spec".to_string(),
            ));
        };
        Ok(resp.job.zip(resp.job_id).map(|(job, id)| (suite, job, id)))
    }
}

/// Drives one accepted suite from provision through to a terminal job state.
#[derive(Clone)]
struct SuiteRunner {
    http_client: reqwest::Client,
    token: String,
    coordinator_addr: Url,
    polling_interval: Duration,
    /// This job's working subtree
    cache_path: PathBuf,
    /// Cancelled when the suite is cancelled, preempted, or the agent stops.
    /// Everything the job runs (tasks, hooks, cleanup) runs under it.
    job_token: CancellationToken,
    /// Cancelled to stop waiting without stopping the wind-down: a graceful
    /// shutdown releases a held job, but its cleanup hook still has to run, and
    /// that runs under `job_token`.
    drain_token: CancellationToken,
}

impl SuiteRunner {
    fn api_url(&self, path: &str) -> Url {
        let mut url = self.coordinator_addr.clone();
        url.set_path(path);
        url
    }

    /// Every phase logs and continues on error rather than bailing: only the
    /// agent can walk the job to a terminal state and release itself.
    ///
    /// Returns whether `complete` said this agent has more work waiting.
    async fn run(self, suite: TaskSuiteSpec, job: i64) -> bool {
        let suite_uuid = suite.uuid;
        let mut failure: Option<JobFailureReason> = None;

        let provisioned = match self.prepare_workspace().await {
            Ok(()) => self.run_hook(job, &suite, HookType::Provision).await,
            Err(e) => Err(e),
        };
        if let Err(e) = provisioned {
            tracing::error!("Provisioning failed for suite {suite_uuid}: {e}");
            failure = Some(JobFailureReason {
                kind: JobFailureKind::ProvisionFailed,
                message: e.to_string(),
            });
        }

        if let Err(e) = self
            .post_empty("agents/job/start", &StartJobReq { job })
            .await
        {
            tracing::error!("Failed to start suite {suite_uuid}: {e}");
        }

        let background_token = CancellationToken::new();
        let mut background = None;

        if failure.is_none() {
            background = self.spawn_background_hook(job, &suite, background_token.clone());
            if let Err(e) = self.execute_tasks(suite_uuid, job).await {
                tracing::error!("Task execution failed for suite {suite_uuid}: {e}");
                failure = Some(JobFailureReason {
                    kind: JobFailureKind::ExecutionError,
                    message: e.to_string(),
                });
            }
        }

        if let Some(handle) = background {
            // Finishing before we ask it to is the failure mode a background
            // hook has: it was supposed to outlive the tasks it serves.
            let exited_early = handle.is_finished();
            background_token.cancel();
            if let Err(e) = handle.await {
                tracing::error!("The background hook task failed for suite {suite_uuid}: {e}");
            }
            if exited_early {
                failure.get_or_insert(JobFailureReason {
                    kind: JobFailureKind::BackgroundExited,
                    message: "the background hook exited before the tasks drained".to_string(),
                });
            } else {
                self.record_stopped_hook(job, suite_uuid, HookType::Background)
                    .await;
            }
        }

        if let Err(e) = self
            .post_empty("agents/job/cleanup", &EnterCleanupReq { job })
            .await
        {
            tracing::error!("Failed to enter cleanup for suite {suite_uuid}: {e}");
        }

        if let Err(e) = self.run_hook(job, &suite, HookType::Cleanup).await {
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
        let next_available = match self.report_complete(job, outcome).await {
            Ok(next_available) => {
                tracing::info!(
                    "Finished suite {suite_uuid} (job handle {job}); more work waiting: {next_available}"
                );
                next_available
            }
            // A `complete` that never landed says nothing about what is next.
            Err(e) => {
                tracing::error!("Failed to complete suite {suite_uuid}: {e}");
                false
            }
        };

        // Everything worth keeping has been uploaded by now
        let _ = tokio::fs::remove_dir_all(&self.cache_path).await;
        next_available
    }

    /// The directory the provision hook, the tasks and the cleanup hook all see
    /// as `MITO_SUITE_SHARED`.
    fn shared_path(&self) -> PathBuf {
        self.cache_path.join("share")
    }

    /// Create a job's file tree
    async fn prepare_workspace(&self) -> Result<()> {
        let _ = tokio::fs::remove_dir_all(&self.cache_path).await;
        tokio::fs::create_dir_all(self.shared_path()).await?;
        Ok(())
    }

    /// Claim and run the suite's tasks, one slot at a time.
    ///
    /// TODO: currently we run one task at a time. need to support task parallel
    async fn execute_tasks(&self, suite_uuid: Uuid, job: i64) -> Result<()> {
        let slot = self.cache_path.join("task-0");
        let mut holding = false;
        loop {
            if self.job_token.is_cancelled() {
                tracing::info!("Suite {suite_uuid} cancelled; stopping the task loop");
                return Ok(());
            }

            let resp = self.fetch_tasks(suite_uuid, TASK_SLOTS as u32).await?;
            if resp.tasks.is_empty() {
                if !resp.hold_job_open {
                    tracing::info!("Suite {suite_uuid} is idle and has no more tasks");
                    return Ok(());
                }
                if !holding {
                    tracing::info!(
                        "Suite {suite_uuid} is drained but still open; holding job {job} open for more work"
                    );
                    holding = true;
                }
                // The coordinator only notifies *idle* agents of a submission,
                // and we are Executing, so the poll is what finds late work.
                tokio::select! {
                    _ = self.job_token.cancelled() => return Ok(()),
                    // Stops the wait, not the wind-down: cleanup still runs.
                    _ = self.drain_token.cancelled() => {
                        tracing::info!("Asked to wind down; releasing job {job}");
                        return Ok(());
                    }
                    _ = tokio::time::sleep(HOLD_POLL_INTERVAL) => {}
                }
                continue;
            }
            if holding {
                tracing::info!("Suite {suite_uuid} has work again; job {job} resumes");
                holding = false;
            }

            for task in resp.tasks {
                if self.job_token.is_cancelled() {
                    return Ok(());
                }
                let uuid = task.uuid;
                if let Err(e) = self.run_task(job, task, &slot).await {
                    // One task must not take the job down: the coordinator
                    // reclaims anything left uncommitted.
                    tracing::error!("Failed to run task {uuid}: {e}");
                }
            }
        }
    }

    /// Run one task, reporting it through the agent's task endpoint.
    async fn run_task(&self, job: i64, task: WorkerTaskResp, slot: &std::path::Path) -> Result<()> {
        tracing::info!("Running task {} (args {:?})", task.uuid, task.spec.args);
        reset_workspace(slot).await?;
        let mut executor = Executor {
            cancel_token: self.job_token.clone(),
            polling_interval: self.polling_interval,
            cache_path: slot.to_path_buf(),
            http_client: self.http_client.clone(),
            client: Box::new(AgentTaskClient {
                http: self.connection(),
                job,
                task_id: task.id,
                task_uuid: task.uuid,
                upstream_task_uuid: task.upstream_task_uuid,
                shared_path: self.shared_path(),
            }),
        };
        execute(&mut executor, task.spec, task.exec_options.as_ref()).await
    }

    /// Run a suite hook, reporting it through the hook endpoint. An unconfigured
    /// hook is not a failed one — it just leaves no record.
    async fn run_hook(&self, job: i64, suite: &TaskSuiteSpec, hook_type: HookType) -> Result<()> {
        let Some(spec) = hook_spec(suite, hook_type) else {
            tracing::debug!("No {hook_type} hook configured for suite {}", suite.uuid);
            return Ok(());
        };
        self.run_hook_spec(job, suite, hook_type, spec, self.job_token.clone())
            .await
    }

    async fn run_hook_spec(
        &self,
        job: i64,
        suite: &TaskSuiteSpec,
        hook_type: HookType,
        spec: ExecSpec,
        cancel_token: CancellationToken,
    ) -> Result<()> {
        tracing::info!("Running the {hook_type} hook of suite {}", suite.uuid);
        let dir = self.cache_path.join(format!("hook-{hook_type}"));
        reset_workspace(&dir).await?;
        let outcome = HookOutcome::default();
        let mut executor = Executor {
            cancel_token,
            polling_interval: self.polling_interval,
            cache_path: dir,
            http_client: self.http_client.clone(),
            client: Box::new(AgentHookClient {
                http: self.connection(),
                job,
                hook_type,
                suite_uuid: suite.uuid,
                outcome: outcome.clone(),
                shared_path: self.shared_path(),
            }),
        };
        execute(&mut executor, spec, None).await?;
        match outcome.exit_status() {
            Some(0) | None => Ok(()),
            Some(status) => Err(error::Error::Custom(format!(
                "the {hook_type} hook exited with status {status}"
            ))),
        }
    }

    /// Start the background hook, if the suite has one. Not awaited: it runs for
    /// as long as the tasks do.
    fn spawn_background_hook(
        &self,
        job: i64,
        suite: &TaskSuiteSpec,
        cancel_token: CancellationToken,
    ) -> Option<JoinHandle<()>> {
        let spec = hook_spec(suite, HookType::Background)?;
        let runner = self.clone();
        let suite = suite.clone();
        Some(tokio::spawn(async move {
            if let Err(e) = runner
                .run_hook_spec(job, &suite, HookType::Background, spec, cancel_token)
                .await
            {
                tracing::error!("Background hook failed for suite {}: {e}", suite.uuid);
            }
        }))
    }

    /// Record a hook that ended by our own hand rather than by exiting — the
    /// background hook, once the tasks it served have drained. The execution
    /// core reports nothing on a cancellation, so without this the hook would
    /// leave no row at all.
    async fn record_stopped_hook(&self, job: i64, suite_uuid: Uuid, hook_type: HookType) {
        let mut client = AgentHookClient {
            http: self.connection(),
            job,
            hook_type,
            suite_uuid,
            outcome: HookOutcome::default(),
            shared_path: self.shared_path(),
        };
        let result = TaskResultSpec {
            exit_status: 0,
            msg: None,
        };
        if let Err(e) = client.report_commit(result).await {
            tracing::error!("Failed to record the stopped {hook_type} hook: {e}");
        }
    }

    /// The connection context every per-unit client is built from.
    fn connection(&self) -> AgentConnection {
        AgentConnection {
            http_client: self.http_client.clone(),
            token: self.token.clone(),
            coordinator_addr: self.coordinator_addr.clone(),
            polling_interval: self.polling_interval,
            job_token: self.job_token.clone(),
        }
    }

    async fn fetch_tasks(&self, suite_uuid: Uuid, max_count: u32) -> Result<FetchTasksResp> {
        let resp = self
            .http_client
            .post(self.api_url("agents/tasks/fetch").as_str())
            .bearer_auth(&self.token)
            .json(&FetchTasksReq {
                suite_uuid,
                max_count,
            })
            .send()
            .await
            .map_err(error::map_reqwest_err)?;
        // The suite going terminal under us is an ordinary end of run, not a
        // failure: stop the loop and let cleanup proceed.
        if resp.status() == StatusCode::CONFLICT {
            tracing::info!("Suite {suite_uuid} stopped handing out tasks");
            self.job_token.cancel();
            return Ok(FetchTasksResp {
                tasks: Vec::new(),
                hold_job_open: false,
            });
        }
        parse_json(resp, "fetch tasks").await
    }

    async fn report_complete(&self, job: i64, outcome: SuiteJobOutcome) -> Result<bool> {
        let resp = self
            .http_client
            .post(self.api_url("agents/job/complete").as_str())
            .bearer_auth(&self.token)
            .json(&CompleteJobReq { job, outcome })
            .send()
            .await
            .map_err(error::map_reqwest_err)?;
        let resp: CompleteJobResp = parse_json(resp, "complete job").await?;
        Ok(resp.next_suite_available)
    }

    /// POST a request whose success carries no body.
    async fn post_empty<T: serde::Serialize>(&self, path: &str, req: &T) -> Result<()> {
        let resp = self
            .http_client
            .post(self.api_url(path).as_str())
            .bearer_auth(&self.token)
            .json(req)
            .send()
            .await
            .map_err(error::map_reqwest_err)?;
        if resp.status().is_success() {
            return Ok(());
        }
        Err(error::Error::Custom(format!(
            "{path} failed: {}",
            error::get_error_from_resp(resp).await
        )))
    }
}

/// The hook's `ExecSpec` from the suite definition, if it has one.
fn hook_spec(suite: &TaskSuiteSpec, hook_type: HookType) -> Option<ExecSpec> {
    let hooks = suite.exec_hooks.as_ref()?;
    match hook_type {
        HookType::Provision => hooks.provision.clone(),
        HookType::Cleanup => hooks.cleanup.clone(),
        HookType::Background => hooks.background.clone(),
    }
}

/// The result a hook committed, handed back out of the execution core so the
/// runner can turn a non-zero exit into a job failure. Empty when the hook never
/// got as far as committing (it was cancelled, or its inputs were unavailable).
#[derive(Clone, Default)]
struct HookOutcome(Arc<Mutex<Option<TaskResultSpec>>>);

impl HookOutcome {
    fn record(&self, result: &TaskResultSpec) {
        if let Ok(mut slot) = self.0.lock() {
            *slot = Some(result.clone());
        }
    }

    fn exit_status(&self) -> Option<i32> {
        self.0.lock().ok()?.as_ref().map(|r| r.exit_status)
    }
}

// ── the agent's half of the execution seam ──

/// What every agent-side [`ExecClient`] needs to reach the coordinator. Cloned
/// per unit; the clones are all cheap handles.
#[derive(Clone)]
struct AgentConnection {
    http_client: reqwest::Client,
    token: String,
    coordinator_addr: Url,
    polling_interval: Duration,
    /// The job's token. Cancelling it stops every unit of this job at once,
    /// which is how a 409 ("this job is closed") propagates.
    job_token: CancellationToken,
}

impl AgentConnection {
    fn api_url(&self, path: &str) -> Url {
        let mut url = self.coordinator_addr.clone();
        url.set_path(path);
        url
    }

    fn get(&self, path: &str) -> reqwest::RequestBuilder {
        self.http_client
            .get(self.api_url(path).as_str())
            .bearer_auth(&self.token)
    }

    /// POST a report, retrying connection failures until the job is cancelled.
    /// `None` means the coordinator answered something that ends the reporting:
    /// 409 (the job is closed — which also stops the job) or 404 (it is gone).
    async fn post_report<Req: serde::Serialize, Resp: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        req: &Req,
        what: &str,
    ) -> Result<Option<Resp>> {
        let url = self.api_url(path);
        loop {
            let resp = self
                .http_client
                .post(url.as_str())
                .bearer_auth(&self.token)
                .json(req)
                .send()
                .await;
            let resp = match resp {
                Ok(resp) => resp,
                Err(e) if e.is_connect() && e.is_request() => {
                    tracing::warn!(
                        "{what} failed to connect ({e}); retrying in {:?}",
                        self.polling_interval
                    );
                    tokio::select! {
                        _ = self.job_token.cancelled() => return Ok(None),
                        _ = tokio::time::sleep(self.polling_interval) => {}
                    }
                    continue;
                }
                Err(e) => return Err(error::RequestError::from(e).into()),
            };

            if resp.status() == StatusCode::CONFLICT {
                tracing::info!("The job is closed; stopping this run");
                self.job_token.cancel();
                return Ok(None);
            }
            if resp.status() == StatusCode::NOT_FOUND {
                tracing::debug!("{what}: the target is gone; skipping the report");
                return Ok(None);
            }
            return parse_json(resp, what).await.map(Some);
        }
    }
}

/// A task the agent runs on a suite's behalf. Same protocol as the worker's,
/// addressed to `/agents/tasks/report` with the job handle attached.
struct AgentTaskClient {
    http: AgentConnection,
    job: i64,
    task_id: i64,
    task_uuid: Uuid,
    upstream_task_uuid: Option<Uuid>,
    /// The job's `share/`, exported as `MITO_SUITE_SHARED`.
    shared_path: PathBuf,
}

impl AgentTaskClient {
    async fn report(&self, op: ReportTaskOp) -> Result<Option<ReportTaskResp>> {
        self.http
            .post_report(
                "agents/tasks/report",
                &ReportAgentTaskReq {
                    job: self.job,
                    id: self.task_id,
                    op,
                },
                "report task",
            )
            .await
    }
}

#[async_trait::async_trait]
impl ExecClient for AgentTaskClient {
    fn describe(&self) -> String {
        format!("task {}", self.task_uuid)
    }

    fn exec_env(&self) -> Vec<(&'static str, String)> {
        let mut env = vec![
            ("MITO_TASK_UUID", self.task_uuid.to_string()),
            (
                "MITO_SUITE_SHARED",
                self.shared_path.to_string_lossy().into_owned(),
            ),
        ];
        if let Some(uuid) = self.upstream_task_uuid {
            env.push(("MITO_UPSTREAM_TASK_UUID", uuid.to_string()));
        }
        env
    }

    fn supports_child_tasks(&self) -> bool {
        true
    }

    async fn report_finish(&mut self, finished: bool, _result: &TaskResultSpec) -> Result<()> {
        let op = if finished {
            ReportTaskOp::Finish
        } else {
            ReportTaskOp::Cancel
        };
        self.report(op).await.map(|_| ())
    }

    async fn request_upload(
        &mut self,
        content_type: ArtifactContentType,
        content_length: u64,
    ) -> Result<UploadTarget> {
        let resp = self
            .report(ReportTaskOp::Upload {
                content_type,
                content_length,
            })
            .await?;
        Ok(match resp.and_then(|resp| resp.url) {
            Some(url) => UploadTarget::Url(url),
            None => UploadTarget::Skip,
        })
    }

    async fn report_commit(&mut self, result: TaskResultSpec) -> Result<()> {
        self.report(ReportTaskOp::Commit(result)).await.map(|_| ())
    }

    async fn submit_child_task(&mut self, req: SubmitTaskReq) -> Result<()> {
        self.report(ReportTaskOp::Submit(Box::new(req)))
            .await
            .map(|_| ())
    }

    fn artifact_download_req(
        &self,
        uuid: Uuid,
        content_type: ArtifactContentType,
    ) -> reqwest::RequestBuilder {
        self.http.get(&artifact_path(uuid, content_type))
    }

    fn attachment_download_req(&self, key: &str) -> reqwest::RequestBuilder {
        let uuid = self.task_uuid;
        self.http
            .get(&format!("agents/tasks/{uuid}/attachments/{key}"))
    }

    /// No redis on the agent side yet, so an agent task's fine-grained states go
    /// unpublished.
    async fn announce_state(&mut self, _state: TaskExecState, _ex: Option<u64>) {}

    fn can_watch(&self) -> bool {
        true
    }

    /// Poll-only, which resolves the coarse milestones a `watch` asks for (has
    /// the other task committed?) but not the intra-execution states.
    async fn watch(&mut self, uuid: &Uuid, target: TaskExecState) {
        tracing::debug!("Watch task: {} -> {:?}", uuid, target);
        loop {
            let resp = self.http.get(&format!("agents/tasks/{uuid}")).send().await;
            match resp {
                Ok(resp) if resp.status().is_success() => {
                    match resp.json::<TaskQueryResp>().await {
                        Ok(task) => {
                            if task.info.state.is_reach(&target, task.info.result) {
                                return;
                            }
                        }
                        Err(e) => tracing::warn!("Unreadable watched task {uuid}: {e}"),
                    }
                }
                Ok(resp) => tracing::warn!(
                    "Watching task {uuid} failed: {}",
                    error::get_error_from_resp(resp).await
                ),
                Err(e) => tracing::warn!("Watching task {uuid} failed: {e}"),
            }
            tokio::select! {
                _ = self.http.job_token.cancelled() => return,
                _ = tokio::time::sleep(WATCH_POLL_INTERVAL) => {}
            }
        }
    }
}

/// A suite hook. Keyed by `{job, hook_type}` rather than by a task, and reported
/// to `/agents/job/hook`, whose `Result` op records the outcome and mints the
/// uuid its artifacts hang off — hence the outcome is reported before the
/// uploads.
struct AgentHookClient {
    http: AgentConnection,
    job: i64,
    hook_type: HookType,
    suite_uuid: Uuid,
    outcome: HookOutcome,
    /// The job's `share/`, exported as `MITO_SUITE_SHARED`.
    shared_path: PathBuf,
}

impl AgentHookClient {
    async fn report(&self, op: HookReportOp) -> Result<Option<HookReportResp>> {
        self.http
            .post_report(
                "agents/job/hook",
                &HookReportReq {
                    job: self.job,
                    hook_type: self.hook_type,
                    op,
                },
                "report hook",
            )
            .await
    }
}

#[async_trait::async_trait]
impl ExecClient for AgentHookClient {
    fn describe(&self) -> String {
        format!("the {} hook of job {}", self.hook_type, self.job)
    }

    fn exec_env(&self) -> Vec<(&'static str, String)> {
        vec![
            ("MITO_HOOK_TYPE", self.hook_type.to_string()),
            ("MITO_SUITE_UUID", self.suite_uuid.to_string()),
            (
                "MITO_SUITE_SHARED",
                self.shared_path.to_string_lossy().into_owned(),
            ),
        ]
    }

    /// The hook report endpoint has no submit operation, so `MITO_NEW_TASK` is
    /// not exported to a hook.
    fn supports_child_tasks(&self) -> bool {
        false
    }

    /// Writes the hook row. Its uuid is what the uploads that follow are keyed
    /// by, so this must land before them.
    async fn report_finish(&mut self, _finished: bool, result: &TaskResultSpec) -> Result<()> {
        self.report(HookReportOp::Result(result.clone()))
            .await
            .map(|_| ())
    }

    async fn request_upload(
        &mut self,
        content_type: ArtifactContentType,
        content_length: u64,
    ) -> Result<UploadTarget> {
        let resp = self
            .report(HookReportOp::Upload {
                content_type,
                content_length,
            })
            .await?;
        Ok(match resp.and_then(|resp| resp.url) {
            Some(url) => UploadTarget::Url(url),
            None => UploadTarget::Skip,
        })
    }

    /// Re-reports the row with the final message. The endpoint upserts on
    /// `{job, hook_type}` and never rewrites the uuid, so the artifacts uploaded
    /// in between stay attached.
    async fn report_commit(&mut self, result: TaskResultSpec) -> Result<()> {
        self.outcome.record(&result);
        self.report(HookReportOp::Result(result)).await.map(|_| ())
    }

    async fn submit_child_task(&mut self, _req: SubmitTaskReq) -> Result<()> {
        Err(error::Error::Custom(
            "a suite hook cannot submit a task".to_string(),
        ))
    }

    fn artifact_download_req(
        &self,
        uuid: Uuid,
        content_type: ArtifactContentType,
    ) -> reqwest::RequestBuilder {
        self.http.get(&artifact_path(uuid, content_type))
    }

    /// Through the suite, not a task: a hook has no task uuid to resolve the
    /// owning group by.
    fn attachment_download_req(&self, key: &str) -> reqwest::RequestBuilder {
        let uuid = self.suite_uuid;
        self.http
            .get(&format!("agents/suites/{uuid}/attachments/{key}"))
    }

    async fn announce_state(&mut self, _state: TaskExecState, _ex: Option<u64>) {}
}

/// The artifact download path both agent clients use. Keyed by the artifact's
/// owning uuid, which the service resolves without caring whose it is.
fn artifact_path(uuid: Uuid, content_type: ArtifactContentType) -> String {
    let content_type = serde_json::to_value(content_type)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "result".to_string());
    format!("agents/tasks/{uuid}/artifacts/{content_type}")
}

/// One WebSocket session: read notifications, hand each to the main loop, then
/// acknowledge it. Returns when the socket closes or shutdown is signalled.
async fn websocket_session(
    ws_url: &str,
    token: &str,
    notification_tx: &mpsc::Sender<WsNotificationEvent>,
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
                if notification_tx.send(event).await.is_err() {
                    return Ok(());
                }
                // Acknowledge only once the main loop has taken it, so an event
                // never leaves the coordinator's replay buffer before we own it.
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

/// Decode a successful JSON response, turning a non-2xx into a descriptive error.
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
