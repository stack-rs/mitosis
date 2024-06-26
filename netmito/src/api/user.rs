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
    config::{InfraPool, REDIS_CONNECTION_INFO},
    entity::content::ArtifactContentType,
    error::ApiError,
    schema::*,
    service::{
        auth::{user_auth_middleware, user_login, AuthUser},
        s3::get_artifact,
        task::{get_task, user_submit_task},
    },
};

pub fn user_router(st: InfraPool) -> Router<InfraPool> {
    Router::new()
        .route("/auth", get(auth_user))
        .route("/task", post(submit_task))
        .route("/task/:uuid", get(query_task))
        .route("/artifacts/:uuid/:content_type", get(download_artifact))
        .route("/redis", get(query_redis_connection_info))
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
    Json(req): Json<SubmitTaskReq>,
) -> Result<Json<SubmitTaskResp>, ApiError> {
    let resp = user_submit_task(&pool, u.id, req)
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

pub async fn download_artifact(
    Extension(_): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Path((uuid, content_type)): Path<(Uuid, ArtifactContentType)>,
) -> Result<Json<RemoteResourceDownloadResp>, ApiError> {
    let artifact = get_artifact(&pool, uuid, content_type)
        .await
        .map_err(|e| match e {
            crate::error::Error::AuthError(err) => ApiError::AuthError(err),
            crate::error::Error::ApiError(e) => e,
            _ => {
                tracing::error!("{}", e);
                ApiError::InternalServerError
            }
        })?;
    Ok(Json(artifact))
}

pub async fn query_redis_connection_info(
    Extension(_): Extension<AuthUser>,
) -> Result<Json<RedisConnectionInfo>, ApiError> {
    let url = REDIS_CONNECTION_INFO.get().map(|info| info.client_url());
    Ok(Json(RedisConnectionInfo { url }))
}
