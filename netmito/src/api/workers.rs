use axum::{
    extract::{Path, Query, State},
    middleware,
    routing::{delete, get, post, put},
    Extension, Json, Router,
};
use axum_extra::extract::Query as ExtraQuery;
use uuid::Uuid;

use crate::{
    config::{InfraPool, REDIS_CONNECTION_INFO},
    entity::content::ArtifactContentType,
    error::{ApiError, ApiResult, Error},
    schema::*,
    service::{
        self,
        auth::{
            token::generate_worker_token, user_auth_middleware, worker_auth_middleware, AuthUser,
            AuthWorker,
        },
    },
};

pub fn workers_router(st: InfraPool) -> Router<InfraPool> {
    let worker_auth_router = Router::new()
        .route("/", delete(worker_unregister))
        .route("/heartbeat", post(heartbeat))
        .route("/tasks", post(worker_report_task).get(worker_fetch_task))
        .route("/tasks/{uuid}", get(worker_query_task))
        .route(
            "/tasks/{uuid}/artifacts/{content_type}",
            get(download_artifact),
        )
        .route(
            "/tasks/{uuid}/attachments/{*key}",
            get(worker_download_attachment),
        )
        .route_layer(middleware::from_fn_with_state(
            st.clone(),
            worker_auth_middleware,
        ))
        .with_state(st.clone());
    let user_auth_router = Router::new()
        .route(
            "/{uuid}",
            get(user_query_worker).delete(user_shutdown_worker),
        )
        .route("/{uuid}/tags", put(user_replace_worker_tags))
        .route("/{uuid}/labels", put(user_replace_worker_labels))
        .route("/{uuid}/groups/remove", delete(user_remove_worker_groups))
        .route(
            "/{uuid}/groups",
            put(user_update_worker_groups).delete(user_remove_worker_groups_params),
        )
        .route("/", post(user_register_worker))
        .route("/query", post(user_query_workers))
        .route("/shutdown", post(user_shutdown_workers))
        .route("/shutdown/list", post(user_shutdown_workers_by_uuids))
        .route_layer(middleware::from_fn_with_state(
            st.clone(),
            user_auth_middleware,
        ))
        .with_state(st.clone());
    Router::new()
        .merge(worker_auth_router)
        .merge(user_auth_router)
}

