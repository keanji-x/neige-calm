//! `CoreError` — the IO-free half of calm-server's `CalmError`; `code()` strings match
//! `CalmError::code()` for the shared arms.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("not found: {0}")]
    NotFound(String),

    #[error("conflict: {0}")]
    Conflict(String),

    /// Dispatcher-internal idempotency sentinel.
    #[error("dispatch idempotency collision: {0}")]
    IdempotencyCollision(String),

    #[error("bad request: {0}")]
    BadRequest(String),

    #[error("unauthorized")]
    Unauthorized,

    #[error("forbidden: {0}")]
    Forbidden(String),

    /// Transient backpressure — the flow-control signal, not a fault.
    #[error("service unavailable: {0}")]
    ServiceUnavailable(String),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("internal: {0}")]
    Internal(String),
}

impl CoreError {
    /// Stable machine-readable code.
    pub fn code(&self) -> &'static str {
        match self {
            CoreError::NotFound(_) => "not_found",
            CoreError::Conflict(_) => "conflict",
            CoreError::IdempotencyCollision(_) => "idempotency_collision",
            CoreError::BadRequest(_) => "bad_request",
            CoreError::Unauthorized => "unauthorized",
            CoreError::Forbidden(_) => "forbidden",
            CoreError::ServiceUnavailable(_) => "service_unavailable",
            CoreError::Io(_) => "io_error",
            CoreError::Serde(_) => "serde_error",
            CoreError::Internal(_) => "internal",
        }
    }
}

/// Result alias for the IO-free layers (calm-types itself, calm-exec).
pub type CoreResult<T, E = CoreError> = std::result::Result<T, E>;
