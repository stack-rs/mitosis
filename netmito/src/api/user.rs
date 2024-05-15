use std::net::SocketAddr;

use axum::{
    extract::{ConnectInfo, State},
    http::StatusCode,
    Extension, Json,
};

use crate::{
    config::InfraPool,
    error::ApiError,
    schema::*,
    service::auth::{user_login, AuthUser},
};

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
