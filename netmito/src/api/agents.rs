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
    error::ApiError,
    schema::{
        AcceptSuiteReq, AcceptSuiteResp, AgentHeartbeatReq, AgentHeartbeatResp, AgentShutdownReq,
        AgentsQueryReq, AgentsQueryResp, FetchTasksReq, FetchTasksResp, JobReportOp, JobReportReq,
        JobReportResp, RegisterAgentReq, RegisterAgentResp, StopAgentJobReq, StopAgentJobResp,
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
        .route("/{uuid}/job", delete(stop_agent_job))
        .route_layer(middleware::from_fn_with_state(
            st.clone(),
            auth_middleware::<USER>,
        ))
        .with_state(st.clone());

    let agent_router = Router::new()
        .route("/", delete(agent_exit))
        .route("/heartbeat", post(heartbeat))
        .route("/suite", post(accept_suite))
        .route("/job", post(report_job))
        .route("/tasks", get(fetch_tasks))
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

/// `DELETE /agents/{uuid}/job?op=graceful|force`: stop the job that the agent runs now. The agent
/// stays up and picks a suite again.
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

/// `POST /agents/suite`: pick a suite, claim it, and open a job.
///
/// The transaction picks the suite, thus the span records the suite and the job when they are
/// known.
#[tracing::instrument(
    skip_all,
    fields(
        agent_uuid = %a.uuid,
        suite_uuid = tracing::field::Empty,
        job_id = tracing::field::Empty,
    )
)]
async fn accept_suite(
    Extension(a): Extension<AuthAgent>,
    State(pool): State<InfraPool>,
    Json(req): Json<AcceptSuiteReq>,
) -> Result<Json<AcceptSuiteResp>, ApiError> {
    let resp = service::agent::agent_accept_suite(a.id, &pool, req)
        .await
        .map_err(map_service_error)?;
    Ok(Json(resp))
}

/// `POST /agents/job`: move the job to the next phase. Only `Complete` returns data.
async fn report_job(
    Extension(a): Extension<AuthAgent>,
    State(pool): State<InfraPool>,
    Json(req): Json<JobReportReq>,
) -> Result<Json<JobReportResp>, ApiError> {
    let next_suite_available = match req.op {
        JobReportOp::Start => {
            service::agent::agent_start_job(a.id, &pool, req.job)
                .await
                .map_err(map_service_error)?;
            None
        }
        JobReportOp::EnterCleanup => {
            service::agent::agent_enter_cleanup(a.id, &pool, req.job)
                .await
                .map_err(map_service_error)?;
            None
        }
        JobReportOp::Complete { outcome } => Some(
            service::agent::agent_complete_job(a.id, a.uuid, &pool, req.job, outcome)
                .await
                .map_err(map_service_error)?,
        ),
    };
    Ok(Json(JobReportResp {
        next_suite_available,
    }))
}

/// `GET /agents/tasks?suite_uuid=…`: claim ready tasks of the suite.
#[tracing::instrument(skip_all, fields(agent_uuid = %a.uuid, suite_uuid = %req.suite_uuid))]
async fn fetch_tasks(
    Extension(a): Extension<AuthAgent>,
    State(pool): State<InfraPool>,
    Query(req): Query<FetchTasksReq>,
) -> Result<Json<FetchTasksResp>, ApiError> {
    let resp = service::agent::task::agent_fetch_tasks(a.id, a.uuid, &pool, req.suite_uuid)
        .await
        .map_err(map_service_error)?;
    Ok(Json(resp))
}
