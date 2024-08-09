use axum::{
    extract::{Path, Query, State},
    middleware,
    routing::{delete, post},
    Extension, Json, Router,
};
use sea_orm::DbErr;
use uuid::Uuid;

use crate::{
    config::InfraPool,
    entity::state::UserState,
    error::{ApiError, ApiResult, Error},
    schema::*,
    service::{
        self,
        auth::{admin_auth_middleware, AuthAdminUser},
        name_validator,
        worker::remove_worker_by_uuid,
    },
};

pub fn admin_router(st: InfraPool) -> Router<InfraPool> {
    Router::new()
        .route("/user", post(create_user).delete(delete_user))
        .route("/user/state", post(change_user_state))
        .route("/workers/:uuid/", delete(shutdown_worker))
        .layer(middleware::from_fn_with_state(
            st.clone(),
            admin_auth_middleware,
        ))
        .with_state(st)
}

pub async fn create_user(
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

pub async fn delete_user(
    Extension(_): Extension<AuthAdminUser>,
    State(pool): State<InfraPool>,
    Json(req): Json<DeleteUserReq>,
) -> ApiResult<Json<UserStateResp>> {
    match service::user::change_user_state(&pool.db, req.username.clone(), UserState::Deleted).await
    {
        Ok(UserState::Deleted) => Ok(Json(UserStateResp {
            state: UserState::Deleted,
        })),
        Err(Error::DbError(DbErr::RecordNotUpdated)) => Err(ApiError::NotFound(format!(
            "User or group with name {}",
            req.username
        ))),
        Err(e) => {
            tracing::error!("{}", e);
            Err(ApiError::InternalServerError)
        }
        _ => Err(ApiError::InternalServerError),
    }
}

pub async fn change_user_state(
    Extension(_): Extension<AuthAdminUser>,
    State(pool): State<InfraPool>,
    Json(req): Json<ChangeUserStateReq>,
) -> ApiResult<Json<UserStateResp>> {
    match service::user::change_user_state(&pool.db, req.username.clone(), req.state).await {
        Ok(state) => Ok(Json(UserStateResp { state })),
        Err(Error::DbError(DbErr::RecordNotUpdated)) => Err(ApiError::NotFound(format!(
            "User or group with name {}",
            req.username
        ))),
        Err(e) => {
            tracing::error!("{}", e);
            Err(ApiError::InternalServerError)
        }
    }
}

pub async fn shutdown_worker(
    Extension(_): Extension<AuthAdminUser>,
    State(pool): State<InfraPool>,
    Path(uuid): Path<Uuid>,
    Query(op): Query<WorkerShutdown>,
) -> Result<(), ApiError> {
    let op = op.op.unwrap_or_default();
    tracing::debug!("Shutdown worker {} with op {:?}", uuid, op);
    remove_worker_by_uuid(uuid, op, &pool)
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
