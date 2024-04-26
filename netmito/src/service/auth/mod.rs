pub mod token;

use axum::{body::Body, extract::State, http::Request, middleware::Next, response::IntoResponse};
use axum_extra::{
    headers::{authorization::Bearer, Authorization},
    TypedHeader,
};
use sea_orm::entity::prelude::*;

use crate::{config::InfraPool, entity::users as User, error::AuthError};
use token::verify_token;

#[derive(Debug, Clone)]
pub struct AuthUser {
    pub id: i64,
}

pub async fn user_auth_middleware(
    State(pool): State<InfraPool>,
    TypedHeader(Authorization(bearer)): TypedHeader<Authorization<Bearer>>,
    mut req: Request<Body>,
    next: Next,
) -> Result<impl IntoResponse, AuthError> {
    let auth_user = inner_auth(&pool, &bearer).await?;
    req.extensions_mut().insert(auth_user);
    Ok(next.run(req).await)
}

async fn inner_auth(pool: &InfraPool, bearer: &Bearer) -> Result<AuthUser, AuthError> {
    let token = bearer.token();
    let claims = verify_token(token).map_err(|_| AuthError::InvalidToken)?;

    let user = User::Entity::find()
        .filter(User::Column::Username.eq(claims.sub))
        .one(&pool.db)
        .await
        .map_err(|_| AuthError::WrongCredentials)?
        .ok_or(AuthError::WrongCredentials)?;

    Ok(AuthUser { id: user.id })
}
