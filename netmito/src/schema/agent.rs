//! Wire types for the agent surface: fleet management (`/agents`, user-authed)
//! and the execution loop (`/agents/...`, agent-authed), plus the coordinator →
//! agent notification events carried over `/ws/agents`.
//!
//! Ported from `../mitosis-dev` with the run→job rename applied and the fields
//! our slim `suite_agent_jobs` row does not carry dropped.

use std::collections::HashSet;

use sea_orm::FromQueryResult;
use serde::{Deserialize, Serialize};
use speedy::{Readable, Writable};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::entity::{
    content::ArtifactContentType,
    hook_tasks::HookType,
    state::{AgentState, HookExecState, SuiteJobState, TaskSuiteState},
};

use super::exec::ExecHooks;
use super::suite::WorkerSchedulePlan;
use super::task::{ReportTaskOp, TaskResultSpec, WorkerTaskResp};

// ============================================================================
// Fleet management (user-authed)
// ============================================================================

/// Request to register an agent.
///
/// Registration is an **upsert keyed by `machine_code`**: our `machines` table
/// holds the FK back to `agents` and both `machine_code` and `agent_id` are
/// unique, so one machine has exactly one agent row for its whole life. A
/// re-registering machine (agent restart) reuses that row — its tags, labels,
/// groups and metadata are refreshed and a fresh token is minted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterAgentReq {
    /// Tags for suite matching (e.g., `["gpu", "linux", "cuda:11.8"]`)
    #[serde(default)]
    pub tags: HashSet<String>,
    /// Labels for querying/filtering (e.g., `["datacenter:us-west"]`)
    #[serde(default)]
    pub labels: HashSet<String>,
    /// The group granted Admin over the agent — the role that may shut it down.
    /// The caller must be an Admin of it. Defaults to the registering user's
    /// personal group, the one group every user is guaranteed to administer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admin_group: Option<String>,
    /// Groups to associate with (the agent gets the Write role in each)
    #[serde(default)]
    pub groups: HashSet<String>,
    /// Optional token lifetime. When unset the token never expires.
    #[serde(default, with = "humantime_serde")]
    pub lifetime: Option<std::time::Duration>,
    /// Stable identifier for the machine running this agent. The agent client
    /// resolves one from: config override → cached value → `/etc/machine-id` →
    /// a generated UUID (persisted to the cache).
    pub machine_code: String,
    /// Static metadata about the agent process (stored on the machine row).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<AgentMetadata>,
}

/// Response after registering an agent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterAgentResp {
    pub agent_uuid: Uuid,
    pub token: String,
    /// Current notification counter for this agent (start of sequence)
    pub notification_counter: u64,
    /// True if an existing agent row was reused (same `machine_code`).
    pub reused: bool,
}

/// Static metadata reported by the agent at registration time
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMetadata {
    /// Agent binary version (e.g. "0.6.8")
    pub version: String,
    /// Long version string (e.g. includes build metadata)
    pub long_version: String,
}

/// Query parameters for listing agents
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentsQueryReq {
    pub group_name: Option<String>,
    pub tags: Option<HashSet<String>>,
    pub labels: Option<HashSet<String>>,
    pub states: Option<HashSet<AgentState>>,
    pub creator_username: Option<String>,
    pub limit: Option<u64>,
    pub offset: Option<u64>,
    pub count: bool,
}

