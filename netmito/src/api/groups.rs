use axum::{
    extract::{Path, State},
    middleware,
    routing::{get, post, put},
    Extension, Json, Router,
};

use crate::{
    config::InfraPool,
    error::{ApiError, ApiResult},
    schema::*,
    service::{
        self,
        auth::{user_auth_middleware, AuthUser},
    },
};

pub fn groups_router(st: InfraPool) -> Router<InfraPool> {
    Router::new()
        .route("/", post(create_group))
        .route("/{group_name}", get(get_group))
        .route(
            "/{group_name}/users",
            put(update_user_group).delete(remove_user_group),
        )
        .route(
            "/{group_name}/download/attachments/{*key}",
            get(download_attachment),
        )
        .route(
            "/{group_name}/attachments/{*key}",
            get(query_single_attachment).delete(delete_attachment),
        )
        .route("/{group_name}/attachments", post(upload_attachment))
        .route("/{group_name}/attachments/query", post(query_attachments))
        .route_layer(middleware::from_fn_with_state(
            st.clone(),
            user_auth_middleware,
        ))
        .with_state(st)
}

pub async fn create_group(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Json(req): Json<CreateGroupReq>,
) -> ApiResult<()> {
    service::group::user_create_group(&pool.db, u.id, req.group_name)
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

pub async fn get_group(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Path(group_name): Path<String>,
) -> ApiResult<Json<GroupQueryInfo>> {
    let info = service::group::user_get_group_by_name(u.id, group_name, &pool)
        .await
        .map_err(|e| match e {
            crate::error::Error::AuthError(err) => ApiError::AuthError(err),
            crate::error::Error::ApiError(e) => e,
            _ => {
                tracing::error!("{}", e);
                ApiError::InternalServerError
            }
        })?;
    Ok(Json(info))
}

pub async fn update_user_group(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Path(group_name): Path<String>,
    Json(req): Json<UpdateUserGroupRoleReq>,
) -> ApiResult<()> {
    service::group::update_user_group_role(u.id, group_name, req.relations, &pool)
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

pub async fn remove_user_group(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Path(group_name): Path<String>,
    Json(req): Json<RemoveUserGroupRoleReq>,
) -> ApiResult<()> {
    service::group::remove_user_group_role(u.id, group_name, req.users, &pool)
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

pub async fn upload_attachment(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Path(group_name): Path<String>,
    Json(req): Json<UploadAttachmentReq>,
) -> Result<Json<UploadAttachmentResp>, ApiError> {
    let url =
        service::s3::user_upload_attachment(u.id, &pool, group_name, req.key, req.content_length)
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
    let attachment = service::s3::user_get_attachment(&pool, u.id, group_name, key)
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

pub async fn delete_attachment(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Path((group_name, key)): Path<(String, String)>,
) -> Result<(), ApiError> {
    service::s3::user_delete_attachment(&pool, u.id, group_name, key)
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

pub async fn query_single_attachment(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Path((group_name, key)): Path<(String, String)>,
) -> Result<Json<AttachmentMetadata>, ApiError> {
    let attachment = service::s3::user_query_attachment(&pool, u.id, group_name, key)
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
    Path(group_name): Path<String>,
    Json(req): Json<AttachmentsQueryReq>,
) -> Result<Json<AttachmentsQueryResp>, ApiError> {
    let attachments = service::s3::query_attachments_by_filter(u.id, &pool, group_name, req)
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
