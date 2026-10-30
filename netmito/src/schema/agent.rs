//! Wire types for agents: the user routes under `/agents`, the agent routes
//! under `/agents`, and the notifications on `/ws/agents`.

use std::collections::HashSet;

use sea_orm::FromQueryResult;
use serde::{Deserialize, Serialize};
use speedy::{Readable, Writable};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::entity::{
    hook_tasks::HookType,
    state::{AgentState, HookExecState, SuiteJobState, TaskSuiteState},
};

use super::exec::ExecHooks;
use super::suite::WorkerSchedulePlan;
use super::task::{TaskResultSpec, WorkerTaskResp};

/// Request to register an agent.
///
/// One machine has one agent row for all its life, because `machine_code` is
/// unique. If the machine is already known, registration uses its agent row
/// again: it replaces the tags, labels, groups and metadata, and it issues a
/// new token.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterAgentReq {
    /// Tags that a suite must match to run on this agent (e.g. `["gpu", "linux"]`).
    #[serde(default)]
    pub tags: HashSet<String>,
    /// Labels for queries and filters (e.g. `["datacenter:us-west"]`).
    #[serde(default)]
    pub labels: HashSet<String>,
    /// The group that gets the Admin role on the agent. Only an Admin of this
    /// group can shut the agent down, and the caller must be one. The default
    /// is the personal group of the caller.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admin_group: Option<String>,
    /// Groups that get the Write role on the agent.
    #[serde(default)]
    pub groups: HashSet<String>,
    /// The lifetime of the agent token. `None` means the token never expires.
    #[serde(default, with = "humantime_serde")]
    pub lifetime: Option<std::time::Duration>,
    /// A stable identifier of the machine that runs the agent.
    pub machine_code: String,
    /// Static data about the agent process, kept on the machine row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<AgentMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterAgentResp {
    pub agent_uuid: Uuid,
    pub token: String,
    /// The last notification id issued to this agent. The agent counts its
    /// notifications from this value.
    pub notification_counter: u64,
    /// True if the machine was already known and its agent row is used again.
    pub reused: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMetadata {
    /// Agent binary version (e.g. "0.6.8").
    pub version: String,
    /// Long version string, with build data.
    pub long_version: String,
}

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub machine_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentsQueryResp {
    pub count: u64,
    pub agents: Vec<AgentInfo>,
    pub group_name: String,
}

/// Query parameters of `DELETE /agents/{uuid}`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentShutdownReq {
    #[serde(default)]
    pub op: AgentShutdownOp,
}

/// How to shut an agent down. The agent row stays in both cases.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
pub enum AgentShutdownOp {
    /// Tell the agent to stop. The agent finishes its current work and then
    /// marks itself `Offline`.
    #[default]
    #[serde(alias = "graceful")]
    Graceful,
    /// Mark the agent `Offline` now, then tell it to stop.
    #[serde(alias = "force")]
    Force,
}

/// Query parameters of the stop-job endpoints.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StopAgentJobReq {
    #[serde(default)]
    pub op: StopJobOp,
}

/// How to stop a job. In both cases the agent stays up and then picks a suite again. Thus a stop
/// is how a user moves an agent to the suite with the highest priority now.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
pub enum StopJobOp {
    /// Claim no more tasks, let the running tasks finish, then run the cleanup hook. The agent
    /// moves the job to `Completed` itself.
    #[default]
    #[serde(alias = "graceful")]
    Graceful,
    /// Stop now. The job becomes `Killed` and its tasks go back to `Ready`. The cleanup hook does
    /// not run.
    #[serde(alias = "force")]
    Force,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StopAgentJobResp {
    /// False if the agent had no job to stop. This is not an error.
    pub stopped: bool,
    /// The suite of the stopped job.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suite_uuid: Option<Uuid>,
    /// The number of the stopped job in its suite.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_id: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentHeartbeatReq {
    /// The state the agent reports for itself.
    pub state: AgentState,
    /// The highest notification id the agent has processed.
    #[serde(default)]
    pub last_notification_id: u64,
}

/// The notifications that the agent has not acknowledged yet.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentHeartbeatResp {
    pub notifications: Vec<WsNotificationEvent>,
}

/// Request body of `POST /agents/suite`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AcceptSuiteReq {
    /// The suite that the agent prefers. If this suite cannot run now, the coordinator picks the
    /// best available suite.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suite_uuid: Option<Uuid>,
}

/// The suite that an agent runs, with all the data to run it.
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

/// A hook row of a job. The agent reports the hook with `id` on `POST /tasks/report`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobHook {
    pub id: i64,
    /// The key of the hook artifacts.
    pub uuid: Uuid,
    pub hook_type: HookType,
}

/// Response of `POST /agents/suite`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcceptSuiteResp {
    /// False if the agent got no suite. This is not an error: no suite is available, or the agent
    /// already runs a job.
    pub accepted: bool,
    /// The suite to run, if `accepted` is true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suite: Option<TaskSuiteSpec>,
    /// The handle of the job (`suite_agent_jobs.id`). The agent sends it on each later call for
    /// this job.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job: Option<i64>,
    /// The number of the job in its suite.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_id: Option<i32>,
    /// One row for each hook of the suite.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hooks: Vec<JobHook>,
    /// Why the agent got no suite, if `accepted` is false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Request body of `POST /agents/job`: the agent moves its job to the next phase.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobReportReq {
    /// The job handle from `AcceptSuiteResp`.
    pub job: i64,
    pub op: JobReportOp,
}

