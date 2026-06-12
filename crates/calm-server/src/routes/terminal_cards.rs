//! `POST /api/waves/:wave_id/terminal-cards` — atomic terminal-card creation.
//!
//! Collapses what used to be a 3-step recipe (card-add -> terminal-create ->
//! card-update with `terminal_id` payload) into a single runtime-backed
//! endpoint, then routes the work through the operation runtime:
//!
//! 1. The route derives an operation key and stable payload hash, preserving
//!    non-idempotent semantics unless the caller supplies `Idempotency-Key`.
//! 2. `TerminalAdapter` performs the original DB transaction and emits the
//!    single final-state `card.added` event.
//! 3. The runtime serializes same-key submissions, spawns the terminal side
//!    effect once, and compensates the committed transaction on spawn failure.
//!
//! See #13 for the motivating problem (terminal-card create twitch caused by
//! the multi-event race) and PR1 (#107) for the DB helper this endpoint
//! consumes.

use crate::actor::Actor;
use crate::error::{CalmError, ErrorBody, Result};
use crate::model::{Card, new_id};
use crate::operation::terminal_adapter::{
    TerminalCreateOperationPayload, TerminalCreateRequestPayload, normalize_terminal_create_request,
};
use crate::operation::{OperationKey, OperationOutcome};
use crate::runtime_lookup::project_runtime_into_card_payload;
use crate::state::{AppState, RouteState};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::post,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use utoipa::ToSchema;

pub fn router() -> Router<AppState> {
    Router::new().route(
        "/api/waves/{wave_id}/terminal-cards",
        post(create_terminal_card),
    )
}

/// Body for `POST /api/waves/:wave_id/terminal-cards`.
///
/// Deliberately omits `kind` (always `"terminal"`) and `payload` (the kernel
/// persists schema payload and projects identity from `runtimes`). Empty
/// `program` falls back to `$SHELL` then `/bin/sh`; empty `cwd` falls back to
/// `$HOME` then the server's cwd. `env` is merged into the daemon's environment
/// as additional vars on top of `TERM` / `COLORTERM` / inherited.
#[derive(Serialize, Deserialize, Debug, Clone, ToSchema)]
pub struct NewTerminalCardBody {
    /// Sort order within the wave. `None` defaults to "append to end".
    #[serde(default)]
    pub sort: Option<f64>,
    /// Empty string or missing → `$SHELL` (then `/bin/sh`).
    #[serde(default)]
    pub program: String,
    /// Empty string or missing → `$HOME` (then cwd of server).
    #[serde(default)]
    pub cwd: String,
    /// Extra env on top of the inherited set. JSON object: `{"FOO":"bar"}`.
    #[serde(default)]
    #[schema(value_type = Object)]
    pub env: serde_json::Value,
    /// Host browser's current theme RGB (#177). Required — the kernel
    /// writes it onto the terminal row inside the same transaction
    /// that mints the card, and every spawn for this row reads
    /// `term.theme_fg/_bg` to stamp `--terminal-fg/-bg` daemon argv.
    pub theme: crate::routes::theme::RequestTheme,
}

