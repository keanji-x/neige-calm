//! `POST /api/tracks/:track_id/terminal-cards` — atomic terminal-card creation through
//! the operation runtime; non-idempotent unless the caller supplies `Idempotency-Key`.

use crate::actor::Actor;
use crate::error::{ErrorBody, Result};
use crate::json_body::JsonBody;
use crate::model::{Card, new_id};
use crate::operation::OperationKey;
use crate::operation::terminal_adapter::{
    TerminalCreateOperationPayload, TerminalCreateRequestPayload, normalize_terminal_create_request,
};
use crate::routes::idempotency_key::{
    keyed_card_answer, parse_idempotency_key_header, stable_payload_hash,
};
use crate::state::{AppState, RouteState};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::post,
};
use serde::{Deserialize, Serialize};
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
        (status = 409, description = "`idempotency_key_reused`: the key names another request; `conflict`: refused before its commit. Both final for the key", body = ErrorBody),
        (status = 422, description = "Body missing required fields (e.g. theme)", body = ErrorBody),
        (status = 500, description = "Final for this key: `operation_failed` (spawn failed, the saga rolled back), `operation_stuck` (the card may exist)", body = ErrorBody),
    ),
)]
#[allow(deprecated)]
pub(crate) async fn create_terminal_card(
    State(s): State<RouteState>,
    actor: Actor,
    headers: HeaderMap,
    Path(track_id): Path<String>,
    JsonBody(p): JsonBody<NewTerminalCardBody>,
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
    keyed_card_answer(
        s.repo.as_ref(),
        s.operation_runtime.wait(&op_id).await?.outcome,
    )
    .await
}
