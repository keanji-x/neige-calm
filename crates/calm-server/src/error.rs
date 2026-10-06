//! Unified error type; `IntoResponse` turns it into a JSON `{error, code}` body with an HTTP status.

use crate::extract::Json;
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use utoipa::ToSchema;

/// JSON shape returned for every error response — `{error, code}`, plus `field` when the refusal
/// names one field of the request.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ErrorBody {
    /// The reason, without the variant's log prefix (`CalmError::reason`): a 404 says `plugin x`,
    /// not `not found: plugin x`; the `code` already says which kind of failure it is.
    pub error: String,
    /// Stable machine-readable code (see `CalmError::code`). Four more are written by routes
    /// directly: `forbidden_tool`, `not_a_card_tool`, `tool_call_failed`, `login_throttled`.
    pub code: String,
    /// The dotted path of the one field the refusal is about (`config.retries`,
    /// `template_input.issue_url`); `error` is then that field's reason alone. Absent rather than
    /// required because only `CalmError::InvalidField` names a field: every other error is about
    /// the request as a whole, and an empty path would be a second way to say so.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
}

#[derive(Debug, Error)]
pub enum CalmError {
    /// Codex answered and its answer was a JSON-RPC error; unlike `CodexAppServer` (transport),
    /// retrying it unchanged reproduces the refusal.
    #[error("codex refused: {0}")]
    CodexRefused(String),

    #[error("not found: {0}")]
    NotFound(String),

    /// 404 — a filesystem read named a path that does not exist (ENOENT, or a component on the way that is not a
    /// directory). Its own code because restoring the path is the remedy, which `NotFound` (the Track, card or row the
    /// request names is gone) never offers. The message already says "path … not found", so `Display` adds no prefix.
    #[error("{0}")]
    PathNotFound(String),

    #[error("conflict: {0}")]
    Conflict(String),

    /// 409 — SELECT-inside-tx idempotency sentinel: a worker card with the same `idempotency_key`
    /// already exists. Never escapes the dispatcher closure.
    #[error("dispatch idempotency collision: {0}")]
    IdempotencyCollision(String),

    /// 409 — an `Idempotency-Key` has used up its bounded retry slots; the client must mint a new one.
    #[error("idempotency key exhausted: {0}")]
    IdempotencyKeyExhausted(String),

    /// 400 — the `Idempotency-Key` header is not one the server stores: blank, not visible ASCII,
    /// or longer than [`crate::routes::idempotency_key::IDEMPOTENCY_KEY_MAX_LEN`].
    #[error("invalid Idempotency-Key: {0}")]
    IdempotencyKeyInvalid(String),

    /// 409 — the `Idempotency-Key` is already bound to a different request on this route. Final:
    /// no retry under this key can be answered for this request.
    #[error("idempotency key reused: {0}")]
    IdempotencyKeyReused(String),

    /// 409 — another request under the same `Idempotency-Key` was accepted at the same time and its
    /// binding refused this one. Retryable: the same request under the same key is answered by it.
    #[error("idempotency key concurrent: {0}")]
    IdempotencyKeyConcurrent(String),

    /// 409 — `POST /api/today/summary` found no activity in today's window; nothing was created.
    #[error("no activity today: {0}")]
    TodaySummaryNoActivity(String),

    #[error("bad request: {0}")]
    BadRequest(String),

    /// 400 `bad_request` about one field of the body: `field` is its dotted path
    /// (`config.retries`), `reason` what is wrong with it. The body carries both, so a client puts
    /// the reason on the field without parsing the text. A violation of the body as a whole has no
    /// field and stays a plain `BadRequest`.
    #[error("bad request: {field}: {reason}")]
    InvalidField { field: String, reason: String },

    /// 422 — the request body is well-formed JSON that does not deserialize into the route's type
    /// (a missing or mistyped field), as `JsonBody` reports it.
    #[error("invalid body: {0}")]
    InvalidBody(String),

    /// 415 — the request carries a JSON body extractor's input without `Content-Type: application/json`.
    #[error("unsupported media type: {0}")]
    UnsupportedMediaType(String),

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

    /// 409 — `plugins_dir/<id>` holds a directory the kernel did not write. Nothing was installed,
    /// so unlike `PluginConflict` it is never an earlier install of the same plugin.
    #[error("plugin directory occupied: {0}")]
    PluginDirOccupied(String),

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
    /// recoverable; the client should offer `/planner/restart` (a fresh session that keeps the
    /// history) instead of retrying.
    #[error("planner harness dormant: {0}")]
    PlannerHarnessDormant(String),

    /// 409 — the send reached a runtime that is no longer this card's; the text is intact and
    /// re-sending it will reach the successor.
    #[error("planner harness runtime superseded: {0}")]
    PlannerHarnessRuntimeSuperseded(String),

