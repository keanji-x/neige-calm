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
    /// `idempotency_collision`, `bad_request`, `unauthorized`,
    /// `forbidden`, `plugin_install`, `plugin_permission`,
    /// `plugin_conflict`, `plugin_kernel_too_old`,
    /// `spec_harness_dormant`, `db_error`, `io_error`, `serde_error`,
    /// `codex_app_server`, `service_unavailable`, `internal`,
    /// `forbidden_tool`, `not_a_card_tool`, `tool_call_failed`.
    pub code: String,
}

#[derive(Debug, Error)]
pub enum CalmError {
    #[error("not found: {0}")]
    NotFound(String),

    #[error("conflict: {0}")]
    Conflict(String),

    /// 409 — dispatcher-internal sentinel emitted by the
    /// SELECT-inside-tx idempotency check when a worker card with the
    /// same `idempotency_key` already exists. Distinct from the generic
    /// [`CalmError::Conflict`] so the spawn-side caller can match
    /// precisely on "duplicate request, treat as success" vs. real
    /// uniqueness violations bubbling up from the DB layer (terminal
    /// already exists for card, card-id PK collision, etc.). Same HTTP
    /// status as `Conflict` because no current route surfaces this
    /// variant to clients — it never escapes the dispatcher closure.
    #[error("dispatch idempotency collision: {0}")]
    IdempotencyCollision(String),

    #[error("bad request: {0}")]
    BadRequest(String),

    #[error("unauthorized")]
    Unauthorized,

    /// 403 — non-plugin permission gate (filesystem read denied, etc.).
    /// Distinct from `PluginPermission` so error codes stay meaningful.
    #[error("forbidden: {0}")]
    Forbidden(String),

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

    /// 422 — manifest is structurally valid but its `min_kernel_version`
    /// demands a kernel newer than the one we are. Distinct from
    /// `PluginInstall` (which is a 400 "your input is malformed") because
    /// the input is fine; it's our deployment that's incompatible. Issue #45.
    #[error("plugin kernel too old: {0}")]
    PluginKernelTooOld(String),

    #[error("spec reset unsupported in shared mode: {0}")]
    SpecResetUnsupportedInSharedMode(String),

    /// 409 — `/spec/input` hit a spec card whose harness session is dormant
    /// and not lazily recoverable: no active runtime row exists (the
    /// `spec-harness-start` operation failed at wave creation), or the active
    /// row is unusable (NULL/empty `thread_id` from a half-failed start, or a
    /// corrupt/unknown snapshot shape). Distinct from the generic
    /// [`CalmError::Conflict`] 409 ("runtime shutting down") so the web client
    /// can branch on the error code and steer the user to `/spec/reset`
    /// instead of retrying. Issue #649 (i2).
    #[error("spec harness dormant: {0}")]
    SpecHarnessDormant(String),

    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),

    /// 500 — a codex `app-server` interaction failed: WebSocket transport
    /// error, a JSON-RPC error frame returned by the server, or the
    /// connection's reader task dying mid-request. Issue #293 PR2 — the
    /// [`crate::codex_appserver`] client maps every failure mode onto this
    /// one variant; the carried string is the human-readable cause (it is
    /// never surfaced to an HTTP client today, the client is a daemon-side
    /// control channel, so a single coarse variant keeps `CalmError` from
    /// sprouting transport-specific shapes).
    #[error("codex app-server: {0}")]
    CodexAppServer(String),

    /// 503 — transient backpressure. The server understood the request but
    /// is temporarily unable to enqueue/process it (e.g., the spec harness
    /// observation queue is saturated). Clients should retry; the body
    /// message indicates what was full and may suggest a back-off. Distinct
    /// from `Internal` because nothing went wrong server-side — this is the
    /// flow-control signal RFC 7231 §6.6.4 specifies.
    #[error("service unavailable: {0}")]
    ServiceUnavailable(String),

    #[error("internal: {0}")]
    Internal(String),
}