/// Information about an agent
#[derive(Debug, Clone, Serialize, Deserialize, FromQueryResult)]
pub struct AgentInfo {
    pub uuid: Uuid,
    pub creator_username: String,
    pub tags: Vec<String>,
    pub labels: Vec<String>,
    pub state: AgentState,
    pub last_heartbeat: OffsetDateTime,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assigned_suite_uuid: Option<Uuid>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    /// Machine code of the host this agent runs on (from the `machines` row).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub machine_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// Response for agent query
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentsQueryResp {
    pub count: u64,
    pub agents: Vec<AgentInfo>,
    pub group_name: String,
}

/// Query parameters for `DELETE /agents/{uuid}` selecting the shutdown mode
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentShutdownReq {
    #[serde(default)]
    pub op: AgentShutdownOp,
}

/// Shutdown operation type.
///
/// Neither variant deletes the agent row — an agent is a durable identity and
/// every FK to it is `RESTRICT`. Both mark the agent `Offline`; they differ in
/// what happens to an in-flight job.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
pub enum AgentShutdownOp {
    /// Ask the agent to stop after its current job finishes cleanly.
    #[default]
    #[serde(alias = "graceful")]
    Graceful,
    /// Stop now: in-flight jobs go `Killed` and their tasks are reclaimed.
    #[serde(alias = "force")]
    Force,
}

/// Query parameters for the stop-job endpoints selecting the stop mode
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StopAgentJobReq {
    #[serde(default)]
    pub op: StopJobOp,
}

/// How to end a job the user asked to stop. The agent survives either way and
/// goes straight back to picking a suite — stopping a job *is* how a user
/// preempts an agent onto whatever is highest-priority now.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
pub enum StopJobOp {
    /// Stop claiming tasks, let the running ones finish, then clean up. The
    /// agent walks the job to `Completed` itself.
    #[default]
    #[serde(alias = "graceful")]
    Graceful,
    /// Stop now: the job goes `Killed` and its tasks are reclaimed. The cleanup
    /// hook does not run.
    #[serde(alias = "force")]
    Force,
}

/// Response to a stop-job request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StopAgentJobResp {
    /// False when the agent had no job to stop — an ordinary answer, not an error.
    pub stopped: bool,
    /// The suite whose job was stopped, when one was.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suite_uuid: Option<Uuid>,
    /// Per-suite job number of the stopped job, when one was.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_id: Option<i32>,
}

// ============================================================================
// Execution loop (agent-authed)
// ============================================================================

/// Request for agent heartbeat
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentHeartbeatReq {
    /// Current agent state
    pub state: AgentState,
    /// Currently assigned suite UUID (if any)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assigned_suite_uuid: Option<Uuid>,
    /// The last notification ID the agent has processed
    #[serde(default)]
    pub last_notification_id: u64,
    /// Optional metrics
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics: Option<AgentMetrics>,
}

/// Metrics reported by an agent alongside its heartbeat
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMetrics {
    pub active_workers: u32,
    pub tasks_completed: u64,
    pub tasks_failed: u64,
}

/// Response for agent heartbeat: the notifications the agent has not seen yet.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentHeartbeatResp {
    pub notifications: Vec<WsNotificationEvent>,
}

/// Request body for `POST /agents/suite`
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AcceptSuiteReq {
    /// The suite the agent was notified about, if any. A preference, not a
    /// demand: one that is gone, drained or no longer this agent's to run falls
    /// back to the best available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suite_uuid: Option<Uuid>,
}

/// Full specification of a task suite handed to an agent for execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSuiteSpec {
    pub uuid: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub group_name: String,
    pub tags: Vec<String>,
    pub labels: Vec<String>,
    pub priority: i32,
    pub worker_schedule: WorkerSchedulePlan,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exec_hooks: Option<ExecHooks>,
    pub state: TaskSuiteState,
    pub total_tasks: i32,
    pub incomplete_tasks: i32,
}

/// Response to `POST /agents/suite`: the claim and everything needed to run it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcceptSuiteResp {
    /// Whether a suite was claimed. False is an ordinary answer (nothing
    /// available, or this agent is already busy), not an error.
    pub accepted: bool,
    /// What to run. Present exactly when `accepted` is true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suite: Option<TaskSuiteSpec>,
    /// Opaque job handle (`suite_agent_jobs.id`) the agent echoes on every
    /// later job-scoped call. Present only when `accepted` is true.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job: Option<i64>,
    /// Per-suite job number, for display/inspection. Present with `job`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_id: Option<i32>,
    /// Why nothing was claimed, when `accepted` is false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Request to report that provisioning finished and execution is starting
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartJobReq {
    /// Opaque job handle from `AcceptSuiteResp`
    pub job: i64,
}

/// Request to report the agent entering the cleanup phase
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnterCleanupReq {
    /// Opaque job handle from `AcceptSuiteResp`
    pub job: i64,
}

/// Request to report job completion
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompleteJobReq {
    /// Opaque job handle from `AcceptSuiteResp`
    pub job: i64,
    /// What the agent did: finished cleanly, or failed with a reason.
    pub outcome: SuiteJobOutcome,
}

