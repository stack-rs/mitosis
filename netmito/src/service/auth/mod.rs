pub mod token;

use argon2::{Argon2, PasswordHash, PasswordVerifier};
use axum::{body::Body, extract::State, http::Request, middleware::Next, response::IntoResponse};
use axum_extra::{
    headers::{authorization::Bearer, Authorization},
    TypedHeader,
};
use rand::{rngs::StdRng, RngCore, SeedableRng};
use sea_orm::entity::prelude::*;

use crate::{config::InfraPool, entity::users as User, error::AuthError};
use token::{generate_token, verify_token};

#[derive(Debug, Clone)]
pub struct AuthUser {
    pub id: i64,
}

#[derive(Debug, Clone)]
pub struct AdminUser {
    pub id: i64,
}

pub async fn user_login(
    db: &DatabaseConnection,
    username: String,
    md5_password: [u8; 16],
) -> crate::error::Result<String> {
    match User::Entity::find()
        .filter(User::Column::Username.eq(&username))
        .one(db)
        .await?
    {
        Some(user) => {
            let parsed_hash = PasswordHash::new(&user.encrypted_password)?;
            if Argon2::default()
                .verify_password(&md5_password, &parsed_hash)
                .is_ok()
            {
                let sign = StdRng::from_entropy().next_u32() as i64;
                Ok(generate_token(&username, sign)?)
            } else {
                Err(AuthError::WrongCredentials.into())
            }
        }
        None => Err(AuthError::WrongCredentials.into()),
    }
}

pub async fn user_auth_middleware(
    State(pool): State<InfraPool>,
    TypedHeader(Authorization(bearer)): TypedHeader<Authorization<Bearer>>,
    mut req: Request<Body>,
    next: Next,
) -> Result<impl IntoResponse, AuthError> {
    let auth_user = user_auth(&pool.db, &bearer).await?;
    req.extensions_mut().insert(auth_user);
    Ok(next.run(req).await)
}

async fn user_auth(db: &DatabaseConnection, bearer: &Bearer) -> Result<AuthUser, AuthError> {
    let token = bearer.token();
    let claims = verify_token(token).map_err(|_| AuthError::InvalidToken)?;

    let user = User::Entity::find()
        .filter(User::Column::Username.eq(claims.sub))
        .one(db)
        .await
        .map_err(|_| AuthError::WrongCredentials)?
        .ok_or(AuthError::WrongCredentials)?;

    Ok(AuthUser { id: user.id })
}

pub async fn admin_auth_middleware(
    State(pool): State<InfraPool>,
    TypedHeader(Authorization(bearer)): TypedHeader<Authorization<Bearer>>,
    mut req: Request<Body>,
    next: Next,
) -> Result<impl IntoResponse, AuthError> {
    let admin_user = admin_auth(&pool.db, &bearer).await?;
    req.extensions_mut().insert(admin_user);
    Ok(next.run(req).await)
}

async fn admin_auth(db: &DatabaseConnection, bearer: &Bearer) -> Result<AdminUser, AuthError> {
    let token = bearer.token();
    let claims = verify_token(token).map_err(|_| AuthError::InvalidToken)?;

    let user = User::Entity::find()
        .filter(User::Column::Username.eq(claims.sub))
        .one(db)
        .await
        .map_err(|_| AuthError::WrongCredentials)?
        .ok_or(AuthError::WrongCredentials)?;
    if user.admin {
        Ok(AdminUser { id: user.id })
    } else {
        Err(AuthError::PermissionDenied)
    }
}
