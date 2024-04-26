use std::string::FromUtf8Error;

use sea_orm::DbErr;

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("Invalid configuration {0}")]
    InvalidConfig(#[from] figment::Error),
    #[error("Database error: {0}")]
    DbErr(#[from] DbErr),
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
    #[error("Hash error: {0}")]
    HashError(argon2::password_hash::Error),
    #[error("Custom error: {0}")]
    Custom(String),
    #[error("Auth error: {0}")]
    AuthError(#[from] AuthError),
    #[error("JWT Error: {0}")]
    EncodeTokenError(#[from] jsonwebtoken::errors::Error),
    #[error("Invalid token: {0}")]
    DecodeTokenError(#[from] DecodeTokenError),
}

#[derive(thiserror::Error, Debug)]
pub enum AuthError {
    #[error("Invalid token")]
    InvalidToken,
    #[error("Wrong credentials")]
    WrongCredentials,
}

#[derive(thiserror::Error, Debug)]
pub enum DecodeTokenError {
    #[error(transparent)]
    Base64Error(#[from] base64::DecodeError),
    #[error(transparent)]
    JWTError(#[from] jsonwebtoken::errors::Error),
    #[error(transparent)]
    ParseError(#[from] FromUtf8Error),
}

pub type Result<T> = std::result::Result<T, Error>;
