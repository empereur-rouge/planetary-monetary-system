use thiserror::Error;

#[derive(Debug, Error)]
pub enum SimError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error("Server error ({status}): {message}")]
    ServerError { status: u16, message: String },

    #[error("Gemini API error: {0}")]
    Gemini(String),

    #[error("Insufficient balance for agent {agent}: has {available}, needs {required}")]
    InsufficientBalance {
        agent: String,
        available: String,
        required: String,
    },

    #[error("Config error: {0}")]
    Config(String),

    #[error("Bootstrap failed: {0}")]
    Bootstrap(String),

    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub type SimResult<T> = Result<T, SimError>;
