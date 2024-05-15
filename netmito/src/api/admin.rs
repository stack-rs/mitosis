use axum::{extract::State, Extension, Json};
use sea_orm::DbErr;

use crate::{
    config::InfraPool,
    entity::state::UserState,
    error::{ApiError, ApiResult, Error},
    schema::*,
    service::{self, auth::AuthAdminUser, name_validator},
};

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
        Err(Error::DbError(DbErr::RecordNotUpdated)) => Err(ApiError::NotFound(req.username)),
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
        Err(Error::DbError(DbErr::RecordNotUpdated)) => Err(ApiError::NotFound(req.username)),
        Err(e) => {
            tracing::error!("{}", e);
            Err(ApiError::InternalServerError)
        }
    }
}
