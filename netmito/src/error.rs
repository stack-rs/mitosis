use std::string::FromUtf8Error;

use aws_sdk_s3::{
    error::SdkError,
    operation::{
        create_bucket::CreateBucketError, get_object::GetObjectError, put_object::PutObjectError,
    },
    presigning::PresigningConfigError,
};
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
    #[error("S3 Error: {0}")]
    S3Error(#[from] S3Error),
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
