use thiserror::Error;

#[derive(Error, Debug)]
pub enum TrustedServerError {
    #[error("Configuration error: {0}")]
    Config(#[from] config::ConfigError),

    #[error("Template rendering error: {0}")]
    Template(#[from] handlebars::RenderError),

    #[error("Invalid UTF-8: {0}")]
    Utf8(#[from] std::str::Utf8Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("HTTP error: {0}")]
    Http(String),

    #[error("KV store error: {0}")]
    KvStore(String),

    #[error("Invalid request: {0}")]
    InvalidRequest(String),

    #[error("Security error: {0}")]
    Security(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, TrustedServerError>;
