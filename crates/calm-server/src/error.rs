//! Unified error type; `IntoResponse` turns it into a JSON `{error, code}` body with an HTTP status.

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
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ErrorBody {
    /// Human-readable error message.
    pub error: String,
    /// Stable machine-readable code (see `CalmError::code`). Three more are written by routes
    /// directly: `forbidden_tool`, `not_a_card_tool`, `tool_call_failed`.
    pub code: String,
}

#[derive(Debug, Error)]
pub enum CalmError {
    /// Codex answered and its answer was a JSON-RPC error; unlike `CodexAppServer` (transport),
    /// retrying it unchanged reproduces the refusal.
    #[error("codex refused: {0}")]
    CodexRefused(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("conflict: {0}")]
    Conflict(String),

    /// 409 — SELECT-inside-tx idempotency sentinel: a worker card with the same `idempotency_key`
    /// already exists. Never escapes the dispatcher closure.
    #[error("dispatch idempotency collision: {0}")]
    IdempotencyCollision(String),

    /// 409 — an `Idempotency-Key` has used up its bounded retry slots; the client must mint a new one.
    #[error("idempotency key exhausted: {0}")]
    IdempotencyKeyExhausted(String),

    /// 409 — `POST /api/today/summary` found no activity in today's window; nothing was created.
    #[error("no activity today: {0}")]
    TodaySummaryNoActivity(String),

    #[error("bad request: {0}")]
    BadRequest(String),

    #[error("unauthorized")]
    Unauthorized,

    /// 403 — non-plugin permission gate (filesystem read denied, etc.).
    #[error("forbidden: {0}")]
    Forbidden(String),

    #[error("plugin install: {0}")]
    PluginInstall(String),

    /// 403 — a permission gate denied the request (manifest perms, non-`neige.*` iframe tool call).
    #[error("plugin permission denied: {0}")]
    PluginPermission(String),

    /// 409 — install attempted on an id that's already installed.
    #[error("plugin conflict: {0}")]
    PluginConflict(String),

    /// 409 — another lifecycle operation holds this plugin id's lifecycle lock; unlike
    /// `PluginConflict`, the identical request will succeed shortly.
    #[error("plugin busy: {0}")]
    PluginBusy(String),

    /// 409 — the plugin's DB row exists but the kernel registry holds no `Manifest`, so there is
    /// no `config_schema` to validate a write against.
    #[error("plugin manifest not loaded: {0}")]
    PluginManifestUnloaded(String),

    /// 409 — the stored `user_config` is not a JSON object; `?reset=true` replaces it with `{}`.
    /// Not a 500: nothing went wrong server-side and the state stays reachable from the API.
    #[error("plugin config corrupt: {0}")]
    PluginConfigCorrupt(String),

    /// 400 — the whole stored document would exceed `USER_CONFIG_MAX_BYTES` with residue from
    /// keys earlier manifests declared; `?reset=true` is the only way out.
    #[error("plugin config too large: {0}")]
    PluginConfigTooLarge(String),

    /// 422 — manifest is structurally valid but its `min_kernel_version` demands a newer kernel.
    #[error("plugin kernel too old: {0}")]
    PluginKernelTooOld(String),

    #[error("planner reset unsupported in shared mode: {0}")]
    PlannerResetUnsupportedInSharedMode(String),

    /// 409 — `/planner/input` hit a planner card whose harness session is dormant and not lazily
    /// recoverable; the client should steer to `/planner/reset` instead of retrying.
    #[error("planner harness dormant: {0}")]
    PlannerHarnessDormant(String),

    /// 409 — the send reached a runtime that is no longer this card's; the text is intact and
    /// re-sending it will reach the successor.
    #[error("planner harness runtime superseded: {0}")]
    PlannerHarnessRuntimeSuperseded(String),

    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),

    /// 500 — a codex `app-server` interaction failed (transport, JSON-RPC error frame, reader task died).
    #[error("codex app-server: {0}")]
    CodexAppServer(String),

    /// 503 — transient backpressure (e.g. the planner harness observation queue is saturated);
    /// clients should retry.
    #[error("service unavailable: {0}")]
    ServiceUnavailable(String),

    /// 413 — a request body exceeded a route's size gate; `http_body_util::Limited` reports the
    /// overrun as it happens rather than trusting `Content-Length`.
    #[error("payload too large: {0}")]
    PayloadTooLarge(String),

    #[error("internal: {0}")]
    Internal(String),
}

