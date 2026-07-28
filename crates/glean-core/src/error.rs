use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("http: {0}")]
    Http(String),
    #[error("parse: {0}")]
    Parse(String),
    #[error("{0}")]
    Message(String),
}

pub type Result<T> = std::result::Result<T, CoreError>;
