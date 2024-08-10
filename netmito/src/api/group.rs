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

pub fn group_router(st: InfraPool) -> Router<InfraPool> {
    Router::new()
        .route("/", post(create_group))
        .route("/:group", get(get_group))
        .route(
            "/:group/users",
            put(update_user_group).delete(remove_user_group),
        )
        .layer(middleware::from_fn_with_state(
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
    service::group::create_group(&pool.db, u.id, req.group_name.clone())
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
    Path(group): Path<String>,
) -> ApiResult<Json<GroupQueryInfo>> {
    let info = service::group::get_group(u.id, group, &pool)
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
    Path(group): Path<String>,
    Json(req): Json<UpdateUserGroupRoleReq>,
) -> ApiResult<()> {
    service::group::update_user_group_role(u.id, group, req.relations, &pool)
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
    Path(group): Path<String>,
    Json(req): Json<RemoveUserGroupRoleReq>,
) -> ApiResult<()> {
    service::group::remove_user_group_role(u.id, group, req.users, &pool)
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