pub async fn user_register_worker(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Json(req): Json<RegisterWorkerReq>,
) -> ApiResult<Json<RegisterWorkerResp>> {
    let uuid = service::worker::register_worker(
        u.id,
        Vec::from_iter(req.tags),
        Vec::from_iter(req.labels),
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

pub async fn worker_unregister(
    Extension(w): Extension<AuthWorker>,
    State(pool): State<InfraPool>,
) -> ApiResult<Json<()>> {
    match service::worker::unregister_worker(w.id, &pool).await {
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
    match service::worker::heartbeat(w.id, &pool).await {
        Ok(_) => Ok(Json(())),
        Err(Error::ApiError(e)) => Err(e),
        Err(e) => {
            tracing::error!("{}", e);
            Err(ApiError::InternalServerError)
        }
    }
}

pub async fn worker_fetch_task(
    Extension(w): Extension<AuthWorker>,
    State(pool): State<InfraPool>,
) -> ApiResult<Json<Option<WorkerTaskResp>>> {
    // Add timeout to prevent long-running requests from blocking graceful shutdown
    let timeout = std::time::Duration::from_secs(25);
    match tokio::time::timeout(timeout, service::worker::fetch_task(w.id, &pool)).await {
        Ok(Ok(t)) => Ok(Json(t)),
        Ok(Err(Error::ApiError(e))) => Err(e),
        Ok(Err(e)) => {
            tracing::error!("{}", e);
            Err(ApiError::InternalServerError)
        }
        Err(_) => {
            tracing::warn!(
                "Worker {} fetch_task timed out after {} seconds",
                w.id,
                timeout.as_secs()
            );
            // Return None to let the worker retry later
            Ok(Json(None))
        }
    }
}

pub async fn worker_report_task(
    Extension(w): Extension<AuthWorker>,
    State(pool): State<InfraPool>,
    Json(ReportTaskReq { id, op }): Json<ReportTaskReq>,
) -> ApiResult<Json<ReportTaskResp>> {
    match service::worker::report_task(w.id, w.uuid, id, op, &pool).await {
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
    let artifact = service::s3::download_artifact_by_uuid(&pool, uuid, content_type)
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

pub async fn worker_download_attachment(
    Extension(_): Extension<AuthWorker>,
    State(pool): State<InfraPool>,
    Path((uuid, key)): Path<(Uuid, String)>,
) -> Result<Json<RemoteResourceDownloadResp>, ApiError> {
    let attachment = service::s3::worker_download_attachment(&pool, uuid, key)
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

pub async fn worker_query_task(
    Extension(_): Extension<AuthWorker>,
    State(pool): State<InfraPool>,
    Path(uuid): Path<Uuid>,
) -> Result<Json<TaskQueryResp>, ApiError> {
    let task = service::task::get_task_by_uuid(&pool, uuid)
        .await
        .map_err(|e| match e {
            crate::error::Error::AuthError(err) => ApiError::AuthError(err),
            crate::error::Error::ApiError(e) => e,
            _ => {
                tracing::error!("{}", e);
                ApiError::InternalServerError
            }
        })?;
    Ok(Json(task))
}

pub async fn user_query_worker(
    Extension(_): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Path(uuid): Path<Uuid>,
) -> Result<Json<WorkerQueryResp>, ApiError> {
    let resp = service::worker::get_worker_by_uuid(&pool, uuid)
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

pub async fn user_shutdown_worker(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Path(uuid): Path<Uuid>,
    Query(op): Query<WorkerShutdown>,
) -> Result<(), ApiError> {
    let op = op.op.unwrap_or_default();
    tracing::debug!("Shutdown worker {} with op {:?}", uuid, op);
    service::worker::user_remove_worker_by_uuid(u.id, uuid, op, &pool)
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

pub async fn user_replace_worker_tags(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Path(uuid): Path<Uuid>,
    Json(req): Json<ReplaceWorkerTagsReq>,
) -> Result<(), ApiError> {
    service::worker::user_replace_worker_tags(u.id, uuid, req.tags, &pool)
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

pub async fn user_replace_worker_labels(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Path(uuid): Path<Uuid>,
    Json(req): Json<UpdateTaskLabelsReq>,
) -> Result<(), ApiError> {
    service::worker::user_replace_worker_labels(u.id, uuid, req.labels, &pool)
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

pub async fn user_update_worker_groups(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Path(uuid): Path<Uuid>,
    Json(req): Json<UpdateGroupWorkerRoleReq>,
) -> Result<(), ApiError> {
    service::worker::user_update_worker_groups(u.id, uuid, req.relations, &pool)
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

pub async fn user_remove_worker_groups(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Path(uuid): Path<Uuid>,
    Json(req): Json<RemoveGroupWorkerRoleReq>,
) -> Result<(), ApiError> {
    service::worker::user_remove_worker_groups(u.id, uuid, req.groups, &pool)
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

pub async fn user_remove_worker_groups_params(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Path(uuid): Path<Uuid>,
    ExtraQuery(req): ExtraQuery<RemoveGroupWorkerRoleParams>,
) -> Result<(), ApiError> {
    service::worker::user_remove_worker_groups(u.id, uuid, req.groups, &pool)
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

pub async fn user_query_workers(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Json(req): Json<WorkersQueryReq>,
) -> Result<Json<WorkersQueryResp>, ApiError> {
    let resp = service::worker::query_workers_by_filter(u.id, &pool, req)
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

pub async fn user_shutdown_workers(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Json(req): Json<WorkersShutdownByFilterReq>,
) -> Result<Json<WorkersShutdownByFilterResp>, ApiError> {
    let resp = service::worker::shutdown_workers_by_filter(u.id, &pool, req)
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

pub async fn user_shutdown_workers_by_uuids(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Json(req): Json<WorkersShutdownByUuidsReq>,
) -> Result<Json<WorkersShutdownByUuidsResp>, ApiError> {
    let resp = service::worker::shutdown_workers_by_uuids(u.id, &pool, req)
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
