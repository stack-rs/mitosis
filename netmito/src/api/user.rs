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
        s3::{get_artifact, user_get_attachment, user_upload_attachment},
        task::{get_task, query_task_list, user_submit_task},
    },
};

pub fn user_router(st: InfraPool) -> Router<InfraPool> {
    Router::new()
        .route("/auth", get(auth_user))
        .route("/task", post(submit_task))
        .route("/task/:uuid", get(query_task))
        .route("/tasks", post(query_tasks))
        .route("/artifacts/:uuid/:content_type", get(download_artifact))
        .route("/attachments/:group_name/*key", get(download_attachment))
        .route("/attachment", post(upload_attachment))
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

pub async fn query_tasks(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Json(req): Json<TasksQueryReq>,
) -> Result<Json<Vec<TaskQueryInfo>>, ApiError> {
    let tasks = query_task_list(u.id, &pool, req)
        .await
        .map_err(|e| match e {
            crate::error::Error::AuthError(err) => ApiError::AuthError(err),
            crate::error::Error::ApiError(e) => e,
            _ => {
                tracing::error!("{}", e);
                ApiError::InternalServerError
            }
        })?;
    Ok(Json(tasks))
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

pub async fn upload_attachment(
    Extension(_): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Json(req): Json<UploadAttachmentReq>,
) -> Result<Json<UploadAttachmentResp>, ApiError> {
    let url = user_upload_attachment(&pool, req.group_name, req.key, req.content_length)
        .await
        .map_err(|e| match e {
            crate::error::Error::AuthError(err) => ApiError::AuthError(err),
            crate::error::Error::ApiError(e) => e,
            _ => {
                tracing::error!("{}", e);
                ApiError::InternalServerError
            }
        })?;
    Ok(Json(UploadAttachmentResp { url }))
}

pub async fn download_attachment(
    Extension(_): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Path((group_name, key)): Path<(String, String)>,
) -> Result<Json<RemoteResourceDownloadResp>, ApiError> {
    let attachment = user_get_attachment(&pool, group_name, key)
        .await
        .map_err(|e| match e {
            crate::error::Error::AuthError(err) => ApiError::AuthError(err),
            crate::error::Error::ApiError(e) => e,
            _ => {
                tracing::error!("{}", e);
                ApiError::InternalServerError
            }
        })?;
    Ok(Json(attachment))
}

pub async fn query_redis_connection_info(
    Extension(_): Extension<AuthUser>,
) -> Result<Json<RedisConnectionInfo>, ApiError> {
    let url = REDIS_CONNECTION_INFO.get().map(|info| info.client_url());
    Ok(Json(RedisConnectionInfo { url }))
}
