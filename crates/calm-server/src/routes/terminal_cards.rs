//! `POST /api/tracks/:track_id/terminal-cards` — atomic terminal-card creation through
//! the operation runtime; non-idempotent unless the caller supplies `Idempotency-Key`.

use crate::actor::Actor;
use crate::error::{CalmError, ErrorBody, Result};
use crate::extract::{Json, JsonBody, Path};
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
    Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::{patch, post},
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/tracks/{track_id}/terminal-cards",
            post(create_terminal_card),
        )
        .route(
            "/api/cards/{id}/terminal-workbench",
            patch(set_terminal_workbench),
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
        (status = 201, description = "Card + linked terminal created atomically; typed execution admitted or Legacy daemon spawned", body = Card),
        (status = 400, description = "An `Idempotency-Key` blank, non-ASCII or over 128 bytes (`idempotency_key_invalid`)", body = ErrorBody),
        (status = 404, description = "Track not found, or the keyed Terminal was deleted or changed kind", body = ErrorBody),
        (status = 409, description = "`idempotency_key_reused`: the key names another request; `conflict`: refused before its commit. Both final for the key", body = ErrorBody),
        (status = 422, description = "Body missing required fields (e.g. theme)", body = ErrorBody),
        (status = 503, description = "`execution_unavailable`: typed execution is unavailable; no rows written", body = ErrorBody),
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
        source: calm_types::card_presentation::CardCreationSource::Manual,
        actor,
        worker_session_id: Some(runtime_id),
        // Human-created terminals get exactly the env they asked for.
        planner_hooks: false,
        request,
    })?;
    let key = OperationKey {
        operation_key,
        idempotency_key,
        payload_hash,
    };
    let created = crate::operation::terminal_execution_create::create(
        &s.operation_runtime,
        &s.events,
        s.terminal_execution,
        key,
        payload,
    )
    .await?;
    let answer = keyed_card_answer(s.repo.as_ref(), created.result.outcome.clone()).await?;
    if created.newly_admitted {
        let term = s
            .repo
            .terminal_get_by_card(answer.1.0.id.as_str())
            .await?
            .ok_or_else(|| CalmError::NotFound("new terminal".into()))?;
        let engine = s
            .operation_runtime
            .execution()
            .engine()
            .ok_or_else(|| CalmError::Conflict("execution engine unavailable".into()))?;
        // Install before driving: this renderer owns every geometry change from birth.
        crate::routes::terminal::attach_typed_terminal(
            s.repo.clone(),
            &s.terminal_renderer,
            std::sync::Arc::new(engine.clone()),
            &term,
            true,
            s.daemon.proc_supervisor_sock.clone().ok_or_else(|| {
                CalmError::Internal("typed supervisor socket is not configured".into())
            })?,
        )
        .await?;
    }
    crate::operation::terminal_execution_create::drive_result(
        s.operation_runtime.execution(),
        &created.result,
    )?;
    Ok(answer)
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TerminalWorkbenchBody {
    pub in_workbench: bool,
}

#[utoipa::path(
    patch, path = "/api/cards/{id}/terminal-workbench", tag = "cards",
    params(("id" = String, Path, description = "Terminal card id")),
    request_body = TerminalWorkbenchBody,
    responses(
        (status = 200, description = "Updated terminal placement; process and source unchanged", body = Card),
        (status = 400, description = "Not a terminal card or invalid body", body = ErrorBody),
        (status = 404, description = "Card not found", body = ErrorBody),
    ),
)]
pub(crate) async fn set_terminal_workbench(
    State(s): State<AppState>,
    actor: Actor,
    Path(id): Path<String>,
    JsonBody(body): JsonBody<TerminalWorkbenchBody>,
) -> Result<Json<Card>> {
    let current = s
        .repo
        .card_get(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("card {id}")))?;
    let scope =
        crate::routes::cards::card_scope(s.repo.as_ref(), current.id, current.track_id).await?;
    let (mut card, _) = crate::db::write_with_event_typed(
        s.repo.as_ref(),
        actor.to_actor_id(),
        scope,
        None,
        &s.events,
        s.write(),
        move |tx| {
            Box::pin(async move {
                let card = calm_truth::db::sqlite::terminal_presentation::set_workbench_tx(
                    tx,
                    &id,
                    body.in_workbench,
                )
                .await
                .map_err(CalmError::from)?;
                Ok((card.clone(), crate::event::Event::CardUpdated(card)))
            })
        },
    )
    .await?;
    crate::session_projection_lookup::project_runtime_into_card_payload(s.repo.as_ref(), &mut card)
        .await?;
    Ok(Json(card))
}

/// Creation receipts preserve their identity but must not overwrite later user placement.
/// Historical events are deliberately not projected through this live-resource response seam.
pub(crate) async fn project_live_terminal_presentation<R: crate::db::RepoRead + ?Sized>(
    repo: &R,
    card: &mut Card,
) -> Result<()> {
    if card.kind != "terminal" {
        return Ok(());
    }
    let current = repo
        .card_get(card.id.as_str())
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("terminal card {}", card.id)))?;
    if current.kind != "terminal" {
        return Err(CalmError::NotFound(format!("terminal card {}", card.id)));
    }
    let presentation =
        calm_types::card_presentation::TerminalPresentation::from_payload(&current.payload)
            .map_err(|error| CalmError::Internal(format!("terminal card {}: {error}", card.id)))?;
    if let Some(map) = card.payload.as_object_mut() {
        map.insert(
            calm_types::card_presentation::TERMINAL_PRESENTATION_PAYLOAD_KEY.into(),
            serde_json::to_value(presentation)?,
        );
    } else {
        // A pre-upgrade creation receipt may legitimately have had a null payload.
        card.payload = serde_json::json!({
            (calm_types::card_presentation::TERMINAL_PRESENTATION_PAYLOAD_KEY): presentation,
        });
    }
    Ok(())
}
