use axum::{
    extract::{Path, Query, State},
    middleware,
    routing::{delete, get, post},
    Extension, Json, Router,
};
use uuid::Uuid;

use super::map_service_error;
use crate::{
    config::InfraPool,
    entity::content::ArtifactContentType,
    error::ApiError,
    schema::{
        AcceptSuiteReq, AcceptSuiteResp, AgentHeartbeatReq, AgentHeartbeatResp, AgentShutdownReq,
        AgentsQueryReq, AgentsQueryResp, CompleteJobReq, CompleteJobResp, EnterCleanupReq,
        FetchTasksReq, FetchTasksResp, HookReportReq, HookReportResp, RegisterAgentReq,
        RegisterAgentResp, RemoteResourceDownloadResp, ReportAgentTaskReq, ReportTaskResp,
        StartJobReq, StopAgentJobReq, StopAgentJobResp, TaskQueryResp,
    },
    service::{
        self,
        auth::{agent_auth_middleware, user_auth_middleware, AuthAgent, AuthUser},
    },
};

pub fn agents_router(st: InfraPool) -> Router<InfraPool> {
    let user_router = Router::new()
        .route("/", post(register_agent))
        .route("/query", post(query_agents))
        .route("/{uuid}", delete(shutdown_agent))
        .route("/{uuid}/job/stop", post(stop_agent_job))
        .route_layer(middleware::from_fn_with_state(
            st.clone(),
            user_auth_middleware,
        ))
        .with_state(st.clone());

    let agent_router = Router::new()
        .route("/heartbeat", post(heartbeat))
        .route("/suite", post(accept_suite))
        .route("/job/start", post(start_job))
        .route("/job/cleanup", post(enter_cleanup))
        .route("/job/complete", post(complete_job))
        .route("/job/hook", post(report_hook))
        .route("/tasks/fetch", post(fetch_tasks))
        .route("/tasks/report", post(report_task))
        .route("/tasks/{uuid}", get(query_task))
        .route(
            "/tasks/{uuid}/artifacts/{content_type}",
            get(download_artifact),
        )
        .route("/tasks/{uuid}/attachments/{*key}", get(download_attachment))
        .route(
            "/suites/{uuid}/attachments/{*key}",
            get(download_suite_attachment),
        )
        .route_layer(middleware::from_fn_with_state(
            st.clone(),
            agent_auth_middleware,
        ))
        .with_state(st.clone());

    Router::new().merge(user_router).merge(agent_router)
}

// ── fleet management (user-authed) ──

/// `POST /agents` — register, or re-adopt the agent already bound to this
/// machine, and mint its token.
async fn register_agent(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Json(req): Json<RegisterAgentReq>,
) -> Result<Json<RegisterAgentResp>, ApiError> {
    let resp = service::agent::user_register_agent(u.id, &pool, req)
        .await
        .map_err(map_service_error)?;
    Ok(Json(resp))
}

/// `POST /agents/query`
async fn query_agents(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Json(req): Json<AgentsQueryReq>,
) -> Result<Json<AgentsQueryResp>, ApiError> {
    let resp = service::agent::user_query_agents(u.id, &pool, req)
        .await
        .map_err(map_service_error)?;
    Ok(Json(resp))
}

/// `DELETE /agents/{uuid}?op=graceful|force` — shut the agent down. The agent db row
/// persists
async fn shutdown_agent(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Path(uuid): Path<Uuid>,
    Query(req): Query<AgentShutdownReq>,
) -> Result<(), ApiError> {
    service::agent::user_shutdown_agent_by_uuid(u.id, uuid, req.op, &pool)
        .await
        .map_err(map_service_error)?;
    Ok(())
}

/// `POST /agents/{uuid}/job/stop?op=graceful|force` — end the job the agent is
/// running now. The agent stays up and picks a suite again, so this is how a
/// user preempts one onto higher-priority work.
async fn stop_agent_job(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Path(uuid): Path<Uuid>,
    Query(req): Query<StopAgentJobReq>,
) -> Result<Json<StopAgentJobResp>, ApiError> {
    let resp = service::agent::user_stop_agent_job(u.id, uuid, req.op, &pool)
        .await
        .map_err(map_service_error)?;
    Ok(Json(resp))
}

// ── execution loop (agent-authed) ──

/// `POST /agents/heartbeat` — liveness plus the notifications the agent missed.
async fn heartbeat(
    Extension(a): Extension<AuthAgent>,
    State(pool): State<InfraPool>,
    Json(req): Json<AgentHeartbeatReq>,
) -> Result<Json<AgentHeartbeatResp>, ApiError> {
    let resp = service::agent::agent_heartbeat(a.id, a.uuid, &pool, req)
        .await
        .map_err(map_service_error)?;
    Ok(Json(resp))
}

/// `POST /agents/suite` — pick a suite and claim it, opening a job. The body's
/// optional `suite_uuid` is a preference; a stale one falls back to the best
/// available.
async fn accept_suite(
    Extension(a): Extension<AuthAgent>,
    State(pool): State<InfraPool>,
    Json(req): Json<AcceptSuiteReq>,
) -> Result<Json<AcceptSuiteResp>, ApiError> {
    let resp = service::agent::agent_accept_suite(a.id, a.uuid, &pool, req)
        .await
        .map_err(map_service_error)?;
    Ok(Json(resp))
}

