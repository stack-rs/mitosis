use std::net::SocketAddr;

use axum::{
    extract::{ConnectInfo, Path, State},
    http::StatusCode,
    middleware,
    routing::{get, post},
    Extension, Json, Router,
};
use uuid::Uuid;

use crate::{
    config::InfraPool,
    error::ApiError,
    schema::*,
    service::{
        auth::{user_auth_middleware, user_login, AuthUser},
        task::{get_task, user_submit_task},
    },
};

pub fn user_router(st: InfraPool) -> Router<InfraPool> {
    Router::new()
        .route("/auth", get(auth_user))
        .route("/task", post(submit_task))
        .route("/task/:uuid", get(query_task))
        .layer(middleware::from_fn_with_state(
            st.clone(),
            user_auth_middleware,
        ))
        .with_state(st)
}

pub async fn login_user(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    State(pool): State<InfraPool>,
    Json(req): Json<UserLoginReq>,
) -> Result<Json<UserLoginResp>, ApiError> {
    let token = user_login(&pool.db, &req.username, &req.md5_password, addr)
        .await
        .map_err(|e| match e {
            crate::error::Error::AuthError(err) => ApiError::AuthError(err),
            crate::error::Error::ApiError(e) => e,
            _ => {
                tracing::error!("{}", e);
                ApiError::InternalServerError
            }
        })?;
    Ok(Json(UserLoginResp { token }))
}

pub async fn auth_user(Extension(_): Extension<AuthUser>) -> StatusCode {
    StatusCode::OK
}

pub async fn submit_task(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Json(SubmitTaskReq {
        group_name,
        tags,
        timeout,
        priority,
        task_spec,
    }): Json<SubmitTaskReq>,
) -> Result<Json<SubmitTaskResp>, ApiError> {
    let (task_id, uuid) = user_submit_task(
        &pool,
        u.id,
        group_name,
        Vec::from_iter(tags),
        timeout,
        priority,
        task_spec,
    )
    .await
    .map_err(|e| match e {
        crate::error::Error::AuthError(err) => ApiError::AuthError(err),
        crate::error::Error::ApiError(e) => e,
        _ => {
            tracing::error!("{}", e);
            ApiError::InternalServerError
        }
    })?;
    Ok(Json(SubmitTaskResp { task_id, uuid }))
}

pub async fn query_task(
    Extension(_): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Path(uuid): Path<Uuid>,
) -> Result<Json<TaskQueryResp>, ApiError> {
    let task = get_task(&pool, uuid).await.map_err(|e| match e {
        crate::error::Error::AuthError(err) => ApiError::AuthError(err),
        crate::error::Error::ApiError(e) => e,
        _ => {
            tracing::error!("{}", e);
            ApiError::InternalServerError
        }
    })?;
    Ok(Json(task))
}
