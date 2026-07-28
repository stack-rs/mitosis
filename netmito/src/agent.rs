//! The agent: an orchestrator process that claims task suites from the
//! coordinator and runs them.
//!
//! ```text
//!  register ─▶ ws connect ─┐
//!                          ├─▶ main loop ─▶ (idle + work available) ─▶ fetch ─▶ accept
//!  heartbeat every N ──────┘                                                     │
//!                                                                                ▼
//!                        complete ◀─ cleanup hook ◀─ cleanup ◀─ tasks ◀─ start ◀─ provision hook
//! ```
//!
//! The main loop only ever services heartbeats and notifications; a claimed
//! suite runs in a spawned [`SuiteRunner`] so neither starves the other.
//!

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
use crate::entity::state::AgentState;
use crate::error::{self, Result};
use crate::schema::*;
use crate::service::auth::cred::get_user_credential;

/// The payload every faked task "produces".
const FAKE_RESULT: &[u8] = b"fake result";

/// How many consecutive empty task fetches end a suite run. The suite's
/// `incomplete_tasks` can be non-zero while every remaining task is held by
/// another agent, so a single empty fetch is not enough to conclude there is no
/// work left for us.
const EMPTY_FETCHES_BEFORE_DONE: u32 = 3;

/// Delay between empty task fetches.
const EMPTY_FETCH_BACKOFF: Duration = Duration::from_millis(500);

/// How long to wait before retrying a dropped WebSocket.
const WS_RECONNECT_DELAY: Duration = Duration::from_secs(5);

pub struct MitoAgent;

/// What the agent should fetch next, recorded by a notification and consumed in
/// the idle branch of the main loop. Distinct from "nothing pending" so an idle
/// agent never polls unless the coordinator actually signalled work.
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
    http_client: reqwest::Client,
    /// Cancels the running suite (notification-driven); `None` while idle.
    job_token: Option<CancellationToken>,
    current_run: Option<JoinHandle<()>>,
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
            http_client,
            job_token: None,
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
                // Only act on an actual signal. The coordinator re-checks
                // availability on every heartbeat and re-notifies, so there is
                // nothing to gain from polling on our own.
                let target = match self.pending_suite.take() {
                    None => return Ok(()),
                    Some(PendingSuite::Any) => None,
                    Some(PendingSuite::Specific(uuid)) => Some(uuid),
                };

                // One attempt per signal: the slot is already cleared above, so
                // a stale hint or a lost race leaves us idle until the next
                // notification rather than spinning.
                let Some(suite) = self.fetch_suite(target).await? else {
                    return Ok(());
                };
                let Some((job, job_id)) = self.accept_suite(suite.uuid).await? else {
                    return Ok(());
                };
                tracing::info!("Accepted suite {} as job {job_id}", suite.uuid);

                self.assigned_suite_uuid = Some(suite.uuid);
                let job_token = CancellationToken::new();
                self.job_token = Some(job_token.clone());
                let runner = SuiteRunner {
                    http_client: self.http_client.clone(),
                    token: self.token.clone(),
                    coordinator_addr: self.coordinator_addr.clone(),
                    polling_interval: self.polling_interval,
                    job_token,
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
                        if let Err(e) = handle.await {
                            tracing::error!("The suite runner task failed: {e}");
                        }
                    }
                    self.assigned_suite_uuid = None;
                    self.job_token = None;
                    self.state = AgentState::Idle;
                    if self.run_once {
                        tracing::info!("--run-once: one suite done, stopping");
                        self.stop_after_current = true;
                    }
                    tracing::info!("Agent is idle again");
                }
            }
            // The coordinator owns these; the agent only ever reports
            // Idle/Executing, since its provisioning and cleanup phases are
            // bracketed by the start/cleanup calls rather than self-declared.
            AgentState::Provisioning | AgentState::Cleaning | AgentState::Offline => {}
        }
        Ok(())
    }

    async fn fetch_suite(&self, suite_uuid: Option<Uuid>) -> Result<Option<TaskSuiteSpec>> {
        let mut url = self.api_url("agents/suite");
        if let Some(uuid) = suite_uuid {
            url.set_query(Some(&format!("suite_uuid={uuid}")));
        }
        let resp = self
            .http_client
            .get(url.as_str())
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(error::map_reqwest_err)?;
        let resp: FetchSuiteResp = parse_json(resp, "fetch suite").await?;
        Ok(resp.suite)
    }

    /// Claim a suite. `None` means the coordinator declined — a normal race, not
    /// an error.
    async fn accept_suite(&self, suite_uuid: Uuid) -> Result<Option<(i64, i32)>> {
        let resp = self
            .http_client
            .post(self.api_url("agents/suite/accept").as_str())
            .bearer_auth(&self.token)
            .json(&AcceptSuiteReq { suite_uuid })
            .send()
            .await
            .map_err(error::map_reqwest_err)?;
        let resp: AcceptSuiteResp = parse_json(resp, "accept suite").await?;
        if !resp.accepted {
            tracing::info!(
                "Suite {suite_uuid} not accepted: {}",
                resp.reason.unwrap_or_default()
            );
            return Ok(None);
        }
        Ok(resp.job.zip(resp.job_id))
    }
}