#[utoipa::path(
    post,
    path = "/api/waves/{wave_id}/terminal-cards",
    tag = "terminals",
    params(("wave_id" = String, Path, description = "Wave id to create the terminal card under")),
    request_body(content = NewTerminalCardBody, description = "Body required (theme is mandatory; program/cwd/env optional)"),
    responses(
        (status = 201, description = "Card + linked terminal created atomically; daemon spawned", body = Card),
        (status = 404, description = "Wave not found", body = ErrorBody),
        (status = 422, description = "Body missing required fields (e.g. theme)", body = ErrorBody),
        (status = 500, description = "Daemon spawn failed; the saga rolled back the committed transaction (no leaked rows).", body = ErrorBody),
    ),
)]
#[allow(deprecated)]
pub(crate) async fn create_terminal_card(
    State(s): State<RouteState>,
    actor: Actor,
    headers: HeaderMap,
    Path(wave_id): Path<String>,
    Json(p): Json<NewTerminalCardBody>,
) -> Result<(StatusCode, Json<Card>)> {
    let request = normalize_terminal_create_request(TerminalCreateRequestPayload {
        wave_id,
        sort: p.sort,
        program: p.program,
        cwd: p.cwd,
        env: p.env,
        theme: p.theme,
    });
    let idempotency_key = parse_idempotency_key_header(&headers)?;
    let operation_key = new_id();
    let runtime_id = new_id();
    let payload_hash = stable_payload_hash(&serde_json::json!({
        "actor": actor.as_str(),
        "request": &request,
    }))?;
    let actor = actor.to_actor_id();
    let payload = serde_json::to_value(TerminalCreateOperationPayload {
        actor,
        runtime_id: Some(runtime_id),
        request,
    })?;
    let op_id = s
        .operation_runtime
        .submit(
            "terminal-create",
            OperationKey {
                operation_key,
                idempotency_key,
                payload_hash,
            },
            payload,
        )
        .await?;
    let result = s.operation_runtime.wait(&op_id).await?;
    match result.outcome {
        OperationOutcome::Succeeded { result }
        | OperationOutcome::SucceededViaCollision { result, .. } => {
            let mut card: Card = serde_json::from_value(result)?;
            project_runtime_into_card_payload(s.repo.as_ref(), &mut card).await?;
            Ok((StatusCode::CREATED, Json(card)))
        }
        OperationOutcome::Failed {
            last_error,
            from_phase,
            last_error_class,
        } => Err(calm_error_from_operation_failure(
            last_error_class.as_deref(),
            last_error,
            from_phase,
        )),
        OperationOutcome::Stuck { .. } => {
            Err(CalmError::Internal("operation stuck, see DB".to_string()))
        }
    }
}

pub(crate) fn parse_idempotency_key_header(headers: &HeaderMap) -> Result<Option<String>> {
    match headers.get("idempotency-key") {
        Some(value) => {
            let value = value.to_str().map_err(|_| {
                CalmError::BadRequest("invalid Idempotency-Key header (non-ASCII bytes)".into())
            })?;
            let value = value.trim();
            if value.is_empty() {
                return Err(CalmError::BadRequest(
                    "invalid Idempotency-Key header (empty)".into(),
                ));
            }
            Ok(Some(value.to_string()))
        }
        None => Ok(None),
    }
}

pub(crate) fn calm_error_from_operation_failure(
    last_error_class: Option<&str>,
    last_error: String,
    from_phase: crate::operation::PhaseTag,
) -> CalmError {
    match last_error_class {
        Some("bad_request") => CalmError::BadRequest(last_error),
        Some("not_found") => CalmError::NotFound(last_error),
        Some("forbidden") => CalmError::Forbidden(last_error),
        Some("conflict") => CalmError::Conflict(last_error),
        Some("unauthorized") => CalmError::Unauthorized,
        _ if from_phase == crate::operation::PhaseTag::Pending => CalmError::BadRequest(last_error),
        _ => CalmError::Internal(last_error),
    }
}

// `pub` (not `pub(crate)`) so the scheduler integration tests can
// construct idempotency-matched worker operations (issue #644 PR-B
// review F8 fixtures).
pub fn stable_payload_hash<T: Serialize>(value: &T) -> Result<String> {
    let value = canonical_json(serde_json::to_value(value)?);
    let bytes = serde_json::to_vec(&value)?;
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    Ok(hex::encode(hasher.finalize()))
}

fn canonical_json(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.into_iter().map(canonical_json).collect())
        }
        serde_json::Value::Object(map) => {
            let sorted: BTreeMap<_, _> = map
                .into_iter()
                .map(|(key, value)| (key, canonical_json(value)))
                .collect();
            serde_json::Value::Object(sorted.into_iter().collect())
        }
        other => other,
    }
}
