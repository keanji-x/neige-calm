//! Unified error type. Anything a handler bubbles up converts here, and
//! `IntoResponse` turns it into a JSON `{error, code}` body with a sane
//! HTTP status.

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;
use utoipa::ToSchema;

/// JSON shape returned for every error response — `{error, code}`.
/// Mirrors the body produced by `CalmError::into_response`. Hand-written
/// duplicate of the in-line `json!` body so OpenAPI consumers see a
/// concrete schema.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ErrorBody {
    /// Human-readable error message.
    pub error: String,
    /// Stable machine-readable code — one of `not_found`, `conflict`,
    /// `bad_request`, `unauthorized`, `plugin_install`, `plugin_permission`,
    /// `plugin_conflict`, `db_error`, `io_error`, `serde_error`, `internal`,
    /// `forbidden_tool`, `not_a_card_tool`, `tool_call_failed`.
    pub code: String,
}

#[derive(Debug, Error)]
pub enum CalmError {
    #[error("not found: {0}")]
    NotFound(String),

    #[error("conflict: {0}")]
    Conflict(String),

    #[error("bad request: {0}")]
    BadRequest(String),

    #[error("unauthorized")]
    Unauthorized,

    // ---- M3 plugin-specific variants ----
    //
    // Distinct from the generic shapes above so route bodies can carry the
    // plugin-system error codes the design doc §7 enumerates. The HTTP status
    // mapping mirrors §7's table.
    /// 400 — manifest invalid, install path missing, unsupported source kind.
    /// The carried string lands in the response body's `error` field.
    #[error("plugin install: {0}")]
    PluginInstall(String),

    /// 403 — a permission gate denied the request (manifest perms, etc.).
    /// Also used by the M5 tool-call route when an iframe attempts a
    /// non-`neige.*` tool call (§7.6 row 5).
    #[error("plugin permission denied: {0}")]
    PluginPermission(String),

    /// 409 — install attempted on an id that's already installed. Distinct
    /// from the generic Conflict variant so the API client can branch on the
    /// code without string-matching the message.
    #[error("plugin conflict: {0}")]
    PluginConflict(String),

    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("internal: {0}")]
    Internal(String),
}

impl CalmError {
    pub fn code(&self) -> &'static str {
        match self {
            CalmError::NotFound(_) => "not_found",
            CalmError::Conflict(_) => "conflict",
            CalmError::BadRequest(_) => "bad_request",
            CalmError::Unauthorized => "unauthorized",
            CalmError::PluginInstall(_) => "plugin_install",
            CalmError::PluginPermission(_) => "plugin_permission",
            CalmError::PluginConflict(_) => "plugin_conflict",
            CalmError::Db(_) => "db_error",
            CalmError::Io(_) => "io_error",
            CalmError::Serde(_) => "serde_error",
            CalmError::Internal(_) => "internal",
        }
    }

    pub fn status(&self) -> StatusCode {
        match self {
            CalmError::NotFound(_) => StatusCode::NOT_FOUND,
            CalmError::Conflict(_) | CalmError::PluginConflict(_) => StatusCode::CONFLICT,
            CalmError::BadRequest(_) | CalmError::PluginInstall(_) => StatusCode::BAD_REQUEST,
            CalmError::Unauthorized => StatusCode::UNAUTHORIZED,
            CalmError::PluginPermission(_) => StatusCode::FORBIDDEN,
            CalmError::Db(_)
            | CalmError::Io(_)
            | CalmError::Serde(_)
            | CalmError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for CalmError {
    fn into_response(self) -> Response {
        let body = json!({
            "error": self.to_string(),
            "code": self.code(),
        });
        (self.status(), Json(body)).into_response()
    }
}

pub type Result<T, E = CalmError> = std::result::Result<T, E>;
