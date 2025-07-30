pub mod cred;
pub mod token;

use std::net::SocketAddr;

use argon2::{
    password_hash::{rand_core::OsRng, PasswordHasher, SaltString},
    Argon2, PasswordHash, PasswordVerifier,
};
use axum::{body::Body, extract::State, http::Request, middleware::Next, response::IntoResponse};
use axum_extra::{
    headers::{authorization::Bearer, Authorization},
    TypedHeader,
};
use rand::{rngs::StdRng, RngCore, SeedableRng};
use sea_orm::{entity::prelude::*, Set};

use crate::{
    config::InfraPool,
    entity::{state::UserState, users as User, workers as Worker},
    error::{ApiError, AuthError},
    schema::{ChangePasswordReq, UserChangePasswordReq},
};
use token::{generate_token, verify_token};

#[derive(Debug, Clone)]
pub struct AuthUser {
    pub id: i64,
}

#[derive(Debug, Clone)]
pub struct AuthAdminUser {
    pub id: i64,
}

#[derive(Debug, Clone)]
pub struct AuthWorker {
    pub id: i64,
}

pub async fn user_login(
    db: &DatabaseConnection,
    username: &str,
    md5_password: &[u8; 16],
    ip: SocketAddr,
) -> crate::error::Result<String> {
    match User::Entity::find()
        .filter(User::Column::Username.eq(username))
        .one(db)
        .await?
    {
        Some(user) => {
            if user.state != UserState::Active {
                return Err(AuthError::PermissionDenied.into());
            }
            let parsed_hash = PasswordHash::new(&user.encrypted_password)?;
            if Argon2::default()
                .verify_password(md5_password, &parsed_hash)
                .is_ok()
            {
                let sign = StdRng::from_os_rng().next_u32() as i64;
                let token = generate_token(username, sign)?;
                let now = TimeDateTimeWithTimeZone::now_utc();
                let active_user = User::ActiveModel {
                    id: Set(user.id),
                    auth_signature: Set(Some(sign)),
                    current_sign_in_at: Set(Some(now)),
                    last_sign_in_at: Set(user.current_sign_in_at),
                    current_sign_in_ip: Set(Some(ip.ip().to_string())),
                    last_sign_in_ip: Set(user.current_sign_in_ip),
                    updated_at: Set(now),
                    ..Default::default()
                };
                active_user.update(db).await?;
                tracing::debug!("User {} logged in", username);
                Ok(token)
            } else {
                tracing::debug!("Wrong password for user {}", username);
                Err(AuthError::WrongCredentials.into())
            }
        }
        None => {
            tracing::debug!("User {} not found", username);
            Err(AuthError::WrongCredentials.into())
        }
    }
}

pub async fn change_password(
    db: &DatabaseConnection,
    user_id: i64,
    ip: SocketAddr,
    UserChangePasswordReq {
        username,
        old_md5_password: orig_md5_password,
        new_md5_password,
    }: UserChangePasswordReq,
) -> crate::error::Result<String> {
    let user = User::Entity::find_by_id(user_id)
        .one(db)
        .await?
        .ok_or(ApiError::NotFound("User not found".to_string()))?;
    if user.username != username {
        return Err(AuthError::WrongCredentials.into());
    }
    if user.state != UserState::Active {
        return Err(AuthError::PermissionDenied.into());
    }
    let parsed_hash = PasswordHash::new(&user.encrypted_password)?;
    if Argon2::default()
        .verify_password(&orig_md5_password, &parsed_hash)
        .is_ok()
    {
        let salt = SaltString::generate(&mut OsRng);
        let argon2 = Argon2::default();
        let password_hash = argon2.hash_password(&new_md5_password, &salt)?.to_string();
        let sign = StdRng::from_os_rng().next_u32() as i64;
        let token = generate_token(&username, sign)?;
        let now = TimeDateTimeWithTimeZone::now_utc();
        let active_user = User::ActiveModel {
            id: Set(user.id),
            encrypted_password: Set(password_hash),
            auth_signature: Set(Some(sign)),
            current_sign_in_at: Set(Some(now)),
            last_sign_in_at: Set(user.current_sign_in_at),
            current_sign_in_ip: Set(Some(ip.ip().to_string())),
            last_sign_in_ip: Set(user.current_sign_in_ip),
            updated_at: Set(now),
            ..Default::default()
        };
        tracing::debug!("User {} change password and logged in", username);
        active_user.update(db).await?;
        Ok(token)
    } else {
        tracing::debug!("Wrong password for user {}", username);
        Err(AuthError::WrongCredentials.into())
    }
}

