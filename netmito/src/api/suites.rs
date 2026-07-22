use axum::{
    extract::{Path, Query, State},
    middleware,
    routing::{get, post},
    Extension, Json, Router,
};
use uuid::Uuid;

use crate::{
    config::InfraPool,
    entity::task_suite_agent::SuiteAgentSelectionType,
    error::ApiError,
    schema::{
        CancelSuiteResp, CancelTaskSuiteParam, CreateTaskSuiteReq, CreateTaskSuiteResp,
        RemoveSuiteAgentResp, SuiteAgentReq, SuiteAgentResp, TaskSuiteQueryResp, TaskSuitesQueryReq,
        TaskSuitesQueryResp,
    },
    service::{
        self,
        auth::{user_auth_middleware, AuthUser},
    },
};

pub fn suites_router(st: InfraPool) -> Router<InfraPool> {
    Router::new()
        .route("/", post(create_suite))
        .route("/query", post(query_suites))
        .route("/{uuid}", get(get_suite_details).delete(cancel_suite))
        .route("/{uuid}/close", post(close_suite))
        .route("/{uuid}/agents/include", post(include_suite_agents))
        .route("/{uuid}/agents/exclude", post(exclude_suite_agents))
        .route("/{uuid}/agents/remove", post(remove_suite_agents))
        .route_layer(middleware::from_fn_with_state(
            st.clone(),
            user_auth_middleware,
        ))
        .with_state(st)
}

/// Map a service-layer error onto the API error surface.
fn map_service_error(e: crate::error::Error) -> ApiError {
    match e {
        crate::error::Error::AuthError(err) => ApiError::AuthError(err),
        crate::error::Error::ApiError(e) => e,
        _ => {
            tracing::error!("{}", e);
            ApiError::InternalServerError
        }
    }
}

pub async fn create_suite(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Json(req): Json<CreateTaskSuiteReq>,
) -> Result<Json<CreateTaskSuiteResp>, ApiError> {
    let resp = service::suite::user_create_task_suite(u.id, &pool, req)
        .await
        .map_err(map_service_error)?;
    Ok(Json(resp))
}

pub async fn query_suites(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Json(req): Json<TaskSuitesQueryReq>,
) -> Result<Json<TaskSuitesQueryResp>, ApiError> {
    let resp = service::suite::user_query_task_suites(u.id, &pool, req)
        .await
        .map_err(map_service_error)?;
    Ok(Json(resp))
}

pub async fn get_suite_details(
    Extension(_): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Path(uuid): Path<Uuid>,
) -> Result<Json<TaskSuiteQueryResp>, ApiError> {
    let details = service::suite::user_get_task_suite_by_uuid(&pool, uuid)
        .await
        .map_err(map_service_error)?;
    Ok(Json(details))
}

pub async fn close_suite(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Path(uuid): Path<Uuid>,
) -> Result<(), ApiError> {
    service::suite::user_close_task_suite(u.id, &pool, uuid)
        .await
        .map_err(map_service_error)?;
    Ok(())
}

pub async fn cancel_suite(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Path(uuid): Path<Uuid>,
    Query(param): Query<CancelTaskSuiteParam>,
) -> Result<Json<CancelSuiteResp>, ApiError> {
    let resp =
        service::suite::user_cancel_task_suite(u.id, &pool, uuid, param.op.unwrap_or_default())
            .await
            .map_err(map_service_error)?;
    Ok(Json(resp))
}

pub async fn include_suite_agents(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Path(uuid): Path<Uuid>,
    Json(req): Json<SuiteAgentReq>,
) -> Result<Json<SuiteAgentResp>, ApiError> {
    let resp = service::suite::user_set_suite_agent(
        u.id,
        &pool,
        uuid,
        req.agent_uuid,
        SuiteAgentSelectionType::UserIncluded,
    )
    .await
    .map_err(map_service_error)?;
    Ok(Json(resp))
}

pub async fn exclude_suite_agents(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Path(uuid): Path<Uuid>,
    Json(req): Json<SuiteAgentReq>,
) -> Result<Json<SuiteAgentResp>, ApiError> {
    let resp = service::suite::user_set_suite_agent(
        u.id,
        &pool,
        uuid,
        req.agent_uuid,
        SuiteAgentSelectionType::UserExcluded,
    )
    .await
    .map_err(map_service_error)?;
    Ok(Json(resp))
}

pub async fn remove_suite_agents(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Path(uuid): Path<Uuid>,
    Json(req): Json<SuiteAgentReq>,
) -> Result<Json<RemoveSuiteAgentResp>, ApiError> {
    let resp = service::suite::user_remove_suite_agent(u.id, &pool, uuid, req.agent_uuid)
        .await
        .map_err(map_service_error)?;
    Ok(Json(resp))
}
