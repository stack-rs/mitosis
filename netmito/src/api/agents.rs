//! Agent API handlers

use axum::{
    extract::{Path, Query, State},
    middleware,
    routing::{delete, get, post},
    Extension, Json, Router,
};
use uuid::Uuid;

use crate::{
    config::InfraPool,
    error::{map_service_error, ApiError},
    schema::*,
    service::{
        self,
        auth::{agent_auth_middleware, user_auth_middleware, AuthAgent, AuthUser},
        agent_task,
    },
};

pub fn agents_router(st: InfraPool) -> Router<InfraPool> {
    // User-authenticated routes
    let user_auth_router = Router::new()
        .route("/", post(register_agent))
        .route("/query", post(query_agents))
        .route("/{uuid}", delete(user_shutdown_agent))
        .route_layer(middleware::from_fn_with_state(
            st.clone(),
            user_auth_middleware,
        ))
        .with_state(st.clone());

    // Agent-authenticated routes
    let agent_auth_router = Router::new()
        .route("/heartbeat", post(heartbeat))
        .route("/suite", get(fetch_suite))
        .route("/suite/accept", post(accept_suite))
        .route("/suite/start", post(start_suite))
        .route("/suite/complete", post(complete_suite))
        .route("/suite/cleanup", post(enter_cleanup))
        .route("/tasks/fetch", post(fetch_tasks))
        .route("/tasks/{uuid}/report", post(report_task))
        .route_layer(middleware::from_fn_with_state(
            st.clone(),
            agent_auth_middleware,
        ))
        .with_state(st.clone());

    Router::new()
        .merge(user_auth_router)
        .merge(agent_auth_router)
}

pub async fn register_agent(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Json(req): Json<RegisterAgentReq>,
) -> Result<Json<RegisterAgentResp>, ApiError> {
    let resp = service::agent::user_register_agent(u.id, &pool, req)
        .await
        .map_err(map_service_error)?;
    Ok(Json(resp))
}

pub async fn query_agents(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Json(req): Json<AgentsQueryReq>,
) -> Result<Json<AgentsQueryResp>, ApiError> {
    let resp = service::agent::user_query_agents(u.id, &pool, req)
        .await
        .map_err(map_service_error)?;
    Ok(Json(resp))
}

pub async fn user_shutdown_agent(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Path(uuid): Path<Uuid>,
    Query(req): Query<AgentShutdownReq>,
) -> Result<(), ApiError> {
    tracing::debug!("Shutdown agent {} with op {:?}", uuid, req.op);
    service::agent::user_remove_agent_by_uuid(u.id, uuid, req.op, &pool)
        .await
        .map_err(map_service_error)?;
    Ok(())
}

/// Returns pending actions and notification counter
pub async fn heartbeat(
    Extension(m): Extension<AuthAgent>,
    State(pool): State<InfraPool>,
    Json(req): Json<AgentHeartbeatReq>,
) -> Result<Json<AgentHeartbeatResp>, ApiError> {
    let resp = service::agent::agent_heartbeat(m.id, m.uuid, &pool, req)
        .await
        .map_err(map_service_error)?;
    Ok(Json(resp))
}

/// Fetch an available suite for execution
pub async fn fetch_suite(
    Extension(m): Extension<AuthAgent>,
    State(pool): State<InfraPool>,
    Query(req): Query<FetchSuiteReq>,
) -> Result<Json<FetchSuiteResp>, ApiError> {
    let resp = service::agent::agent_fetch_suite(m.id, &pool, req.suite_uuid)
        .await
        .map_err(map_service_error)?;
    Ok(Json(resp))
}

/// POST /agents/suite/accept - Accept a suite for execution
async fn accept_suite(
    Extension(m): Extension<AuthAgent>,
    State(pool): State<InfraPool>,
    Json(req): Json<AcceptSuiteReq>,
) -> Result<Json<AcceptSuiteResp>, ApiError> {
    let resp = service::agent::agent_accept_suite(m.id, &pool, req)
        .await
        .map_err(map_service_error)?;
    Ok(Json(resp))
}

/// POST /agents/suite/start - Report suite execution started
async fn start_suite(
    Extension(m): Extension<AuthAgent>,
    State(pool): State<InfraPool>,
    Json(req): Json<StartSuiteReq>,
) -> Result<(), ApiError> {
    service::agent::agent_start_suite(m.id, &pool, req.suite_uuid)
        .await
        .map_err(map_service_error)?;
    Ok(())
}

/// POST /agents/suite/complete - Report suite execution completed
async fn complete_suite(
    Extension(m): Extension<AuthAgent>,
    State(pool): State<InfraPool>,
    Json(req): Json<CompleteSuiteReq>,
) -> Result<Json<CompleteSuiteResp>, ApiError> {
    let resp = service::agent::agent_complete_suite(m.id, &pool, req)
        .await
        .map_err(map_service_error)?;
    Ok(Json(resp))
}

/// POST /agents/suite/cleanup - Report agent entering cleanup phase
async fn enter_cleanup(
    Extension(m): Extension<AuthAgent>,
    State(pool): State<InfraPool>,
) -> Result<(), ApiError> {
    service::agent::agent_enter_cleanup(m.id, &pool)
        .await
        .map_err(map_service_error)?;
    Ok(())
}

/// POST /agents/tasks/fetch - Batch-fetch tasks from an assigned suite
async fn fetch_tasks(
    Extension(m): Extension<AuthAgent>,
    State(pool): State<InfraPool>,
    Json(req): Json<FetchTasksReq>,
) -> Result<Json<FetchTasksResp>, ApiError> {
    let resp = agent_task::agent_fetch_tasks(m.id, m.uuid, &pool, req.suite_uuid, req.max_count)
        .await
        .map_err(map_service_error)?;
    Ok(Json(resp))
}

/// POST /agents/tasks/{uuid}/report - Report the result of an agent-executed task
async fn report_task(
    Extension(m): Extension<AuthAgent>,
    State(pool): State<InfraPool>,
    Path(uuid): Path<uuid::Uuid>,
    Json(req): Json<ReportAgentTaskReq>,
) -> Result<Json<Option<String>>, ApiError> {
    let presigned_url =
        agent_task::agent_report_task(m.uuid, uuid, req.op, &pool)
            .await
            .map_err(map_service_error)?;
    Ok(Json(presigned_url))
}
