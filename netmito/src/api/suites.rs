use axum::{
    extract::{Path, Query, State},
    middleware,
    routing::{get, post},
    Extension, Json, Router,
};
use axum_extra::extract::Query as ExtraQuery;
use uuid::Uuid;

use crate::{
    config::InfraPool,
    error::{map_service_error, ApiError},
    schema::*,
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
        // Agent assignment routes
        .route("/{uuid}/agents/refresh", post(refresh_suite_agents))
        .route(
            "/{uuid}/agents",
            post(add_suite_agents).delete(remove_suite_agents_params),
        )
        .route("/{uuid}/agents/remove", post(remove_suite_agents))
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
        .map_err(|e| match e {
            crate::error::Error::AuthError(err) => ApiError::AuthError(err),
            crate::error::Error::ApiError(e) => e,
            _ => {
                tracing::error!("{}", e);
                ApiError::InternalServerError
            }
        })?;
    Ok(Json(resp))
}

pub async fn query_suites(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Json(req): Json<TaskSuitesQueryReq>,
) -> Result<Json<TaskSuitesQueryResp>, ApiError> {
    let resp = service::suite::user_query_task_suites(u.id, &pool, req)
        .await
        .map_err(|e| match e {
            crate::error::Error::AuthError(err) => ApiError::AuthError(err),
            crate::error::Error::ApiError(e) => e,
            _ => {
                tracing::error!("{}", e);
                ApiError::InternalServerError
            }
        })?;
    Ok(Json(resp))
}

pub async fn get_suite_details(
    Extension(_): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Path(uuid): Path<Uuid>,
) -> Result<Json<TaskSuiteQueryResp>, ApiError> {
    let details = service::suite::user_get_task_suite_by_uuid(&pool, uuid)
        .await
        .map_err(|e| match e {
            crate::error::Error::AuthError(err) => ApiError::AuthError(err),
            crate::error::Error::ApiError(e) => e,
            _ => {
                tracing::error!("{}", e);
                ApiError::InternalServerError
            }
        })?;
    Ok(Json(details))
}

pub async fn close_suite(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Path(uuid): Path<Uuid>,
) -> Result<(), ApiError> {
    service::suite::user_close_task_suite(u.id, &pool, uuid)
        .await
        .map_err(|e| match e {
            crate::error::Error::AuthError(err) => ApiError::AuthError(err),
            crate::error::Error::ApiError(e) => e,
            _ => {
                tracing::error!("{}", e);
                ApiError::InternalServerError
            }
        })?;
    Ok(())
}

pub async fn cancel_suite(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Path(uuid): Path<Uuid>,
    Query(op): Query<CancelTaskSuiteParam>,
) -> Result<Json<CancelSuiteResp>, ApiError> {
    let resp = service::suite::user_cancel_task_suite(u.id, &pool, uuid, op.op.unwrap_or_default())
        .await
        .map_err(|e| match e {
            crate::error::Error::AuthError(err) => ApiError::AuthError(err),
            crate::error::Error::ApiError(e) => e,
            _ => {
                tracing::error!("{}", e);
                ApiError::InternalServerError
            }
        })?;
    Ok(Json(resp))
}

pub async fn refresh_suite_agents(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Path(uuid): Path<Uuid>,
) -> Result<Json<RefreshSuiteAgentsResp>, ApiError> {
    let resp = service::suite::user_refresh_suite_agents(u.id, &pool, uuid)
        .await
        .map_err(map_service_error)?;
    Ok(Json(resp))
}

pub async fn add_suite_agents(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Path(uuid): Path<Uuid>,
    Json(req): Json<AddSuiteAgentsReq>,
) -> Result<Json<AddSuiteAgentsResp>, ApiError> {
    let resp = service::suite::user_add_suite_agents(u.id, &pool, uuid, req)
        .await
        .map_err(map_service_error)?;
    Ok(Json(resp))
}

pub async fn remove_suite_agents_params(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Path(uuid): Path<Uuid>,
    ExtraQuery(req): ExtraQuery<RemoveSuiteAgentsReq>,
) -> Result<Json<RemoveSuiteAgentsResp>, ApiError> {
    let resp = service::suite::user_remove_suite_agents(u.id, &pool, uuid, req)
        .await
        .map_err(map_service_error)?;
    Ok(Json(resp))
}

pub async fn remove_suite_agents(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Path(uuid): Path<Uuid>,
    Json(req): Json<RemoveSuiteAgentsReq>,
) -> Result<Json<RemoveSuiteAgentsResp>, ApiError> {
    let resp = service::suite::user_remove_suite_agents(u.id, &pool, uuid, req)
        .await
        .map_err(map_service_error)?;
    Ok(Json(resp))
}
