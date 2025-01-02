use std::net::SocketAddr;

use axum::{
    extract::{ConnectInfo, Path, Query, State},
    http::StatusCode,
    middleware,
    routing::{get, post, put},
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
        group::query_user_groups,
        s3::{
            get_artifact, query_attachment_list, user_get_attachment, user_get_attachment_db,
            user_upload_artifact, user_upload_attachment,
        },
        task::{
            get_task, query_task_list, user_cancel_task, user_change_task, user_change_task_labels,
            user_submit_task,
        },
        worker::{
            get_worker, query_worker_list, user_remove_worker_by_uuid, user_remove_worker_groups,
            user_replace_worker_tags, user_update_worker_groups,
        },
    },
};

pub fn user_router(st: InfraPool) -> Router<InfraPool> {
    Router::new()
        .route("/auth", get(auth_user))
        .route("/tasks", post(submit_task))
        .route(
            "/tasks/{uuid}",
            get(query_task).put(change_task).delete(cancel_task),
        )
        .route("/tasks/{uuid}/labels", put(change_task_labels))
        .route("/artifacts/{uuid}/{content_type}", get(download_artifact))
        .route("/artifacts", post(upload_artifact))
        .route("/attachments/{group_name}/{*key}", get(download_attachment))
        .route(
            "/attachments/meta/{group_name}/{*key}",
            get(get_attachment_metadata),
        )
        .route("/attachments", post(upload_attachment))
        .route(
            "/workers/{uuid}/",
            get(query_worker).delete(shutdown_worker),
        )
        .route("/workers/{uuid}/tags", put(replace_worker_tags))
        .route(
            "/workers/{uuid}/groups",
            put(update_worker_groups).delete(remove_worker_groups),
        )
        .route("/redis", get(query_redis_connection_info))
        .route("/filters/tasks", post(query_tasks))
        .route("/filters/attachments", post(query_attachments))
        .route("/filters/workers", post(query_workers))
        .route("/groups", get(query_groups))
        .route_layer(middleware::from_fn_with_state(
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

pub async fn change_task(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Path(uuid): Path<Uuid>,
    Json(req): Json<ChangeTaskReq>,
) -> Result<(), ApiError> {
    user_change_task(&pool, u.id, uuid, req)
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
    user_change_task_labels(&pool, u.id, uuid, req)
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
    user_cancel_task(&pool, u.id, uuid)
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

pub async fn upload_artifact(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Json(req): Json<UploadArtifactReq>,
) -> Result<Json<UploadArtifactResp>, ApiError> {
    let url = user_upload_artifact(&pool, u.id, req.uuid, req.content_type, req.content_length)
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
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Json(req): Json<UploadAttachmentReq>,
) -> Result<Json<UploadAttachmentResp>, ApiError> {
    let url = user_upload_attachment(u.id, &pool, req.group_name, req.key, req.content_length)
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
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Path((group_name, key)): Path<(String, String)>,
) -> Result<Json<RemoteResourceDownloadResp>, ApiError> {
    let attachment = user_get_attachment(&pool, u.id, group_name, key)
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

pub async fn get_attachment_metadata(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Path((group_name, key)): Path<(String, String)>,
) -> Result<Json<AttachmentMetadata>, ApiError> {
    let attachment = user_get_attachment_db(&pool, u.id, group_name, key)
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

pub async fn query_attachments(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Json(req): Json<AttachmentsQueryReq>,
) -> Result<Json<Vec<AttachmentQueryInfo>>, ApiError> {
    let attachments = query_attachment_list(u.id, &pool, req)
        .await
        .map_err(|e| match e {
            crate::error::Error::AuthError(err) => ApiError::AuthError(err),
            crate::error::Error::ApiError(e) => e,
            _ => {
                tracing::error!("{}", e);
                ApiError::InternalServerError
            }
        })?;
    Ok(Json(attachments))
}

pub async fn query_redis_connection_info(
    Extension(_): Extension<AuthUser>,
) -> Result<Json<RedisConnectionInfo>, ApiError> {
    let url = REDIS_CONNECTION_INFO.get().map(|info| info.client_url());
    Ok(Json(RedisConnectionInfo { url }))
}

pub async fn query_worker(
    Extension(_): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Path(uuid): Path<Uuid>,
) -> Result<Json<WorkerQueryResp>, ApiError> {
    let resp = get_worker(&pool, uuid).await.map_err(|e| match e {
        crate::error::Error::AuthError(err) => ApiError::AuthError(err),
        crate::error::Error::ApiError(e) => e,
        _ => {
            tracing::error!("{}", e);
            ApiError::InternalServerError
        }
    })?;
    Ok(Json(resp))
}

pub async fn query_workers(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Json(req): Json<WorkersQueryReq>,
) -> Result<Json<WorkersQueryResp>, ApiError> {
    let resp = query_worker_list(u.id, &pool, req)
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

pub async fn shutdown_worker(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Path(uuid): Path<Uuid>,
    Query(op): Query<WorkerShutdown>,
) -> Result<(), ApiError> {
    let op = op.op.unwrap_or_default();
    tracing::debug!("Shutdown worker {} with op {:?}", uuid, op);
    user_remove_worker_by_uuid(u.id, uuid, op, &pool)
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

pub async fn replace_worker_tags(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Path(uuid): Path<Uuid>,
    Json(req): Json<ReplaceWorkerTagsReq>,
) -> Result<(), ApiError> {
    user_replace_worker_tags(u.id, uuid, req.tags, &pool)
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

pub async fn update_worker_groups(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Path(uuid): Path<Uuid>,
    Json(req): Json<UpdateGroupWorkerRoleReq>,
) -> Result<(), ApiError> {
    user_update_worker_groups(u.id, uuid, req.relations, &pool)
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

pub async fn remove_worker_groups(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Path(uuid): Path<Uuid>,
    Json(req): Json<RemoveGroupWorkerRoleReq>,
) -> Result<(), ApiError> {
    user_remove_worker_groups(u.id, uuid, req.groups, &pool)
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

pub async fn query_groups(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
) -> Result<Json<GroupsQueryResp>, ApiError> {
    let groups = query_user_groups(u.id, &pool).await.map_err(|e| match e {
        crate::error::Error::AuthError(err) => ApiError::AuthError(err),
        crate::error::Error::ApiError(e) => e,
        _ => {
            tracing::error!("{}", e);
            ApiError::InternalServerError
        }
    })?;
    Ok(Json(GroupsQueryResp { groups }))
}
