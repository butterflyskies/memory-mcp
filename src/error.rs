use thiserror::Error;

#[derive(Debug, Error)]
pub enum MemoryError {
    #[error("git error: {0}")]
    Git(#[from] git2::Error),

    #[error("embedding error: {0}")]
    Embedding(String),

    #[error("index error: {0}")]
    Index(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("memory not found: {name}")]
    NotFound { name: String },

    #[error("invalid input: {reason}")]
    InvalidInput { reason: String },

    #[error("auth error: {0}")]
    Auth(String),

    #[error("oauth error: {0}")]
    OAuthError(String),

    #[error("token storage error: {0}")]
    TokenStorage(String),

    #[error("yaml error: {0}")]
    Yaml(#[from] serde_yaml::Error),

    #[error("task join error: {0}")]
    Join(String),

    #[error("internal error: {0}")]
    Internal(String),
}

impl From<MemoryError> for rmcp::model::ErrorData {
    fn from(err: MemoryError) -> Self {
        let code = match &err {
            MemoryError::NotFound { .. } | MemoryError::InvalidInput { .. } => {
                rmcp::model::ErrorCode::INVALID_PARAMS
            }
            _ => rmcp::model::ErrorCode::INTERNAL_ERROR,
        };
        rmcp::model::ErrorData {
            code,
            message: std::borrow::Cow::Owned(err.to_string()),
            data: None,
        }
    }
}
