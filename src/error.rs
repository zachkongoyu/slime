use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("provider request failed: {0}")]
    Request(String),

    #[error("provider returned an error {status}: {body}")]
    ApiError { status: u16, body: String },

    #[error("feature not supported by this provider")]
    NotSupported,

    #[error("response parse error: {0}")]
    Parse(String),
}

#[derive(Debug, thiserror::Error)]
pub enum MossError {
    #[error("provider error: {0}")]
    Provider(#[from] ProviderError),

    #[error("solver error for gap {gap_id}: {reason}")]
    Solver { gap_id: Uuid, reason: String },

    #[error("defense scan rejected code: {reason}")]
    DefenseRejection { reason: String },

    #[error("blackboard error: {0}")]
    Blackboard(String),

    #[error("deadlock: blocked gaps remain but no gaps are ready or assigned")]
    Deadlock,

    #[error("session expired")]
    SessionExpired,

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