/// The agent's report of how its job ended. By design the agent reports only
/// what *it* did — `Lost`/`Killed` are coordinator decisions and are never
/// agent outcomes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SuiteJobOutcome {
    /// Provision, execution, and cleanup all succeeded.
    Completed,
    /// The job failed; `reason` summarizes the failing phase and cause. Full
    /// hook output lives in the corresponding `hook_tasks` row.
    Failed { reason: JobFailureReason },
}

/// Machine-readable category for why a job terminated abnormally
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum JobFailureKind {
    /// Provision hook failed
    ProvisionFailed,
    /// Background hook exited before the job finished
    BackgroundExited,
    /// Task execution phase failed
    ExecutionError,
    /// Cleanup hook failed
    CleanupFailed,
}

/// One-line, job-level summary of abnormal termination. Our slim job row has no
/// `failure_reason` column, so this is logged by the coordinator rather than
/// stored; full hook stdout/stderr lives in `hook_tasks.result`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct JobFailureReason {
    pub kind: JobFailureKind,
    pub message: String,
}

/// Response after completing a job
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompleteJobResp {
    /// Whether another suite is available for this agent immediately
    pub next_suite_available: bool,
}

/// Request to report a suite hook execution (`POST /agents/job/hook`).
/// Append-only: accepted even on a terminal job (a cleanup hook may legitimately
/// finish after the coordinator terminated the job).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookReportReq {
    /// Opaque job handle from `AcceptSuiteResp`.
    pub job: i64,
    /// Which hook this report is for (provision / cleanup / background).
    pub hook_type: HookType,
    /// What to do: record the hook's result, or presign a log upload.
    pub op: HookReportOp,
}

/// Operation for a hook report. `Result` writes the `hook_tasks` row and must
/// precede `Upload`, which presigns an S3 PUT for that row's log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HookReportOp {
    /// Record the hook's final result (state derived from `exit_status`).
    Result(TaskResultSpec),
    /// Presign an S3 upload for a hook artifact/log; returns the URL. Requires
    /// the hook's `Result` to have been reported first.
    Upload {
        content_type: ArtifactContentType,
        content_length: u64,
    },
}

/// Response to a hook report: the hook's uuid (its artifact key), plus the
/// presigned URL for the `Upload` op.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookReportResp {
    pub hook_uuid: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// Request to claim tasks from a suite for execution.
///
/// The coordinator knows the suite's schedule and how many tasks this agent
/// is already holding, so it sizes the batch itself. An agent that asks is
/// an agent with room, and it takes what it is given.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchTasksReq {
    /// Suite UUID to fetch tasks from
    pub suite_uuid: Uuid,
}

/// Response containing the claimed tasks (empty if the suite has none ready)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchTasksResp {
    pub tasks: Vec<WorkerTaskResp>,
    /// Whether the agent should keep waiting on this job rather than wind it
    /// down. True while the suite is `Open`, which covers "drained, but not idle
    /// long enough to be sure". An empty batch with this set means "come back
    /// and ask again", so the provisioned environment stays warm.
    #[serde(default)]
    pub hold_job_open: bool,
}

/// Request to report a task result. Mirrors the worker's `ReportTaskReq{id, op}`
/// with the suite `job` handle as the only agent-specific addition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportAgentTaskReq {
    /// Opaque job handle from `AcceptSuiteResp` — the job this task belongs to.
    pub job: i64,
    /// Internal id of the task being reported (from the fetched `WorkerTaskResp`).
    pub id: i64,
    /// Operation to perform
    pub op: ReportTaskOp,
}

/// Filter for `POST /suites/{uuid}/jobs/query`.
///
/// The relationship between the fields is AND.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SuiteJobsQueryReq {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub states: Option<HashSet<SuiteJobState>>,
    /// Only jobs run by this agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_uuid: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<u64>,
    /// Return the total number of matching jobs instead of the jobs themselves.
    #[serde(default)]
    pub count: bool,
}

/// One job of a suite
#[derive(Debug, Clone, Serialize, Deserialize, FromQueryResult)]
pub struct SuiteJobInfo {
    /// Per-suite job number (the user-facing key)
    pub job_id: i32,
    pub state: SuiteJobState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_uuid: Option<Uuid>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// Response for `GET /suites/{uuid}/jobs`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuiteJobsQueryResp {
    pub count: u64,
    pub jobs: Vec<SuiteJobInfo>,
}

