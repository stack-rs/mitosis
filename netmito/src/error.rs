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
}

pub type Result<T> = std::result::Result<T, Error>;
