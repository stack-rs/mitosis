use sea_orm::DbErr;

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("Invalid configuration {0}")]
    InvalidConfig(#[from] figment::Error),
    #[error("Database error: {0}")]
    DbErr(#[from] DbErr),
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