/// Drives one accepted suite from provision through to a terminal job state.
struct SuiteRunner {
    http_client: reqwest::Client,
    token: String,
    coordinator_addr: Url,
    polling_interval: Duration,
    /// Cancelled when the suite is cancelled, preempted, or the agent stops.
    job_token: CancellationToken,
}

impl SuiteRunner {
    fn api_url(&self, path: &str) -> Url {
        let mut url = self.coordinator_addr.clone();
        url.set_path(path);
        url
    }

    /// Every phase logs and continues on error rather than bailing, so a partial
    /// run still walks the job toward a terminal state instead of stranding it
    /// for the heartbeat timeout to clean up.
    async fn run(self, suite: TaskSuiteSpec, job: i64) {
        let suite_uuid = suite.uuid;
        let mut failure: Option<JobFailureReason> = None;

        // Provision. Reported even though it is faked, so the job's hook record
        // exists and a real hook drops straight in.
        if let Err(e) = self
            .report_fake_hook(job, HookType::Provision, &suite)
            .await
        {
            tracing::error!("Provision hook report failed for suite {suite_uuid}: {e}");
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

        if let Err(e) = self.execute_tasks(suite_uuid, job).await {
            tracing::error!("Task execution failed for suite {suite_uuid}: {e}");
            failure.get_or_insert(JobFailureReason {
                kind: JobFailureKind::ExecutionError,
                message: e.to_string(),
            });
        }

        if let Err(e) = self
            .post_empty("agents/job/cleanup", &EnterCleanupReq { job })
            .await
        {
            tracing::error!("Failed to enter cleanup for suite {suite_uuid}: {e}");
        }

        if let Err(e) = self.report_fake_hook(job, HookType::Cleanup, &suite).await {
            tracing::error!("Cleanup hook report failed for suite {suite_uuid}: {e}");
            failure.get_or_insert(JobFailureReason {
                kind: JobFailureKind::CleanupFailed,
                message: e.to_string(),
            });
        }

        let outcome = match failure {
            None => SuiteJobOutcome::Completed,
            Some(reason) => SuiteJobOutcome::Failed { reason },
        };
        match self.report_complete(job, outcome).await {
            Ok(next_available) => tracing::info!(
                "Finished suite {suite_uuid} (job handle {job}); more work waiting: {next_available}"
            ),
            Err(e) => tracing::error!("Failed to complete suite {suite_uuid}: {e}"),
        }
    }

    /// Claim and run the suite's tasks one at a time.
    ///
    /// Sequential on purpose: the suite's `FixedWorkers` count is carried on the
    /// wire but not yet used to parallelize. When it is, this is the loop that
    /// grows into a worker pool — the per-task protocol below does not change.
    async fn execute_tasks(&self, suite_uuid: Uuid, job: i64) -> Result<()> {
        let mut empty_fetches = 0u32;
        loop {
            if self.job_token.is_cancelled() {
                tracing::info!("Suite {suite_uuid} cancelled; stopping the task loop");
                return Ok(());
            }

            let tasks = self.fetch_tasks(suite_uuid, 1).await?;
            if tasks.is_empty() {
                empty_fetches += 1;
                if empty_fetches >= EMPTY_FETCHES_BEFORE_DONE {
                    tracing::info!("No more tasks in suite {suite_uuid}");
                    return Ok(());
                }
                tokio::select! {
                    _ = self.job_token.cancelled() => return Ok(()),
                    _ = tokio::time::sleep(EMPTY_FETCH_BACKOFF) => {}
                }
                continue;
            }
            empty_fetches = 0;

            for task in tasks {
                if self.job_token.is_cancelled() {
                    return Ok(());
                }
                if let Err(e) = self.fake_run_task(job, &task).await {
                    tracing::error!("Failed to run task {}: {e}", task.uuid);
                }
            }
        }
    }

    /// Pretend to run one task, then report it exactly as a real run would:
    /// `Finish`, upload the `result` artifact, `Commit` with a zero exit status.
    async fn fake_run_task(&self, job: i64, task: &WorkerTaskResp) -> Result<()> {
        tracing::info!(
            "Faking execution of task {} (args {:?})",
            task.uuid,
            task.spec.args
        );

        self.report_task(job, task.id, ReportTaskOp::Finish).await?;

        let url = self
            .report_task(
                job,
                task.id,
                ReportTaskOp::Upload {
                    content_type: ArtifactContentType::Result,
                    content_length: FAKE_RESULT.len() as u64,
                },
            )
            .await?;
        match url {
            Some(url) => self.put_bytes(&url, FAKE_RESULT.to_vec()).await?,
            None => tracing::warn!(
                "No upload URL returned for task {}; skipping its artifact",
                task.uuid
            ),
        }

        self.report_task(
            job,
            task.id,
            ReportTaskOp::Commit(TaskResultSpec {
                exit_status: 0,
                msg: None,
            }),
        )
        .await?;

        tracing::info!("Task {} reported complete", task.uuid);
        Ok(())
    }

    async fn fetch_tasks(&self, suite_uuid: Uuid, max_count: u32) -> Result<Vec<WorkerTaskResp>> {
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
            return Ok(Vec::new());
        }
        let resp: FetchTasksResp = parse_json(resp, "fetch tasks").await?;
        Ok(resp.tasks)
    }

