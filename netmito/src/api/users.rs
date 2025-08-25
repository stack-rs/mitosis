use std::net::SocketAddr;

use axum::{
    extract::{ConnectInfo, Path, State},
    middleware,
    routing::{get, post},
    Extension, Json, Router,
};

use crate::{
    config::InfraPool,
    error::ApiError,
    schema::*,
    service::{
        self,
        auth::{user_auth_middleware, AuthUser, AuthUserWithName},
    },
};

pub fn users_router(st: InfraPool) -> Router<InfraPool> {
    Router::new()
        .route("/{username}/password", post(change_password))
        .route("/groups", get(query_groups))
        .route_layer(middleware::from_fn_with_state(
            st.clone(),
            user_auth_middleware,
        ))
        .with_state(st)
}

pub async fn user_login(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    State(pool): State<InfraPool>,
    Json(req): Json<UserLoginReq>,
) -> Result<Json<UserLoginResp>, ApiError> {
    let token =
        service::auth::user_login(&pool.db, &req.username, &req.md5_password, req.retain, addr)
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

pub async fn user_auth(Extension(u): Extension<AuthUserWithName>) -> String {
    u.username
}

pub async fn change_password(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Path(username): Path<String>,
    Json(req): Json<UserChangePasswordReq>,
) -> Result<Json<UserChangePasswordResp>, ApiError> {
    let token = service::auth::user_change_password(&pool.db, u.id, addr, username, req)
        .await
        .map_err(|e| match e {
            crate::error::Error::AuthError(err) => ApiError::AuthError(err),
            crate::error::Error::ApiError(e) => e,
            _ => {
                tracing::error!("{}", e);
                ApiError::InternalServerError
            }
        })?;
    Ok(Json(UserChangePasswordResp { token }))
}

pub async fn query_groups(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
) -> Result<Json<GroupsQueryResp>, ApiError> {
    let groups = service::group::query_user_groups(u.id, &pool)
        .await
        .map_err(|e| match e {
            crate::error::Error::AuthError(err) => ApiError::AuthError(err),
            crate::error::Error::ApiError(e) => e,
            _ => {
                tracing::error!("{}", e);
                ApiError::InternalServerError
            }
        })?;
    Ok(Json(GroupsQueryResp { groups }))
}
