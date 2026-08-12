#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("configuration error: {0}")]
    Configuration(String),
    #[error("authentication error: {0}")]
    Authentication(String),
    #[error("authorization error: {0}")]
    Authorization(String),
    #[error("network error: {0}")]
    Network(String),
    #[error("storage error: {0}")]
    Storage(String),
    #[error("media error: {0}")]
    Media(String),
    #[error("terminal error: {0}")]
    Terminal(String),
    #[error("invalid command: {0}")]
    InvalidCommand(String),
}

pub type Result<T> = std::result::Result<T, AppError>;