impl CalmError {
    pub fn code(&self) -> &'static str {
        match self {
            CalmError::NotFound(_) => "not_found",
            CalmError::Conflict(_) => "conflict",
            CalmError::IdempotencyCollision(_) => "idempotency_collision",
            CalmError::BadRequest(_) => "bad_request",
            CalmError::Unauthorized => "unauthorized",
            CalmError::Forbidden(_) => "forbidden",
            CalmError::PluginInstall(_) => "plugin_install",
            CalmError::PluginPermission(_) => "plugin_permission",
            CalmError::PluginConflict(_) => "plugin_conflict",
            CalmError::PluginKernelTooOld(_) => "plugin_kernel_too_old",
            CalmError::SpecResetUnsupportedInSharedMode(_) => {
                "spec_reset_unsupported_in_shared_mode"
            }
            CalmError::SpecHarnessDormant(_) => "spec_harness_dormant",
            CalmError::Db(_) => "db_error",
            CalmError::Io(_) => "io_error",
            CalmError::Serde(_) => "serde_error",
            CalmError::CodexAppServer(_) => "codex_app_server",
            CalmError::ServiceUnavailable(_) => "service_unavailable",
            CalmError::Internal(_) => "internal",
        }
    }

    pub fn status(&self) -> StatusCode {
        match self {
            CalmError::NotFound(_) => StatusCode::NOT_FOUND,
            CalmError::Conflict(_)
            | CalmError::IdempotencyCollision(_)
            | CalmError::PluginConflict(_)
            | CalmError::SpecHarnessDormant(_) => StatusCode::CONFLICT,
            CalmError::BadRequest(_) | CalmError::PluginInstall(_) => StatusCode::BAD_REQUEST,
            CalmError::Unauthorized => StatusCode::UNAUTHORIZED,
            CalmError::Forbidden(_) | CalmError::PluginPermission(_) => StatusCode::FORBIDDEN,
            CalmError::PluginKernelTooOld(_) | CalmError::SpecResetUnsupportedInSharedMode(_) => {
                StatusCode::UNPROCESSABLE_ENTITY
            }
            CalmError::ServiceUnavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            CalmError::Db(_)
            | CalmError::Io(_)
            | CalmError::Serde(_)
            | CalmError::CodexAppServer(_)
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

/// #679 PR1 — bridge from the IO-free core error (calm-types/calm-exec
/// layers) into the HTTP-mapped `CalmError`. This is the "two-stage enum"
/// half of the issue's CalmError split: `CalmError` itself stays a local
/// type (the orphan rule pins `Db(#[from] sqlx::Error)` and `IntoResponse`
/// here), while signatures below the IO line speak
/// [`calm_types::error::CoreError`] and convert losslessly at the boundary.
/// Variant mapping is 1:1 — `code()` and `status()` are preserved for every
/// shared arm (pinned by the test below).
impl From<calm_types::error::CoreError> for CalmError {
    fn from(err: calm_types::error::CoreError) -> Self {
        use calm_types::error::CoreError as Core;
        match err {
            Core::NotFound(m) => CalmError::NotFound(m),
            Core::Conflict(m) => CalmError::Conflict(m),
            Core::IdempotencyCollision(m) => CalmError::IdempotencyCollision(m),
            Core::BadRequest(m) => CalmError::BadRequest(m),
            Core::Unauthorized => CalmError::Unauthorized,
            Core::Forbidden(m) => CalmError::Forbidden(m),
            Core::ServiceUnavailable(m) => CalmError::ServiceUnavailable(m),
            Core::Io(e) => CalmError::Io(e),
            Core::Serde(e) => CalmError::Serde(e),
            Core::Internal(m) => CalmError::Internal(m),
        }
    }
}

pub type Result<T, E = CalmError> = std::result::Result<T, E>;

#[cfg(test)]
mod core_error_bridge_tests {
    use super::CalmError;
    use calm_types::error::CoreError;

    #[test]
    fn conversion_preserves_code_and_status() {
        // The external error shape (`{error, code}` + HTTP status) must be
        // unchanged by the #679 PR1 split: converting a CoreError into
        // CalmError keeps the same machine-readable code for every arm.
        let cases: Vec<CoreError> = vec![
            CoreError::NotFound("x".into()),
            CoreError::Conflict("x".into()),
            CoreError::IdempotencyCollision("x".into()),
            CoreError::BadRequest("x".into()),
            CoreError::Unauthorized,
            CoreError::Forbidden("x".into()),
            CoreError::ServiceUnavailable("x".into()),
            CoreError::Io(std::io::Error::other("x")),
            CoreError::Serde(serde_json::from_str::<i32>("x").unwrap_err()),
            CoreError::Internal("x".into()),
        ];
        for core in cases {
            let code = core.code();
            let message = core.to_string();
            let mapped = CalmError::from(core);
            assert_eq!(mapped.code(), code, "code drift for {mapped:?}");
            assert_eq!(mapped.to_string(), message, "message drift for {mapped:?}");
        }
    }
}
