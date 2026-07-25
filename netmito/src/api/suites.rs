use axum::{
    extract::{Path, Query, State},
    middleware,
    routing::{get, post},
    Extension, Json, Router,
};
use uuid::Uuid;

use super::map_service_error;
use crate::{
    config::InfraPool,
    error::ApiError,
    schema::{
        CancelTaskSuiteParam, CreateTaskSuiteReq, CreateTaskSuiteResp, SuiteAgentOverrideReq,
        SuiteAgentOverrideResp, TaskSuiteQueryResp, TaskSuitesQueryReq, TaskSuitesQueryResp,
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
        .route("/{uuid}/agents/override", post(override_agents_for_suite))
        .route_layer(middleware::from_fn_with_state(
            st.clone(),
            user_auth_middleware,
        ))
        .with_state(st)
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
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Path(uuid): Path<Uuid>,
) -> Result<Json<TaskSuiteQueryResp>, ApiError> {
    let details = service::suite::user_get_task_suite_by_uuid(u.id, &pool, uuid)
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
) -> Result<(), ApiError> {
    service::suite::user_cancel_task_suite(u.id, &pool, uuid, param.op.unwrap_or_default())
        .await
        .map_err(map_service_error)?;
    Ok(())
}

pub async fn override_agents_for_suite(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Path(uuid): Path<Uuid>,
    Json(req): Json<SuiteAgentOverrideReq>,
) -> Result<Json<SuiteAgentOverrideResp>, ApiError> {
    let resp = service::suite::user_override_agents_for_suite(u.id, &pool, uuid, req)
        .await
        .map_err(map_service_error)?;
    Ok(Json(resp))
}