impl CalmError {
    pub fn code(&self) -> &'static str {
        match self {
            CalmError::NotFound(_) => "not_found",
            CalmError::Conflict(_) => "conflict",
            CalmError::IdempotencyCollision(_) => "idempotency_collision",
            CalmError::IdempotencyKeyExhausted(_) => "idempotency_key_exhausted",
            CalmError::TodaySummaryNoActivity(_) => "today_summary_no_activity",
            CalmError::BadRequest(_) => "bad_request",
            CalmError::Unauthorized => "unauthorized",
            CalmError::Forbidden(_) => "forbidden",
            CalmError::PluginInstall(_) => "plugin_install",
            CalmError::PluginPermission(_) => "plugin_permission",
            CalmError::PluginConflict(_) => "plugin_conflict",
            CalmError::PluginBusy(_) => "plugin_busy",
            CalmError::PluginManifestUnloaded(_) => "plugin_manifest_unloaded",
            CalmError::PluginConfigCorrupt(_) => "plugin_config_corrupt",
            CalmError::PluginConfigTooLarge(_) => "plugin_config_too_large",
            CalmError::PluginKernelTooOld(_) => "plugin_kernel_too_old",
            CalmError::PlannerResetUnsupportedInSharedMode(_) => {
                "planner_reset_unsupported_in_shared_mode"
            }
            CalmError::PlannerHarnessDormant(_) => "planner_harness_dormant",
            CalmError::PlannerHarnessRuntimeSuperseded(_) => "planner_harness_runtime_superseded",
            CalmError::Db(_) => "db_error",
            CalmError::Io(_) => "io_error",
            CalmError::Serde(_) => "serde_error",
            CalmError::CodexAppServer(_) => "codex_app_server",
            CalmError::CodexRefused(_) => "codex_refused",
            CalmError::ServiceUnavailable(_) => "service_unavailable",
            CalmError::PayloadTooLarge(_) => "payload_too_large",
            CalmError::Internal(_) => "internal",
        }
    }

    pub fn status(&self) -> StatusCode {
        match self {
            CalmError::NotFound(_) => StatusCode::NOT_FOUND,
            CalmError::Conflict(_)
            | CalmError::IdempotencyCollision(_)
            | CalmError::IdempotencyKeyExhausted(_)
            | CalmError::PluginConflict(_)
            | CalmError::PluginBusy(_)
            | CalmError::PluginManifestUnloaded(_)
            | CalmError::PluginConfigCorrupt(_)
            | CalmError::PlannerHarnessDormant(_)
            | CalmError::PlannerHarnessRuntimeSuperseded(_)
            | CalmError::TodaySummaryNoActivity(_) => StatusCode::CONFLICT,
            CalmError::BadRequest(_)
            | CalmError::PluginInstall(_)
            | CalmError::PluginConfigTooLarge(_) => StatusCode::BAD_REQUEST,
            CalmError::Unauthorized => StatusCode::UNAUTHORIZED,
            CalmError::Forbidden(_) | CalmError::PluginPermission(_) => StatusCode::FORBIDDEN,
            CalmError::PluginKernelTooOld(_)
            | CalmError::PlannerResetUnsupportedInSharedMode(_) => StatusCode::UNPROCESSABLE_ENTITY,
            CalmError::ServiceUnavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            CalmError::PayloadTooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
            CalmError::Db(_)
            | CalmError::Io(_)
            | CalmError::Serde(_)
            | CalmError::CodexAppServer(_)
            | CalmError::CodexRefused(_)
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

/// Bridge from the IO-free `CoreError` into the HTTP-mapped `CalmError`; variant mapping is 1:1.
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

impl From<calm_truth::TruthError> for CalmError {
    fn from(err: calm_truth::TruthError) -> Self {
        use calm_truth::TruthError as Truth;
        match err {
            Truth::Core(core) => CalmError::from(core),
            Truth::Forbidden(m) => CalmError::Forbidden(m),
            Truth::Db(e) => CalmError::Db(e),
            Truth::Io(e) => CalmError::Io(e),
            Truth::Serde(e) => CalmError::Serde(e),
            Truth::Internal(m) => CalmError::Internal(m),
        }
    }
}

impl From<calm_truth::session_projection_repo::WorkerSessionProjectionRepoError> for CalmError {
    fn from(err: calm_truth::session_projection_repo::WorkerSessionProjectionRepoError) -> Self {
        CalmError::Internal(err.to_string())
    }
}

impl From<calm_truth::card_kind::CardKindError> for CalmError {
    fn from(err: calm_truth::card_kind::CardKindError) -> Self {
        calm_truth::TruthError::from(err).into()
    }
}

impl From<calm_truth::track_fs_view::TrackFsError> for CalmError {
    fn from(err: calm_truth::track_fs_view::TrackFsError) -> Self {
        calm_truth::TruthError::from(err).into()
    }
}

impl From<CalmError> for calm_truth::TruthError {
    fn from(err: CalmError) -> Self {
        // Lossy bridge: server-only variants collapse to Internal(500).
        match err {
            CalmError::NotFound(m) => calm_types::error::CoreError::NotFound(m).into(),
            CalmError::Conflict(m) => calm_types::error::CoreError::Conflict(m).into(),
            CalmError::IdempotencyCollision(m) => {
                calm_types::error::CoreError::IdempotencyCollision(m).into()
            }
            CalmError::BadRequest(m) => calm_types::error::CoreError::BadRequest(m).into(),
            CalmError::Unauthorized => calm_types::error::CoreError::Unauthorized.into(),
            CalmError::Forbidden(m) => calm_truth::TruthError::Forbidden(m),
            CalmError::ServiceUnavailable(m) => {
                calm_types::error::CoreError::ServiceUnavailable(m).into()
            }
            CalmError::Db(e) => calm_truth::TruthError::Db(e),
            CalmError::Io(e) => calm_truth::TruthError::Io(e),
            CalmError::Serde(e) => calm_truth::TruthError::Serde(e),
            // Route-only variants with no `CoreError`/`TruthError` twin collapse to Internal.
            CalmError::IdempotencyKeyExhausted(m)
            | CalmError::PluginInstall(m)
            | CalmError::PluginPermission(m)
            | CalmError::PluginConflict(m)
            | CalmError::PluginBusy(m)
            | CalmError::PluginManifestUnloaded(m)
            | CalmError::PluginConfigCorrupt(m)
            | CalmError::PluginConfigTooLarge(m)
            | CalmError::PluginKernelTooOld(m)
            | CalmError::PlannerResetUnsupportedInSharedMode(m)
            | CalmError::PlannerHarnessDormant(m)
            | CalmError::PlannerHarnessRuntimeSuperseded(m)
            | CalmError::TodaySummaryNoActivity(m)
            | CalmError::CodexRefused(m)
            | CalmError::CodexAppServer(m)
            | CalmError::PayloadTooLarge(m)
            | CalmError::Internal(m) => calm_truth::TruthError::Internal(m),
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