pub async fn admin_change_password(
    db: &DatabaseConnection,
    ChangePasswordReq {
        username,
        new_md5_password,
    }: ChangePasswordReq,
) -> crate::error::Result<()> {
    let user = User::Entity::find()
        .filter(User::Column::Username.eq(&username))
        .one(db)
        .await?
        .ok_or(ApiError::NotFound("User not found".to_string()))?;
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();
    let password_hash = argon2.hash_password(&new_md5_password, &salt)?.to_string();
    let now = TimeDateTimeWithTimeZone::now_utc();
    let active_user = User::ActiveModel {
        id: Set(user.id),
        encrypted_password: Set(password_hash),
        updated_at: Set(now),
        ..Default::default()
    };
    tracing::debug!("User {} change password", username);
    active_user.update(db).await?;
    Ok(())
}

pub async fn user_auth_middleware(
    State(pool): State<InfraPool>,
    TypedHeader(Authorization(bearer)): TypedHeader<Authorization<Bearer>>,
    mut req: Request<Body>,
    next: Next,
) -> Result<impl IntoResponse, ApiError> {
    let auth_user = user_auth(&pool.db, &bearer).await?;
    req.extensions_mut().insert(auth_user);
    Ok(next.run(req).await)
}

async fn user_auth(db: &DatabaseConnection, bearer: &Bearer) -> Result<AuthUser, AuthError> {
    let token = bearer.token();
    let claims = verify_token(token).map_err(|_| AuthError::InvalidToken)?;
    let now = TimeDateTimeWithTimeZone::now_utc();
    if claims.exp < now {
        return Err(AuthError::WrongCredentials);
    }

    let user = User::Entity::find()
        .filter(User::Column::Username.eq(claims.sub))
        .one(db)
        .await
        .map_err(|_| AuthError::WrongCredentials)?
        .ok_or(AuthError::WrongCredentials)?;

    if user.state != UserState::Active {
        Err(AuthError::PermissionDenied)
    } else if user.auth_signature != Some(claims.sign) {
        Err(AuthError::WrongCredentials)
    } else {
        Ok(AuthUser { id: user.id })
    }
}

pub async fn admin_auth_middleware(
    State(pool): State<InfraPool>,
    TypedHeader(Authorization(bearer)): TypedHeader<Authorization<Bearer>>,
    mut req: Request<Body>,
    next: Next,
) -> Result<impl IntoResponse, ApiError> {
    let admin_user = admin_auth(&pool.db, &bearer).await?;
    req.extensions_mut().insert(admin_user);
    Ok(next.run(req).await)
}

async fn admin_auth(db: &DatabaseConnection, bearer: &Bearer) -> Result<AuthAdminUser, AuthError> {
    let token = bearer.token();
    let claims = verify_token(token).map_err(|_| AuthError::InvalidToken)?;
    let now = TimeDateTimeWithTimeZone::now_utc();
    if claims.exp < now {
        return Err(AuthError::WrongCredentials);
    }

    let user = User::Entity::find()
        .filter(User::Column::Username.eq(claims.sub))
        .one(db)
        .await
        .map_err(|_| AuthError::WrongCredentials)?
        .ok_or(AuthError::WrongCredentials)?;
    if user.admin {
        if user.state != UserState::Active {
            Err(AuthError::PermissionDenied)
        } else if user.auth_signature != Some(claims.sign) {
            Err(AuthError::WrongCredentials)
        } else {
            Ok(AuthAdminUser { id: user.id })
        }
    } else {
        Err(AuthError::PermissionDenied)
    }
}

pub async fn worker_auth_middleware(
    State(pool): State<InfraPool>,
    TypedHeader(Authorization(bearer)): TypedHeader<Authorization<Bearer>>,
    mut req: Request<Body>,
    next: Next,
) -> Result<impl IntoResponse, ApiError> {
    let auth_worker = worker_auth(&pool.db, &bearer).await?;
    req.extensions_mut().insert(auth_worker);
    Ok(next.run(req).await)
}

async fn worker_auth(db: &DatabaseConnection, bearer: &Bearer) -> Result<AuthWorker, AuthError> {
    let token = bearer.token();
    let claims = verify_token(token).map_err(|_| AuthError::InvalidToken)?;
    let uuid = Uuid::parse_str(&claims.sub).map_err(|_| AuthError::InvalidToken)?;

    let worker = Worker::Entity::find()
        .filter(Worker::Column::WorkerId.eq(uuid))
        .one(db)
        .await
        .map_err(|_| AuthError::WrongCredentials)?
        .ok_or(AuthError::WrongCredentials)?;
    Ok(AuthWorker { id: worker.id })
}
