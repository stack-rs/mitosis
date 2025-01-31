use axum::{
    extract::{Path, State},
    middleware,
    routing::{delete, get, post},
    Extension, Json, Router,
};
use uuid::Uuid;

use crate::{
    config::{InfraPool, REDIS_CONNECTION_INFO},
    entity::content::ArtifactContentType,
    error::{ApiError, ApiResult, Error},
    schema::*,
    service::{
        auth::{token::generate_worker_token, worker_auth_middleware, AuthUser, AuthWorker},
        s3::{get_artifact, get_attachment},
        task::get_task,
        worker,
    },
};

pub fn worker_router(st: InfraPool) -> Router<InfraPool> {
    Router::new()
        .route("/", delete(unregister))
        .route("/heartbeat", post(heartbeat))
        .route("/tasks", post(report_task).get(fetch_task))
        .route("/tasks/{uuid}", get(query_task))
        .route("/artifacts/{uuid}/{content_type}", get(download_artifact))
        .route("/attachments/{uuid}/{*key}", get(download_attachment))
        .route_layer(middleware::from_fn_with_state(
            st.clone(),
            worker_auth_middleware,
        ))
        .with_state(st)
}

pub async fn register(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Json(req): Json<RegisterWorkerReq>,
) -> ApiResult<Json<RegisterWorkerResp>> {
    let uuid = worker::register_worker(
        u.id,
        Vec::from_iter(req.tags),
        Vec::from_iter(req.groups),
        &pool,
    )
    .await
    .map_err(|e| match e {
        Error::ApiError(e) => e,
        _ => {
            tracing::error!("{}", e);
            ApiError::InternalServerError
        }
    })?;
    let token =
        generate_worker_token(uuid.clone().to_string(), 0, req.lifetime).map_err(|e| match e {
            crate::error::Error::AuthError(err) => ApiError::AuthError(err),
            crate::error::Error::ApiError(e) => e,
            _ => {
                tracing::error!("{}", e);
                ApiError::InternalServerError
            }
        })?;
    let redis_url = REDIS_CONNECTION_INFO.get().map(|info| info.worker_url());
    Ok(Json(RegisterWorkerResp {
        worker_id: uuid,
        token,
        redis_url,
    }))
}

pub async fn unregister(
    Extension(w): Extension<AuthWorker>,
    State(pool): State<InfraPool>,
) -> ApiResult<Json<()>> {
    match worker::unregister_worker(w.id, &pool).await {
        Ok(_) => Ok(Json(())),
        Err(Error::ApiError(e)) => Err(e),
        Err(e) => {
            tracing::error!("{}", e);
            Err(ApiError::InternalServerError)
        }
    }
}

pub async fn heartbeat(
    Extension(w): Extension<AuthWorker>,
    State(pool): State<InfraPool>,
) -> ApiResult<Json<()>> {
    match worker::heartbeat(w.id, &pool).await {
        Ok(_) => Ok(Json(())),
        Err(Error::ApiError(e)) => Err(e),
        Err(e) => {
            tracing::error!("{}", e);
            Err(ApiError::InternalServerError)
        }
    }
}

pub async fn fetch_task(
    Extension(w): Extension<AuthWorker>,
    State(pool): State<InfraPool>,
) -> ApiResult<Json<Option<WorkerTaskResp>>> {
    match worker::fetch_task(w.id, &pool).await {
        Ok(t) => Ok(Json(t)),
        Err(Error::ApiError(e)) => Err(e),
        Err(e) => {
            tracing::error!("{}", e);
            Err(ApiError::InternalServerError)
        }
    }
}

pub async fn report_task(
    Extension(w): Extension<AuthWorker>,
    State(pool): State<InfraPool>,
    Json(ReportTaskReq { id, op }): Json<ReportTaskReq>,
) -> ApiResult<Json<ReportTaskResp>> {
    match worker::report_task(w.id, id, op, &pool).await {
        Ok(url) => Ok(Json(ReportTaskResp { url })),
        Err(Error::ApiError(e)) => Err(e),
        Err(e) => {
            tracing::error!("{}", e);
            Err(ApiError::InternalServerError)
        }
    }
}

pub async fn download_artifact(
    Extension(_): Extension<AuthWorker>,
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

pub async fn download_attachment(
    Extension(_): Extension<AuthWorker>,
    State(pool): State<InfraPool>,
    Path((uuid, key)): Path<(Uuid, String)>,
) -> Result<Json<RemoteResourceDownloadResp>, ApiError> {
    let attachment = get_attachment(&pool, uuid, key)
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

pub async fn query_task(
    Extension(_): Extension<AuthWorker>,
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
