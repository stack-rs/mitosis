use axum::{
    extract::{Path, Query, State},
    middleware,
    routing::{delete, post},
    Extension, Json, Router,
};
use sea_orm::DbErr;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    config::{self, InfraPool},
    entity::{content::ArtifactContentType, state::UserState},
    error::{ApiError, ApiResult, Error},
    schema::*,
    service::{
        self,
        auth::{admin_auth_middleware, AuthAdminUser},
        name_validator,
    },
};

pub fn admin_router(st: InfraPool, cancel_token: CancellationToken) -> Router<InfraPool> {
    Router::new()
        .route("/users", post(admin_create_user))
        .route("/users/{username}", delete(admin_delete_user))
        .route(
            "/users/{username}/password",
            post(admin_change_user_password),
        )
        .route(
            "/users/{username}/group-quota",
            post(admin_change_user_group_quota),
        )
        .route("/users/{username}/state", post(admin_change_user_state))
        .route(
            "/groups/{group_name}/storage-quota",
            post(admin_change_group_storage_quota),
        )
        .route("/workers/{uuid}", delete(admin_shutdown_worker))
        .route(
            "/groups/{group_name}/attachments/{*key}",
            delete(admin_delete_attachment),
        )
        .route(
            "/tasks/{uuid}/artifacts/{content_type}",
            delete(admin_delete_artifact),
        )
        .route(
            "/shutdown",
            post(admin_shutdown_coordinator).with_state(cancel_token),
        )
        .route_layer(middleware::from_fn_with_state(
            st.clone(),
            admin_auth_middleware,
        ))
        .with_state(st)
}

pub async fn admin_create_user(
    Extension(_): Extension<AuthAdminUser>,
    State(pool): State<InfraPool>,
    Json(CreateUserReq {
        username,
        md5_password,
        admin,
    }): Json<CreateUserReq>,
) -> ApiResult<()> {
    if !name_validator(&username) {
        Err(ApiError::InvalidRequest("Invalid username".to_string()))
    } else {
        match service::user::create_user(&pool.db, username.clone(), md5_password, admin).await {
            Ok(_) => Ok(()),
            Err(e) => match e {
                crate::error::Error::ApiError(e) => Err(e),
                crate::error::Error::DbError(DbErr::RecordNotInserted) => {
                    Err(ApiError::AlreadyExists(username))
                }
                _ => {
                    tracing::error!("{}", e);
                    Err(ApiError::InternalServerError)
                }
            },
        }
    }
}

pub async fn admin_delete_user(
    Extension(_): Extension<AuthAdminUser>,
    State(pool): State<InfraPool>,
    Path(username): Path<String>,
) -> ApiResult<Json<UserStateResp>> {
    match service::user::change_user_state(&pool.db, username.clone(), UserState::Deleted).await {
        Ok(UserState::Deleted) => Ok(Json(UserStateResp {
            state: UserState::Deleted,
        })),
        Err(Error::DbError(DbErr::RecordNotUpdated)) => Err(ApiError::NotFound(format!(
            "User or group with name {}",
            username
        ))),
        Err(e) => {
            tracing::error!("{}", e);
            Err(ApiError::InternalServerError)
        }
        _ => Err(ApiError::InternalServerError),
    }
}

pub async fn admin_delete_attachment(
    Extension(_): Extension<AuthAdminUser>,
    State(pool): State<InfraPool>,
    Path((group_name, key)): Path<(String, String)>,
) -> Result<(), ApiError> {
    service::s3::admin_delete_attachment(&pool, group_name, key)
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

pub async fn admin_delete_artifact(
    Extension(_): Extension<AuthAdminUser>,
    State(pool): State<InfraPool>,
    Path((uuid, content_type)): Path<(Uuid, ArtifactContentType)>,
) -> Result<(), ApiError> {
    service::s3::admin_delete_artifact(&pool, uuid, content_type)
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

pub async fn admin_change_user_password(
    Extension(_): Extension<AuthAdminUser>,
    State(pool): State<InfraPool>,
    Path(username): Path<String>,
    Json(req): Json<AdminChangePasswordReq>,
) -> ApiResult<()> {
    service::auth::admin_change_password(&pool.db, username, req.new_md5_password)
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

pub async fn admin_change_user_state(
    Extension(_): Extension<AuthAdminUser>,
    State(pool): State<InfraPool>,
    Path(username): Path<String>,
    Json(req): Json<ChangeUserStateReq>,
) -> ApiResult<Json<UserStateResp>> {
    match service::user::change_user_state(&pool.db, username.clone(), req.state).await {
        Ok(state) => Ok(Json(UserStateResp { state })),
        Err(Error::DbError(DbErr::RecordNotUpdated)) => Err(ApiError::NotFound(format!(
            "User or group with name {username}"
        ))),
        Err(e) => {
            tracing::error!("{}", e);
            Err(ApiError::InternalServerError)
        }
    }
}

pub async fn admin_change_group_storage_quota(
    Extension(_): Extension<AuthAdminUser>,
    State(pool): State<InfraPool>,
    Path(group_name): Path<String>,
    Json(req): Json<ChangeGroupStorageQuotaReq>,
) -> ApiResult<Json<GroupStorageQuotaResp>> {
    let storage_quota =
        service::group::change_group_storage_quota(&pool, group_name, req.storage_quota)
            .await
            .map_err(|e| match e {
                crate::error::Error::AuthError(err) => ApiError::AuthError(err),
                crate::error::Error::ApiError(e) => e,
                _ => {
                    tracing::error!("{}", e);
                    ApiError::InternalServerError
                }
            })?;
    Ok(Json(GroupStorageQuotaResp { storage_quota }))
}

pub async fn admin_change_user_group_quota(
    Extension(_): Extension<AuthAdminUser>,
    State(pool): State<InfraPool>,
    Path(username): Path<String>,
    Json(req): Json<ChangeUserGroupQuota>,
) -> ApiResult<Json<UserGroupQuotaResp>> {
    let group_quota = service::user::change_user_group_quota(&pool, username, req.group_quota)
        .await
        .map_err(|e| match e {
            crate::error::Error::AuthError(err) => ApiError::AuthError(err),
            crate::error::Error::ApiError(e) => e,
            _ => {
                tracing::error!("{}", e);
                ApiError::InternalServerError
            }
        })?;
    Ok(Json(UserGroupQuotaResp { group_quota }))
}

pub async fn admin_shutdown_worker(
    Extension(_): Extension<AuthAdminUser>,
    State(pool): State<InfraPool>,
    Path(uuid): Path<Uuid>,
    Query(op): Query<WorkerShutdown>,
) -> Result<(), ApiError> {
    let op = op.op.unwrap_or_default();
    tracing::debug!("Shutdown worker {} with op {:?}", uuid, op);
    service::worker::remove_worker_by_uuid(uuid, op, &pool)
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

pub async fn admin_shutdown_coordinator(
    Extension(_): Extension<AuthAdminUser>,
    State(cancel_token): State<CancellationToken>,
    Json(req): Json<ShutdownReq>,
) -> Result<(), ApiError> {
    tracing::info!("Coordinator is requested to shutdown");
    if let Some(secret) = config::SHUTDOWN_SECRET.get() {
        if *secret == req.secret {
            tracing::info!("Coordinator shutdown initiated");
            cancel_token.cancel();
            Ok(())
        } else {
            Err(ApiError::AuthError(
                crate::error::AuthError::PermissionDenied,
            ))
        }
    } else {
        Err(ApiError::InternalServerError)
    }
}