/// One hook execution of a job
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookTaskInfo {
    /// Also the key its artifacts are stored under in the `artifacts` table.
    pub uuid: Uuid,
    pub hook_type: HookType,
    pub state: HookExecState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<TaskResultSpec>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<OffsetDateTime>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<OffsetDateTime>,
}

/// Response for `GET /suites/{uuid}/jobs/{job_id}`: the job plus its hooks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuiteJobQueryResp {
    pub info: SuiteJobInfo,
    pub hooks: Vec<HookTaskInfo>,
}

// ============================================================================
// Coordinator → agent notifications (WebSocket, `speedy` binary frames)
// ============================================================================

/// A sequenced notification. The `id` lets an agent that reconnects (or falls
/// back to heartbeat catch-up) tell what it has already seen.
///
/// `speedy` carries it over the socket; serde is still needed because the same
/// events come back as JSON in the heartbeat response.
#[derive(Debug, Clone, Serialize, Deserialize, Readable, Writable)]
pub struct WsNotificationEvent {
    /// Monotonically increasing sequence ID, per agent
    pub id: u64,
    pub event: AgentNotification,
}

/// Lightweight push notification prompting the agent to act over HTTP.
#[derive(Debug, Clone, Serialize, Deserialize, Readable, Writable)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentNotification {
    /// Some suite this agent could run has work. Deliberately unaddressed: by
    /// the time the agent asks, the best suite for it may not be the one that
    /// prompted this, so the choice is made in `accept` — against the state
    /// then, and under the suite's lock — rather than named here.
    SuiteAvailable,

    /// Stop the current suite and pick up a higher-priority one.
    ///
    /// **Not emitted yet.** A running agent is never interrupted for now. The
    /// variant exists so the signal can be turned on later without a wire
    /// change; the agent already handles it by cancelling its job and targeting
    /// the new suite.
    PreemptSuite {
        new_suite_uuid: Uuid,
        new_priority: i32,
        current_suite_uuid: Uuid,
    },

    /// The suite the agent is running was cancelled; stop and clean up.
    SuiteCancelled { suite_uuid: Uuid, reason: String },

    /// Specific tasks were cancelled; stop executing them if in progress.
    TasksCancelled { task_uuids: Vec<Uuid> },

    /// The agent should shut down.
    Shutdown { graceful: bool },

    /// Keepalive.
    Ping { server_time: i64 },

    /// Resync the notification counter (coordinator restart / wrap-around).
    CounterSync { counter: u64, boot_id: Uuid },

    /// Stop the job the agent is running now; the agent itself stays up and
    /// picks a suite again immediately. This is manual preemption: the agent
    /// re-runs the match *after* winding down, so it lands on whatever is
    /// highest-priority then — including the same suite, which re-provisions it.
    ///
    /// `suite_uuid` is the guard. An agent that has already moved on ignores the
    /// event rather than killing the job it started in the meantime.
    ///
    /// New variants go **last**: `speedy` encodes the variant index in
    /// declaration order, so appending is compatible with older agents and
    /// inserting silently renumbers everything after it.
    StopJob { suite_uuid: Uuid, graceful: bool },

    /// The job this agent is running on `suite_uuid` has tasks waiting: fetch
    /// now instead of sitting out the rest of the hold interval. Sent only to
    /// jobs that came up short on their last fetch, and paired with a
    /// reservation that keeps the tasks for them while they come back.
    ///
    /// An agent that is not running this suite ignores it.
    TasksAvailable { suite_uuid: Uuid },
}

impl AgentNotification {
    /// Whether an identical event already waiting unacknowledged says everything
    /// this one would.
    ///
    /// True only for the "come and look" nudges, which carry no state of their
    /// own: the agent re-reads the real thing over HTTP, so a second copy buys
    /// nothing and a burst of submissions must not push the buffer's older,
    /// stateful events out.
    pub fn coalesces_with(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::SuiteAvailable, Self::SuiteAvailable) => true,
            (Self::TasksAvailable { suite_uuid: a }, Self::TasksAvailable { suite_uuid: b }) => {
                a == b
            }
            _ => false,
        }
    }
}

/// Message from an agent to the coordinator over the WebSocket.
#[derive(Debug, Clone, Serialize, Deserialize, Readable, Writable)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentWsMessage {
    /// Acknowledge receipt of a notification (drops it from the replay buffer).
    Ack { notification_id: u64 },
    /// Response to a `Ping`.
    Pong { client_time: i64 },
}
