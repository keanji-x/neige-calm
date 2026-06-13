use thiserror::Error;

#[derive(Debug, Error)]
pub enum TruthError {
    #[error(transparent)]
    Core(calm_types::error::CoreError),

    #[error("forbidden: {0}")]
    Forbidden(String),

    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("internal: {0}")]
    Internal(String),
}

impl TruthError {
    pub fn is_not_found(&self) -> bool {
        matches!(
            self,
            TruthError::Core(calm_types::error::CoreError::NotFound(_))
        )
    }
}

#[allow(non_snake_case)]
impl TruthError {
    pub fn NotFound(message: impl Into<String>) -> Self {
        calm_types::error::CoreError::NotFound(message.into()).into()
    }

    pub fn Conflict(message: impl Into<String>) -> Self {
        calm_types::error::CoreError::Conflict(message.into()).into()
    }

    pub fn IdempotencyCollision(message: impl Into<String>) -> Self {
        calm_types::error::CoreError::IdempotencyCollision(message.into()).into()
    }

    #[allow(non_upper_case_globals)]
    pub const Unauthorized: Self = Self::Core(calm_types::error::CoreError::Unauthorized);

    pub fn BadRequest(message: impl Into<String>) -> Self {
        calm_types::error::CoreError::BadRequest(message.into()).into()
    }

    pub fn ServiceUnavailable(message: impl Into<String>) -> Self {
        calm_types::error::CoreError::ServiceUnavailable(message.into()).into()
    }
}

impl From<calm_types::error::CoreError> for TruthError {
    fn from(err: calm_types::error::CoreError) -> Self {
        use calm_types::error::CoreError as Core;
        match err {
            Core::Forbidden(m) => TruthError::Forbidden(m),
            Core::Io(e) => TruthError::Io(e),
            Core::Serde(e) => TruthError::Serde(e),
            Core::Internal(m) => TruthError::Internal(m),
            other => TruthError::Core(other),
        }
    }
}

/// Migration-readability alias; shadows calm-server's `CalmError` enum.
pub type CalmError = TruthError;
pub type Result<T, E = TruthError> = std::result::Result<T, E>;
