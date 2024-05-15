use axum::{extract::State, Extension, Json};
use sea_orm::DbErr;

use crate::{
    config::InfraPool,
    error::{ApiError, ApiResult},
    schema::*,
    service::{self, auth::AuthUser, name_validator},
};

pub async fn create_group(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Json(req): Json<CreateGroupReq>,
) -> ApiResult<()> {
    if !name_validator(&req.group_name) {
        Err(ApiError::InvalidRequest("Invalid group name".to_string()))
    } else {
        match service::group::create_group(&pool.db, u.id, req.group_name.clone()).await {
            Ok(_) => Ok(()),
            Err(e) => match e {
                crate::error::Error::ApiError(e) => Err(e),
                crate::error::Error::DbError(DbErr::RecordNotInserted) => {
                    Err(ApiError::AlreadyExists(req.group_name))
                }
                _ => {
                    tracing::error!("{}", e);
                    Err(ApiError::InternalServerError)
                }
            },
        }
    }
}
