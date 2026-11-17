use axum::{
    extract::{Path, Query, State},
    middleware,
    routing::{delete, post},
    Extension, Json, Router,
};
use uuid::Uuid;

use super::map_service_error;
use crate::{
    config::InfraPool,
    error::ApiError,
    schema::{
        AgentHeartbeatReq, AgentHeartbeatResp, AgentShutdownReq, AgentsQueryReq, AgentsQueryResp,
        RegisterAgentReq, RegisterAgentResp,
    },
    service::{
        self,
        auth::{
            auth_middleware,
            roles::{AGENT, USER},
            AuthAgent, AuthUser,
        },
    },
};

pub fn agents_router(st: InfraPool) -> Router<InfraPool> {
    let user_router = Router::new()
        .route("/", post(register_agent))
        .route("/query", post(query_agents))
        .route("/{uuid}", delete(shutdown_agent))
        .route_layer(middleware::from_fn_with_state(
            st.clone(),
            auth_middleware::<USER>,
        ))
        .with_state(st.clone());

    let agent_router = Router::new()
        .route("/", delete(agent_exit))
        .route("/heartbeat", post(heartbeat))
        .route_layer(middleware::from_fn_with_state(
            st.clone(),
            auth_middleware::<AGENT>,
        ))
        .with_state(st.clone());

    Router::new().merge(user_router).merge(agent_router)
}

/// `POST /agents`: register an agent and issue its token.
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

/// `DELETE /agents/{uuid}?op=graceful|force`: shut the agent down. The agent row stays.
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

/// `DELETE /agents`: the agent exits. Retire it now, so that it does not wait for its heartbeat times out.
async fn agent_exit(
    Extension(a): Extension<AuthAgent>,
    State(pool): State<InfraPool>,
) -> Result<(), ApiError> {
    service::agent::retire_agent(&pool, a.id, service::agent::RetireCause::SelfExit)
        .await
        .map_err(map_service_error)?;
    Ok(())
}

/// `POST /agents/heartbeat`: record a heartbeat and return the notifications that the agent has not acknowledged.
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