/// `POST /agents/job/start` — provisioning done, execution starting.
async fn start_job(
    Extension(a): Extension<AuthAgent>,
    State(pool): State<InfraPool>,
    Json(req): Json<StartJobReq>,
) -> Result<(), ApiError> {
    service::agent::agent_start_job(a.id, &pool, req.job)
        .await
        .map_err(map_service_error)?;
    Ok(())
}

/// `POST /agents/job/cleanup` — tasks drained, cleanup starting.
async fn enter_cleanup(
    Extension(a): Extension<AuthAgent>,
    State(pool): State<InfraPool>,
    Json(req): Json<EnterCleanupReq>,
) -> Result<(), ApiError> {
    service::agent::agent_enter_cleanup(a.id, &pool, req.job)
        .await
        .map_err(map_service_error)?;
    Ok(())
}

/// `POST /agents/job/complete` — job terminal, agent back to idle.
async fn complete_job(
    Extension(a): Extension<AuthAgent>,
    State(pool): State<InfraPool>,
    Json(req): Json<CompleteJobReq>,
) -> Result<Json<CompleteJobResp>, ApiError> {
    let resp = service::agent::agent_complete_job(a.id, a.uuid, &pool, req)
        .await
        .map_err(map_service_error)?;
    Ok(Json(resp))
}

/// `POST /agents/job/hook` — record a hook result or presign its artifacts.
async fn report_hook(
    Extension(a): Extension<AuthAgent>,
    State(pool): State<InfraPool>,
    Json(req): Json<HookReportReq>,
) -> Result<Json<HookReportResp>, ApiError> {
    let resp = service::agent::hook::agent_report_hook(a.id, req.job, req.hook_type, req.op, &pool)
        .await
        .map_err(map_service_error)?;
    Ok(Json(resp))
}

/// `POST /agents/tasks/fetch` — claim a batch of the suite's ready tasks.
async fn fetch_tasks(
    Extension(a): Extension<AuthAgent>,
    State(pool): State<InfraPool>,
    Json(req): Json<FetchTasksReq>,
) -> Result<Json<FetchTasksResp>, ApiError> {
    let resp =
        service::agent::task::agent_fetch_tasks(a.id, a.uuid, &pool, req.suite_uuid, req.max_count)
            .await
            .map_err(map_service_error)?;
    Ok(Json(resp))
}

/// `POST /agents/tasks/report` — mirrors the worker report, plus the job handle.
async fn report_task(
    Extension(a): Extension<AuthAgent>,
    State(pool): State<InfraPool>,
    Json(req): Json<ReportAgentTaskReq>,
) -> Result<Json<ReportTaskResp>, ApiError> {
    let url = service::agent::task::agent_report_task(a.id, a.uuid, req.job, req.id, req.op, &pool)
        .await
        .map_err(map_service_error)?;
    Ok(Json(ReportTaskResp { url }))
}

/// `GET /agents/tasks/{uuid}` — a task's state and result, for `watch`
/// dependencies. Reuses the service; agent auth is just the gate.
async fn query_task(
    Extension(_): Extension<AuthAgent>,
    State(pool): State<InfraPool>,
    Path(uuid): Path<Uuid>,
) -> Result<Json<TaskQueryResp>, ApiError> {
    let task = service::task::get_task_by_uuid(&pool, uuid)
        .await
        .map_err(map_service_error)?;
    Ok(Json(task))
}

/// `GET /agents/tasks/{uuid}/artifacts/{content_type}` — presigned input download.
async fn download_artifact(
    Extension(_): Extension<AuthAgent>,
    State(pool): State<InfraPool>,
    Path((uuid, content_type)): Path<(Uuid, ArtifactContentType)>,
) -> Result<Json<RemoteResourceDownloadResp>, ApiError> {
    let artifact = service::s3::download_artifact_by_uuid(&pool, uuid, content_type)
        .await
        .map_err(map_service_error)?;
    Ok(Json(artifact))
}

/// `GET /agents/tasks/{uuid}/attachments/{*key}` — presigned input download.
async fn download_attachment(
    Extension(_): Extension<AuthAgent>,
    State(pool): State<InfraPool>,
    Path((uuid, key)): Path<(Uuid, String)>,
) -> Result<Json<RemoteResourceDownloadResp>, ApiError> {
    let attachment = service::s3::worker_download_attachment(&pool, uuid, key)
        .await
        .map_err(map_service_error)?;
    Ok(Json(attachment))
}

/// `GET /agents/suites/{uuid}/attachments/{*key}` — presigned input download for
/// a hook, whose inputs belong to the suite rather than to any one task.
async fn download_suite_attachment(
    Extension(_): Extension<AuthAgent>,
    State(pool): State<InfraPool>,
    Path((uuid, key)): Path<(Uuid, String)>,
) -> Result<Json<RemoteResourceDownloadResp>, ApiError> {
    let attachment = service::s3::suite_download_attachment(&pool, uuid, key)
        .await
        .map_err(map_service_error)?;
    Ok(Json(attachment))
}
