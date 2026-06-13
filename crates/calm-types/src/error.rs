//! `CoreError` — the IO-free half of calm-server's `CalmError` (#679 PR1).
//!
//! Issue #679 split line: "`CalmError` splits: core variants → calm-types;
//! `Db(sqlx::Error)` wrapping → calm-truth; `IntoResponse` via a newtype in
//! calm-server (orphan rule forbids the impl elsewhere)."
//!
//! PR1 implements this as the **two-stage enum**: calm-server keeps its
//! `CalmError` (with `Db(sqlx::Error)` + axum `IntoResponse`) untouched —
//! the external error shape is byte-identical — and gains a
//! `From<CoreError>` impl so values produced below the IO line (calm-exec
//! traits, future calm-truth internals) convert losslessly at the boundary.
//! The orphan rule forces this direction: `From<sqlx::Error>` /
//! `IntoResponse` cannot be implemented for a calm-types type from
//! calm-server, so the IO-carrying enum must stay a local type there.
//!
//! Variant list = `CalmError` minus the IO/domain-specific arms (Db, the
//! plugin family, spec-harness and codex-appserver variants). The `code()`
//! strings match `CalmError::code()` for the shared arms so error codes
//! stay stable across the conversion.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("not found: {0}")]
    NotFound(String),

    #[error("conflict: {0}")]
    Conflict(String),

    /// Dispatcher-internal idempotency sentinel — see
    /// `CalmError::IdempotencyCollision` for the full contract.
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
    /// Stable machine-readable code. Matches `CalmError::code()` for the
    /// shared variants so the `{error, code}` HTTP body is unchanged after
    /// a `From<CoreError> for CalmError` conversion.
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