    /// 409 — a send that replaces a turn (#2043) was refused before anything was written: the turn
    /// is not the latest, the conversation is busy, or the turn cannot be removed. Nothing changed
    /// and nothing was bound, so the reader is shown the reason.
    #[error("{0}")]
    PlannerTurnNotReplaceable(String),

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

    /// 500 — the operation this request ran, or the one its `Idempotency-Key` names, failed for
    /// good: past its commit with its compensation settled, or stuck before it committed anything.
    /// Final for that key: a replay answers the same, and only a new key may try again.
    #[error("operation failed: {0}")]
    OperationFailed(String),

    /// 500 — that operation stopped part way and is never driven again: what it made may exist.
    /// Final for that key as well, since a replay only answers the same; the caller looks for what
    /// it made before trying again under a new key.
    #[error("operation stuck: {0}")]
    OperationStuck(String),

    #[error("internal: {0}")]
    Internal(String),
}

impl CalmError {
    pub fn code(&self) -> &'static str {
        match self {
            CalmError::NotFound(_) => "not_found",
            CalmError::PathNotFound(_) => "path_not_found",
            CalmError::Conflict(_) => "conflict",
            CalmError::IdempotencyCollision(_) => "idempotency_collision",
            CalmError::IdempotencyKeyExhausted(_) => "idempotency_key_exhausted",
            CalmError::IdempotencyKeyInvalid(_) => "idempotency_key_invalid",
            CalmError::IdempotencyKeyReused(_) => "idempotency_key_reused",
            CalmError::IdempotencyKeyConcurrent(_) => "idempotency_key_concurrent",
            CalmError::TodaySummaryNoActivity(_) => "today_summary_no_activity",
            CalmError::BadRequest(_) | CalmError::InvalidField { .. } => "bad_request",
            CalmError::InvalidBody(_) => "invalid_body",
            CalmError::UnsupportedMediaType(_) => "unsupported_media_type",
            CalmError::Unauthorized => "unauthorized",
            CalmError::Forbidden(_) => "forbidden",
            CalmError::PluginInstall(_) => "plugin_install",
            CalmError::PluginPermission(_) => "plugin_permission",
            CalmError::PluginConflict(_) => "plugin_conflict",
            CalmError::PluginDirOccupied(_) => "plugin_dir_occupied",
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
            CalmError::PlannerTurnNotReplaceable(_) => "planner_turn_not_replaceable",
            CalmError::Db(_) => "db_error",
            CalmError::Io(_) => "io_error",
            CalmError::Serde(_) => "serde_error",
            CalmError::CodexAppServer(_) => "codex_app_server",
            CalmError::CodexRefused(_) => "codex_refused",
            CalmError::ServiceUnavailable(_) => "service_unavailable",
            CalmError::PayloadTooLarge(_) => "payload_too_large",
            CalmError::OperationFailed(_) => "operation_failed",
            CalmError::OperationStuck(_) => "operation_stuck",
            CalmError::Internal(_) => "internal",
        }
    }

    pub fn status(&self) -> StatusCode {
        match self {
            CalmError::NotFound(_) | CalmError::PathNotFound(_) => StatusCode::NOT_FOUND,
            CalmError::Conflict(_)
            | CalmError::IdempotencyCollision(_)
            | CalmError::IdempotencyKeyExhausted(_)
            | CalmError::IdempotencyKeyReused(_)
            | CalmError::IdempotencyKeyConcurrent(_)
            | CalmError::PluginConflict(_)
            | CalmError::PluginDirOccupied(_)
            | CalmError::PluginBusy(_)
            | CalmError::PluginManifestUnloaded(_)
            | CalmError::PluginConfigCorrupt(_)
            | CalmError::PlannerHarnessDormant(_)
            | CalmError::PlannerHarnessRuntimeSuperseded(_)
            | CalmError::PlannerTurnNotReplaceable(_)
            | CalmError::TodaySummaryNoActivity(_) => StatusCode::CONFLICT,
            CalmError::BadRequest(_)
            | CalmError::InvalidField { .. }
            | CalmError::IdempotencyKeyInvalid(_)
            | CalmError::PluginInstall(_)
            | CalmError::PluginConfigTooLarge(_) => StatusCode::BAD_REQUEST,
            CalmError::Unauthorized => StatusCode::UNAUTHORIZED,
            CalmError::Forbidden(_) | CalmError::PluginPermission(_) => StatusCode::FORBIDDEN,
            CalmError::PluginKernelTooOld(_)
            | CalmError::InvalidBody(_)
            | CalmError::PlannerResetUnsupportedInSharedMode(_) => StatusCode::UNPROCESSABLE_ENTITY,
            CalmError::UnsupportedMediaType(_) => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            CalmError::ServiceUnavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            CalmError::PayloadTooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
            CalmError::Db(_)
            | CalmError::Io(_)
            | CalmError::Serde(_)
            | CalmError::CodexAppServer(_)
            | CalmError::CodexRefused(_)
            | CalmError::OperationFailed(_)
            | CalmError::OperationStuck(_)
            | CalmError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl CalmError {
    /// The variant's reason without the prefix `Display` adds: what an HTTP body's `error` says.
    /// `Display` keeps the prefix for logs, persisted operation errors and MCP answers, where the
    /// text is all a reader gets; an HTTP answer has `code` beside it.
    pub fn reason(&self) -> String {
        match self {
            CalmError::CodexRefused(m)
            | CalmError::NotFound(m)
            | CalmError::PathNotFound(m)
            | CalmError::Conflict(m)
            | CalmError::IdempotencyCollision(m)
            | CalmError::IdempotencyKeyExhausted(m)
            | CalmError::IdempotencyKeyInvalid(m)
            | CalmError::IdempotencyKeyReused(m)
            | CalmError::IdempotencyKeyConcurrent(m)
            | CalmError::TodaySummaryNoActivity(m)
            | CalmError::BadRequest(m)
            | CalmError::InvalidBody(m)
            | CalmError::UnsupportedMediaType(m)
            | CalmError::Forbidden(m)
            | CalmError::PluginInstall(m)
            | CalmError::PluginPermission(m)
            | CalmError::PluginConflict(m)
            | CalmError::PluginDirOccupied(m)
            | CalmError::PluginBusy(m)
            | CalmError::PluginManifestUnloaded(m)
            | CalmError::PluginConfigCorrupt(m)
            | CalmError::PluginConfigTooLarge(m)
            | CalmError::PluginKernelTooOld(m)
            | CalmError::PlannerResetUnsupportedInSharedMode(m)
            | CalmError::PlannerHarnessDormant(m)
            | CalmError::PlannerHarnessRuntimeSuperseded(m)
            | CalmError::PlannerTurnNotReplaceable(m)
            | CalmError::CodexAppServer(m)
            | CalmError::ServiceUnavailable(m)
            | CalmError::PayloadTooLarge(m)
            | CalmError::OperationFailed(m)
            | CalmError::OperationStuck(m)
            | CalmError::Internal(m) => m.clone(),
            CalmError::InvalidField { reason, .. } => reason.clone(),
            // Its `Display` carries no prefix: it is the whole sentence.
            CalmError::Unauthorized => self.to_string(),
            CalmError::Db(e) => e.to_string(),
            CalmError::Io(e) => e.to_string(),
            CalmError::Serde(e) => e.to_string(),
        }
    }

    /// The field an `InvalidField` names; every other variant concerns the request as a whole.
    pub fn field(&self) -> Option<&str> {
        match self {
            CalmError::InvalidField { field, .. } => Some(field),
            _ => None,
        }
    }

    /// The JSON body an HTTP answer carries for this error.
    pub fn body(&self) -> ErrorBody {
        ErrorBody {
            error: self.reason(),
            code: self.code().to_string(),
            field: self.field().map(str::to_string),
        }
    }
}

impl IntoResponse for CalmError {
    fn into_response(self) -> Response {
        (self.status(), Json(self.body())).into_response()
    }
}

/// Preserve protocol failures at the server's HTTP and retry-classification boundary.
impl From<provider::codex::error::Error> for CalmError {
    fn from(error: provider::codex::error::Error) -> Self {
        match error {
            provider::codex::error::Error::Transport(message) => Self::CodexAppServer(message),
            provider::codex::error::Error::Refused(message) => Self::CodexRefused(message),
            provider::codex::error::Error::Serde(error) => Self::Serde(error),
        }
    }
}

#[cfg(test)]
mod provider_error_tests {
    use super::*;

    #[test]
    fn codex_error_bridge_preserves_failure_class_and_http_contract() {
        let refused: CalmError = provider::codex::error::Error::Refused("no model".into()).into();
        assert!(matches!(&refused, CalmError::CodexRefused(message) if message == "no model"));
        assert_eq!(refused.code(), "codex_refused");
        assert_eq!(refused.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(refused.to_string(), "codex refused: no model");

        let transport: CalmError = provider::codex::error::Error::Transport("closed".into()).into();
        assert!(matches!(&transport, CalmError::CodexAppServer(message) if message == "closed"));
        assert_eq!(transport.code(), "codex_app_server");
        assert_eq!(transport.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(transport.to_string(), "codex app-server: closed");

        let source = serde_json::from_str::<serde_json::Value>("{").unwrap_err();
        let message = source.to_string();
        let serde: CalmError = provider::codex::error::Error::Serde(source).into();
        assert!(matches!(&serde, CalmError::Serde(_)));
        assert_eq!(serde.code(), "serde_error");
        assert_eq!(serde.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(serde.to_string(), format!("serde: {message}"));
    }
}

#[cfg(test)]
mod response_body_tests {
    use super::*;

    async fn answer(error: CalmError) -> (StatusCode, serde_json::Value) {
        let response = error.into_response();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    /// The body says the reason and leaves the kind to `code`; `Display` keeps its prefix for logs.
    #[tokio::test]
    async fn the_body_carries_the_reason_and_display_keeps_the_prefix() {
        let not_found = CalmError::NotFound("plugin git-forge".into());
        assert_eq!(not_found.to_string(), "not found: plugin git-forge");
        assert_eq!(
            answer(not_found).await,
            (
                StatusCode::NOT_FOUND,
                serde_json::json!({ "error": "plugin git-forge", "code": "not_found" })
            )
        );
        assert_eq!(
            answer(CalmError::Unauthorized).await.1,
            serde_json::json!({ "error": "unauthorized", "code": "unauthorized" })
        );
    }

    /// Only a field violation carries `field`; its `error` is that field's reason alone.
    #[tokio::test]
    async fn a_field_violation_answers_its_field_beside_its_reason() {
        let violation = CalmError::InvalidField {
            field: "config.retries".into(),
            reason: "expected type `integer`".into(),
        };
        assert_eq!(
            violation.to_string(),
            "bad request: config.retries: expected type `integer`"
        );
        assert_eq!(
            answer(violation).await,
            (
                StatusCode::BAD_REQUEST,
                serde_json::json!({
                    "error": "expected type `integer`",
                    "code": "bad_request",
                    "field": "config.retries",
                })
            )
        );
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
            Truth::IdempotencyKeyReused(m) => CalmError::IdempotencyKeyReused(m),
            Truth::IdempotencyKeyExhausted(m) => CalmError::IdempotencyKeyExhausted(m),
            Truth::IdempotencyKeyConcurrent(m) => CalmError::IdempotencyKeyConcurrent(m),
            Truth::PlannerTurnNotReplaceable(m) => CalmError::PlannerTurnNotReplaceable(m),
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
            // The storage layer has no field-shaped refusal; the path stays in the text.
            CalmError::InvalidField { field, reason } => {
                calm_types::error::CoreError::BadRequest(format!("{field}: {reason}")).into()
            }
            CalmError::Unauthorized => calm_types::error::CoreError::Unauthorized.into(),
            CalmError::Forbidden(m) => calm_truth::TruthError::Forbidden(m),
            CalmError::ServiceUnavailable(m) => {
                calm_types::error::CoreError::ServiceUnavailable(m).into()
            }
            CalmError::Db(e) => calm_truth::TruthError::Db(e),
            CalmError::Io(e) => calm_truth::TruthError::Io(e),
            CalmError::Serde(e) => calm_truth::TruthError::Serde(e),
            CalmError::IdempotencyKeyReused(m) => calm_truth::TruthError::IdempotencyKeyReused(m),
            CalmError::IdempotencyKeyExhausted(m) => {
                calm_truth::TruthError::IdempotencyKeyExhausted(m)
            }
            CalmError::IdempotencyKeyConcurrent(m) => {
                calm_truth::TruthError::IdempotencyKeyConcurrent(m)
            }
            CalmError::PlannerTurnNotReplaceable(m) => {
                calm_truth::TruthError::PlannerTurnNotReplaceable(m)
            }
            // Route-only variants with no `CoreError`/`TruthError` twin collapse to Internal.
            CalmError::IdempotencyKeyInvalid(m)
            | CalmError::PathNotFound(m)
            | CalmError::InvalidBody(m)
            | CalmError::UnsupportedMediaType(m)
            | CalmError::PluginInstall(m)
            | CalmError::PluginPermission(m)
            | CalmError::PluginConflict(m)
            | CalmError::PluginDirOccupied(m)
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
            | CalmError::OperationFailed(m)
            | CalmError::OperationStuck(m)
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

#[cfg(test)]
mod truth_error_bridge_tests {
    use super::CalmError;

    /// The typed refusals raised inside a storage transaction must come back out of it with their
    /// code and message: the write path converts a closure's error to `TruthError` and back.
    #[test]
    fn typed_refusals_survive_the_storage_round_trip() {
        let cases = [
            CalmError::IdempotencyKeyReused("x".into()),
            CalmError::IdempotencyKeyExhausted("x".into()),
            CalmError::IdempotencyKeyConcurrent("x".into()),
            CalmError::PlannerTurnNotReplaceable("x".into()),
        ];
        for error in cases {
            let (code, status, message) = (error.code(), error.status(), error.to_string());
            let back = CalmError::from(calm_truth::TruthError::from(error));
            assert_eq!(back.code(), code, "code drift for {back:?}");
            assert_eq!(back.status(), status, "status drift for {back:?}");
            assert_eq!(back.to_string(), message, "message drift for {back:?}");
        }
    }
}
