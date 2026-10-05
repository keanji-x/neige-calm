//! `POST /api/tracks/:track_id/terminal-cards` — atomic terminal-card creation through
//! the operation runtime; non-idempotent unless the caller supplies `Idempotency-Key`.

use crate::actor::Actor;
use crate::error::{CalmError, ErrorBody, Result};
use crate::model::{Card, new_id};
use crate::operation::terminal_adapter::{
    TerminalCreateOperationPayload, TerminalCreateRequestPayload, normalize_terminal_create_request,
};
use crate::operation::{OperationKey, OperationOutcome};
use crate::session_projection_lookup::project_runtime_into_card_payload;
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
        "/api/tracks/{track_id}/terminal-cards",
        post(create_terminal_card),
    )
}

/// Body for `POST /api/tracks/:track_id/terminal-cards`. Omits `kind` (always
/// `"terminal"`) and `payload` (the kernel persists the schema payload).
#[derive(Serialize, Deserialize, Debug, Clone, ToSchema)]
pub struct NewTerminalCardBody {
    #[serde(default)]
    pub title: Option<String>,
    /// Sort order within the track. `None` defaults to "append to end".
    #[serde(default)]
    pub sort: Option<f64>,
    /// Empty string or missing → `$SHELL` (then `/bin/sh`).
    #[serde(default)]
    pub program: String,
    /// Empty string or missing → the track's workspace path.
    #[serde(default)]
    pub cwd: String,
    /// Extra env on top of the inherited set. JSON object: `{"FOO":"bar"}`.
    #[serde(default)]
    #[schema(value_type = Object)]
    pub env: serde_json::Value,
    /// Host browser's current theme RGB. Required — written onto the terminal row in the
    /// same transaction that mints the card; every spawn reads it for `--terminal-fg/-bg`.
    pub theme: crate::routes::theme::RequestTheme,
}

#[utoipa::path(
    post,
    path = "/api/tracks/{track_id}/terminal-cards",
    tag = "terminals",
    params(
        ("track_id" = String, Path, description = "Track id to create the terminal card under"),
        ("Idempotency-Key" = Option<String>, Header, description = "Optional; without one a retry creates another card. A retry under the key returns the same card."),
    ),
    request_body(content = NewTerminalCardBody, description = "Body required (theme is mandatory; program/cwd/env optional)"),
    responses(
        (status = 201, description = "Card + linked terminal created atomically; daemon spawned", body = Card),
        (status = 400, description = "An `Idempotency-Key` blank, non-ASCII or over 128 bytes (`idempotency_key_invalid`)", body = ErrorBody),
        (status = 404, description = "Track not found", body = ErrorBody),
        (status = 409, description = "This `Idempotency-Key` was already used for a different request (code `idempotency_key_reused`); final for this key", body = ErrorBody),
        (status = 422, description = "Body missing required fields (e.g. theme)", body = ErrorBody),
        (status = 500, description = "Daemon spawn failed; the saga rolled back the committed transaction (no leaked rows).", body = ErrorBody),
    ),
)]
#[allow(deprecated)]
pub(crate) async fn create_terminal_card(
    State(s): State<RouteState>,
    actor: Actor,
    headers: HeaderMap,
    Path(track_id): Path<String>,
    Json(p): Json<NewTerminalCardBody>,
) -> Result<(StatusCode, Json<Card>)> {
    let request = normalize_terminal_create_request(TerminalCreateRequestPayload {
        track_id,
        title: p.title,
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
        worker_session_id: Some(runtime_id),
        // Human-created terminals get exactly the env they asked for.
        planner_hooks: false,
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

/// The longest `Idempotency-Key` any keyed route stores, in bytes (the key is ASCII). Every client
/// mints far less: the browser a 36-character UUID (`fe/web/src/app/router/idempotency-key.ts`),
/// the e2e scripts `uuid4`, the kernel's own Today summary `today-summary`. The cap bounds what a
/// binding row keeps for as long as its card or Track lives, with room for a prefixed UUID or ULID.
pub(crate) const IDEMPOTENCY_KEY_MAX_LEN: usize = 128;

/// The one `Idempotency-Key` parser every keyed route shares. A key that is not visible ASCII, is
/// blank, or is longer than [`IDEMPOTENCY_KEY_MAX_LEN`] is 400 `idempotency_key_invalid`.
pub(crate) fn parse_idempotency_key_header(headers: &HeaderMap) -> Result<Option<String>> {
    match headers.get("idempotency-key") {
        Some(value) => {
            let value = value.to_str().map_err(|_| {
                CalmError::IdempotencyKeyInvalid("the header holds non-ASCII bytes".into())
            })?;
            let value = value.trim();
            if value.is_empty() {
                return Err(CalmError::IdempotencyKeyInvalid(
                    "the header is empty".into(),
                ));
            }
            if value.len() > IDEMPOTENCY_KEY_MAX_LEN {
                return Err(CalmError::IdempotencyKeyInvalid(format!(
                    "the key is {} bytes; at most {IDEMPOTENCY_KEY_MAX_LEN} are accepted",
                    value.len()
                )));
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

// `pub` so the scheduler integration tests can construct idempotency-matched operations.
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

#[cfg(test)]
mod idempotency_key_header_tests {
    use super::*;
    use axum::http::HeaderValue;

    fn parse(value: HeaderValue) -> Result<Option<String>> {
        let mut headers = HeaderMap::new();
        headers.insert("idempotency-key", value);
        parse_idempotency_key_header(&headers)
    }

    #[test]
    fn a_key_up_to_the_cap_is_kept_and_one_byte_more_is_invalid() {
        let longest = "k".repeat(IDEMPOTENCY_KEY_MAX_LEN);
        assert_eq!(
            parse(HeaderValue::from_str(&longest).unwrap()).unwrap(),
            Some(longest.clone())
        );
        // The cap applies to the trimmed key, so surrounding blanks do not count.
        assert_eq!(
            parse(HeaderValue::from_str(&format!(" {longest} ")).unwrap()).unwrap(),
            Some(longest)
        );
        let refused =
            parse(HeaderValue::from_str(&"k".repeat(IDEMPOTENCY_KEY_MAX_LEN + 1)).unwrap())
                .unwrap_err();
        assert_eq!(refused.code(), "idempotency_key_invalid", "{refused:?}");
        assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn a_blank_or_non_ascii_key_is_invalid_and_no_key_is_none() {
        for value in [
            HeaderValue::from_static("   "),
            HeaderValue::from_bytes(b"\xff").unwrap(),
        ] {
            let refused = parse(value).unwrap_err();
            assert_eq!(refused.code(), "idempotency_key_invalid", "{refused:?}");
        }
        assert_eq!(
            parse_idempotency_key_header(&HeaderMap::new()).unwrap(),
            None
        );
    }
}