/// The phase that the agent starts. `Lost` and `Killed` are not here, because only the
/// coordinator writes them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum JobReportOp {
    /// The provision hook is done. The agent starts to run tasks.
    Start,
    /// The agent claims no more tasks. The cleanup hook starts.
    EnterCleanup,
    /// The job ended. `outcome` tells how.
    Complete { outcome: SuiteJobOutcome },
}

/// How the job ended, as the agent reports it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SuiteJobOutcome {
    /// All hooks completed successfully.
    Completed,
    /// The job failed. The hook rows keep the full output.
    Failed { reason: JobFailureReason },
}

/// The phase in which a job failed.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum JobFailureKind {
    ProvisionFailed,
    /// The background hook stopped before the job ended.
    BackgroundExited,
    ExecutionError,
    CleanupFailed,
}

/// A short reason for a failed job. The job row has no column for it, thus the coordinator only
/// logs it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct JobFailureReason {
    pub kind: JobFailureKind,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobReportResp {
    /// For `Complete` only: whether the agent can accept another suite now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_suite_available: Option<bool>,
}

/// Query parameters of `GET /agents/tasks`.
///
/// The coordinator calculates the number of tasks from the schedule of the suite. Thus the agent
/// does not give a number.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchTasksReq {
    pub suite_uuid: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchTasksResp {
    /// The claimed tasks. Empty if the suite has no ready task.
    pub tasks: Vec<WorkerTaskResp>,
    /// True while the suite is `Open`. If `tasks` is empty and this is true, the agent keeps its
    /// job and asks again later, because a new task can come before the suite closes. Thus the
    /// provisioned environment is used again.
    #[serde(default)]
    pub hold_job_open: bool,
}

/// Request body of `POST /suites/{uuid}/jobs/query`. All the fields must match.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SuiteJobsQueryReq {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub states: Option<HashSet<SuiteJobState>>,
    /// Only the jobs of this agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_uuid: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<u64>,
    /// Return only the number of the matching jobs.
    #[serde(default)]
    pub count: bool,
}

/// One job of a suite.
#[derive(Debug, Clone, Serialize, Deserialize, FromQueryResult)]
pub struct SuiteJobInfo {
    /// The number of the job in its suite.
    pub job_id: i32,
    pub state: SuiteJobState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_uuid: Option<Uuid>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuiteJobsQueryResp {
    pub count: u64,
    pub jobs: Vec<SuiteJobInfo>,
}

/// One hook of a job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookTaskInfo {
    /// The key of the hook artifacts.
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

/// Response of `GET /suites/{uuid}/jobs/{job_id}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuiteJobQueryResp {
    pub info: SuiteJobInfo,
    pub hooks: Vec<HookTaskInfo>,
}

/// A notification with its sequence id. The id tells an agent which
/// notifications it has already processed.
///
/// `speedy` encodes it on the WebSocket. The heartbeat response sends the same
/// events as JSON, thus it also needs serde.
#[derive(Debug, Clone, Serialize, Deserialize, Readable, Writable)]
pub struct WsNotificationEvent {
    /// Per-agent sequence id. It increases by one for each notification.
    pub id: u64,
    pub event: AgentNotification,
}

/// A message from the coordinator that tells an agent to do something.
///
/// Add new variants only at the end. `speedy` encodes the variant index, thus
/// a variant inserted in the middle changes the meaning of the variants after it.
#[derive(Debug, Clone, Serialize, Deserialize, Readable, Writable)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentNotification {
    /// Shut down. With `graceful`, finish the current work first.
    Shutdown { graceful: bool },
    /// The coordinator restarted and its notification ids start again from
    /// `counter`. `boot_id` identifies the new coordinator process.
    CounterSync { counter: u64, boot_id: Uuid },
    /// A suite that this agent can run has work. The event does not name the
    /// suite: the coordinator picks the best suite when the agent accepts one,
    /// because the best suite can change before the agent asks.
    SuiteAvailable,
    /// Stop the current suite and run a suite with a higher priority.
    ///
    /// The coordinator does not send this yet. The variant keeps its position
    /// on the wire for a later change.
    PreemptSuite {
        new_suite_uuid: Uuid,
        new_priority: i32,
        current_suite_uuid: Uuid,
    },
    /// The suite that the agent runs was cancelled. With `graceful`, claim no
    /// more tasks and let the running tasks and the cleanup hook finish.
    /// Otherwise stop the job now.
    SuiteCancelled {
        suite_uuid: Uuid,
        reason: String,
        graceful: bool,
    },
    /// These tasks were cancelled. Stop them if they run.
    TasksCancelled { task_uuids: Vec<Uuid> },
    /// Stop the job on `suite_uuid`. The agent stays up and picks a suite
    /// again. An agent that runs a different suite ignores the event.
    StopJob { suite_uuid: Uuid, graceful: bool },
}

impl AgentNotification {
    /// Whether an event that waits in the buffer makes this event unnecessary.
    ///
    /// Only for the events that carry no data: the agent reads the current
    /// state over HTTP, thus a second copy gives it nothing. A burst of
    /// submissions also must not push older events out of the buffer.
    pub fn coalesces_with(&self, other: &Self) -> bool {
        matches!((self, other), (Self::SuiteAvailable, Self::SuiteAvailable))
    }
}

/// A message from an agent to the coordinator on the WebSocket.
#[derive(Debug, Clone, Serialize, Deserialize, Readable, Writable)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentWsMessage {
    /// The agent processed all notifications up to and including this id.
    Ack { notification_id: u64 },
    /// The reply to a ping, with the agent's clock in Unix seconds.
    Pong { client_time: i64 },
}
