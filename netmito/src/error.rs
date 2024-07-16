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
use serde::{Deserialize, Serialize};

pub type Result<T> = std::result::Result<T, Error>;

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
    #[error("S3 error: {0}")]
    S3Error(#[from] S3Error),
    #[error("Parse url error: {0}")]
    ParseUrlError(#[from] url::ParseError),
    #[error("Request error: {0}")]
    RequestError(#[from] RequestError),
    #[error(transparent)]
    ApiError(#[from] ApiError),
    #[error("Join task error: {0}")]
    JoinError(#[from] tokio::task::JoinError),
    #[error("Nix error: {0}")]
    NixError(#[from] nix::Error),
    #[error("Serde error: {0}")]
    SerdeError(#[from] serde_json::Error),
    #[error("Parse uuid error: {0}")]
    ParseUuidError(#[from] uuid::Error),
    #[error("Redis error: {0}")]
    RedisError(#[from] redis::RedisError),
    #[error("Parse xml error: {0}")]
    ParseXmlError(#[from] roxmltree::Error),
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
    #[error("{0}")]
    Custom(String),
}

#[derive(thiserror::Error, Debug)]
pub enum RequestError {
    #[error("Fail to connect to {0}")]
    ConnectionError(String),
    #[error(transparent)]
    ClientError(#[from] ClientError),
    #[error(transparent)]
    Custom(#[from] reqwest::Error),
}

#[derive(thiserror::Error, Debug)]
pub enum ClientError {
    #[error("{}, {}", .0, .1)]
    Inner(StatusCode, ErrorMsg),
}

pub type ApiResult<T> = std::result::Result<T, ApiError>;

#[derive(thiserror::Error, Debug)]
pub enum ApiError {
    #[error("Internal server error")]
    InternalServerError,
    #[error("Authentication error: {0}")]
    AuthError(#[from] AuthError),
    #[error("Process request error: {0}")]
    InvalidRequest(String),
    #[error("User or group with same name {0} already exists")]
    AlreadyExists(String),
    #[error("{0} not found")]
    NotFound(String),
    #[error("Resource quota exceeded")]
    QuotaExceeded,
    #[error(transparent)]
    PresignS3Error(#[from] S3Error),
}

#[derive(Serialize, Debug, Deserialize)]
pub struct ErrorMsg {
    pub msg: String,
}

impl std::fmt::Display for ErrorMsg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.msg)
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

// impl IntoResponse for AuthError {
//     fn into_response(self) -> axum::response::Response {
//         let status = match self {
//             AuthError::InvalidToken => StatusCode::UNAUTHORIZED,
//             AuthError::WrongCredentials => StatusCode::UNAUTHORIZED,
//             AuthError::PermissionDenied => StatusCode::FORBIDDEN,
//         };
//         let body = Json(json!({ "msg": Error::from(self).to_string() }));
//         (status, body).into_response()
//     }
// }

pub trait GetStatusCode {
    fn get_status_code(&self) -> StatusCode;
}

impl GetStatusCode for AuthError {
    fn get_status_code(&self) -> StatusCode {
        match self {
            AuthError::InvalidToken => StatusCode::UNAUTHORIZED,
            AuthError::WrongCredentials => StatusCode::UNAUTHORIZED,
            AuthError::PermissionDenied => StatusCode::FORBIDDEN,
        }
    }
}

impl GetStatusCode for ApiError {
    fn get_status_code(&self) -> StatusCode {
        match self {
            ApiError::InternalServerError => StatusCode::INTERNAL_SERVER_ERROR,
            ApiError::AuthError(e) => e.get_status_code(),
            ApiError::InvalidRequest(_) => StatusCode::BAD_REQUEST,
            ApiError::AlreadyExists(_) => StatusCode::CONFLICT,
            ApiError::NotFound(_) => StatusCode::NOT_FOUND,
            ApiError::QuotaExceeded => StatusCode::FORBIDDEN,
            ApiError::PresignS3Error(_) => StatusCode::BAD_REQUEST,
        }
    }
}

impl From<ApiError> for ErrorMsg {
    fn from(e: ApiError) -> Self {
        ErrorMsg { msg: e.to_string() }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        (self.get_status_code(), Json(ErrorMsg::from(self))).into_response()
    }
}

pub(crate) fn map_reqwest_err(e: reqwest::Error) -> RequestError {
    if e.is_request() && e.is_connect() {
        RequestError::ConnectionError(e.to_string())
    } else {
        e.into()
    }
}

pub(crate) async fn get_error_from_resp(resp: reqwest::Response) -> RequestError {
    let status_code = resp.status();
    let resp: ErrorMsg = resp
        .json()
        .await
        .unwrap_or_else(|e| ErrorMsg { msg: e.to_string() });
    ClientError::Inner(status_code, resp).into()
}
