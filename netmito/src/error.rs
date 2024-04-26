use std::string::FromUtf8Error;

use aws_sdk_s3::{
    error::SdkError,
    operation::{
        create_bucket::CreateBucketError, get_object::GetObjectError, put_object::PutObjectError,
    },
    presigning::PresigningConfigError,
};
use axum::{http::StatusCode, response::IntoResponse, Json};
use sea_orm::{DbErr, TransactionError};
use serde_json::json;

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("Invalid configuration {0}")]
    ConfigError(#[from] figment::Error),
    #[error("Database error: {0}")]
    DbError(#[from] DbErr),
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
    #[error("Encrypt error: {0}")]
    EncryptError(#[from] argon2::password_hash::Error),
    #[error("Error: {0}")]
    Custom(String),
    #[error("Auth error: {0}")]
    AuthError(#[from] AuthError),
    #[error("Encode token error: {0}")]
    EncodeTokenError(#[from] jsonwebtoken::errors::Error),
    #[error("Decode token error: {0}")]
    DecodeTokenError(#[from] DecodeTokenError),
    #[error("S3 Error: {0}")]
    S3Error(#[from] S3Error),
}

#[derive(thiserror::Error, Debug)]
pub enum AuthError {
    #[error("Invalid token")]
    InvalidToken,
    #[error("Wrong credentials")]
    WrongCredentials,
    #[error("Permission denied")]
    PermissionDenied,
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

#[derive(thiserror::Error, Debug)]
pub enum S3Error {
    #[error(transparent)]
    CreateBucketError(#[from] SdkError<CreateBucketError>),
    #[error(transparent)]
    PutObjectError(#[from] SdkError<PutObjectError>),
    #[error(transparent)]
    GetObjectError(#[from] SdkError<GetObjectError>),
    #[error(transparent)]
    PresigningConfigError(#[from] PresigningConfigError),
    #[error("Invalid content length {0}")]
    InvalidContentLength(i64),
}

pub type Result<T> = std::result::Result<T, Error>;

impl IntoResponse for AuthError {
    fn into_response(self) -> axum::response::Response {
        let status = match self {
            AuthError::InvalidToken => StatusCode::UNAUTHORIZED,
            AuthError::WrongCredentials => axum::http::StatusCode::UNAUTHORIZED,
            AuthError::PermissionDenied => axum::http::StatusCode::FORBIDDEN,
        };
        let body = Json(json!({ "msg": Error::from(self).to_string() }));
        (status, body).into_response()
    }
}

impl From<TransactionError<Error>> for Error {
    fn from(e: TransactionError<Error>) -> Self {
        match e {
            TransactionError::Connection(e) => e.into(),
            TransactionError::Transaction(e) => e,
        }
    }
}