    /// Report one task operation. A closed job (409) cancels the run; a task
    /// that no longer exists (404) is skipped.
    async fn report_task(
        &self,
        job: i64,
        task_id: i64,
        op: ReportTaskOp,
    ) -> Result<Option<String>> {
        let req = ReportAgentTaskReq {
            job,
            id: task_id,
            op,
        };
        let url = self.api_url("agents/tasks/report");
        loop {
            let resp = self
                .http_client
                .post(url.as_str())
                .bearer_auth(&self.token)
                .json(&req)
                .send()
                .await;
            let resp = match resp {
                Ok(resp) => resp,
                Err(e) if e.is_connect() && e.is_request() => {
                    tracing::warn!(
                        "Task report failed to connect ({e}); retrying in {:?}",
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
                tracing::info!("Job {job} is closed; stopping this run");
                self.job_token.cancel();
                return Ok(None);
            }
            if resp.status() == StatusCode::NOT_FOUND {
                tracing::debug!("Task {task_id} is gone; skipping its report");
                return Ok(None);
            }
            let resp: ReportTaskResp = parse_json(resp, "report task").await?;
            return Ok(resp.url);
        }
    }

    /// Record a hook as a clean no-op and attach a stub log, so the hook
    /// artifact path is exercised end to end.
    async fn report_fake_hook(
        &self,
        job: i64,
        hook_type: HookType,
        suite: &TaskSuiteSpec,
    ) -> Result<()> {
        let configured = suite.exec_hooks.as_ref().and_then(|hooks| match hook_type {
            HookType::Provision => hooks.provision.as_ref(),
            HookType::Cleanup => hooks.cleanup.as_ref(),
            HookType::Background => hooks.background.as_ref(),
        });
        match configured {
            Some(spec) => tracing::info!("Faking the {hook_type} hook (spec {:?})", spec.args),
            None => tracing::info!("No {hook_type} hook configured; reporting a no-op"),
        }

        self.post_hook(&HookReportReq {
            job,
            hook_type,
            op: HookReportOp::Result(TaskResultSpec {
                exit_status: 0,
                msg: None,
            }),
        })
        .await?;

        let log = format!("fake {hook_type} hook log\n").into_bytes();
        let resp = self
            .post_hook(&HookReportReq {
                job,
                hook_type,
                op: HookReportOp::Upload {
                    content_type: ArtifactContentType::ExecLog,
                    content_length: log.len() as u64,
                },
            })
            .await?;
        if let Some(url) = resp.url {
            self.put_bytes(&url, log).await?;
        }
        Ok(())
    }

    async fn post_hook(&self, req: &HookReportReq) -> Result<HookReportResp> {
        let resp = self
            .http_client
            .post(self.api_url("agents/job/hook").as_str())
            .bearer_auth(&self.token)
            .json(req)
            .send()
            .await
            .map_err(error::map_reqwest_err)?;
        parse_json(resp, "report hook").await
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

    /// Upload an artifact body to a presigned URL.
    async fn put_bytes(&self, url: &str, body: Vec<u8>) -> Result<()> {
        let len = body.len();
        let resp = self
            .http_client
            .put(url)
            .header(reqwest::header::CONTENT_LENGTH, len)
            .body(body)
            .send()
            .await
            .map_err(error::map_reqwest_err)?;
        if resp.status().is_success() {
            return Ok(());
        }
        Err(error::Error::Custom(format!(
            "Artifact upload failed with status {}",
            resp.status()
        )))
    }
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
