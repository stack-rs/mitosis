use axum::{
    extract::{Path, State},
    middleware,
    routing::{delete, get, post, put},
    Extension, Json, Router,
};
use uuid::Uuid;

use crate::{
    config::InfraPool,
    entity::content::ArtifactContentType,
    error::ApiError,
    schema::*,
    service::{
        self,
        auth::{user_auth_middleware, AuthUser},
    },
};

pub fn tasks_router(st: InfraPool) -> Router<InfraPool> {
    Router::new()
        .route("/", post(submit_task))
        .route(
            "/{uuid}",
            get(query_task).put(change_task).delete(cancel_task),
        )
        .route("/{uuid}/labels", put(change_task_labels))
        .route(
            "/{uuid}/download/artifacts/{content_type}",
            get(download_artifact),
        )
        .route("/{uuid}/artifacts/{content_type}", delete(delete_artifact))
        .route("/{uuid}/artifacts", post(upload_artifact))
        .route("/query", post(query_tasks))
        .route("/cancel", post(cancel_tasks))
        .route(
            "/download/artifacts/filter",
            post(batch_download_artifacts_by_filter),
        )
        .route(
            "/download/artifacts/list",
            post(batch_download_artifacts_by_uuids),
        )
        .route_layer(middleware::from_fn_with_state(
            st.clone(),
            user_auth_middleware,
        ))
        .with_state(st)
}

pub async fn submit_task(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Json(req): Json<SubmitTaskReq>,
) -> Result<Json<SubmitTaskResp>, ApiError> {
    let resp = service::task::user_submit_task(&pool, u.id, req)
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

pub async fn change_task(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Path(uuid): Path<Uuid>,
    Json(req): Json<ChangeTaskReq>,
) -> Result<(), ApiError> {
    service::task::user_change_task(&pool, u.id, uuid, req)
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

pub async fn change_task_labels(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Path(uuid): Path<Uuid>,
    Json(req): Json<UpdateTaskLabelsReq>,
) -> Result<(), ApiError> {
    service::task::user_change_task_labels(&pool, u.id, uuid, req)
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

pub async fn cancel_task(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Path(uuid): Path<Uuid>,
) -> Result<(), ApiError> {
    service::task::user_cancel_task(&pool, u.id, uuid)
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

pub async fn query_task(
    Extension(_): Extension<AuthUser>,
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

pub async fn query_tasks(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Json(req): Json<TasksQueryReq>,
) -> Result<Json<TasksQueryResp>, ApiError> {
    let tasks = service::task::query_tasks_by_filter(u.id, &pool, req)
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

pub async fn cancel_tasks(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Json(req): Json<TasksCancelByFilterReq>,
) -> Result<Json<TasksCancelByFilterResp>, ApiError> {
    let resp = service::task::cancel_tasks_by_filter(u.id, &pool, req)
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

pub async fn upload_artifact(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Path(uuid): Path<Uuid>,
    Json(req): Json<UploadArtifactReq>,
) -> Result<Json<UploadArtifactResp>, ApiError> {
    let url =
        service::s3::user_upload_artifact(&pool, u.id, uuid, req.content_type, req.content_length)
            .await
            .map_err(|e| match e {
                crate::error::Error::AuthError(err) => ApiError::AuthError(err),
                crate::error::Error::ApiError(e) => e,
                _ => {
                    tracing::error!("{}", e);
                    ApiError::InternalServerError
                }
            })?;
    Ok(Json(UploadArtifactResp { url }))
}

pub async fn download_artifact(
    Extension(_): Extension<AuthUser>,
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

pub async fn delete_artifact(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Path((uuid, content_type)): Path<(Uuid, ArtifactContentType)>,
) -> Result<(), ApiError> {
    service::s3::user_delete_artifact(&pool, u.id, uuid, content_type)
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

pub async fn batch_download_artifacts_by_filter(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Json(req): Json<ArtifactsDownloadByFilterReq>,
) -> Result<Json<ArtifactsDownloadListResp>, ApiError> {
    let resp = service::s3::batch_download_artifacts_by_filter(u.id, &pool, req)
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

pub async fn batch_download_artifacts_by_uuids(
    Extension(_): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Json(req): Json<ArtifactsDownloadByUuidsReq>,
) -> Result<Json<ArtifactsDownloadListResp>, ApiError> {
    let resp = service::s3::batch_download_artifacts_by_uuids(&pool, req)
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
