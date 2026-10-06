//! `/api/cards`, `/api/tracks/:id/cards` — Card CRUD. The create route accepts an optional `via_tool_call` variant, which wins over the direct-create fields when both are sent.

use crate::actor::Actor;
use crate::db::RepoRead;
use crate::db::sqlite::{card_delete_tx, card_update_tx, terminal_delete_tx};
use crate::db::{write_with_actor_events_typed, write_with_event_typed};
use crate::error::{CalmError, ErrorBody, Result};
use crate::event::{Event, EventScope, RatifyDecision};
use crate::git_candidate::delivery::AttemptOutcome;
use crate::ids::{ActorId, CardId, TrackId};
use crate::json_body::JsonBody;
use crate::model::{Card, CardPatch, CardRole, HarnessItem, new_id};
use crate::operation::card_create_adapter::{CARD_CREATE, CardCreateOperationPayload};
use crate::operation::workspace_lease::{ReleaseDelivery, release_workspace_lease_for_card_tx};
use crate::operation::{OperationId, OperationKey};
use crate::per_card_lock::lock_key;
use crate::plugin_host::callbacks::extract_card_creation_from_tool_call_result;
use crate::ratify_state::ratify_request_pending_tx;
use crate::routes::idempotency_key::{
    keyed_card_answer, parse_idempotency_key_header, stable_payload_hash,
};
use crate::routes::planner_cards::{
    card_runs_headless_harness, get_planner_run, interrupt_planner_card,
    interrupt_shared_card_active_turn, reset_planner_card,
};
use crate::session_projection_lookup::{
    project_runtime_into_card_payload, project_runtime_into_cards_payload,
};
use crate::state::{AppState, CodexShellState, RouteState, WorkerState};
use crate::terminal_sweeper::reap_terminal_artifacts_with_renderer;
use crate::validation::reject_client_supplied_server_owned_keys;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use utoipa::{IntoParams, ToSchema};

/// Resolve the (track, area) ancestor pair for a track id into an `EventScope::Card`.
/// Do not call from inside a transaction that has written `tracks`: this reads through the pool (a second connection) and the task deadlocks against its own lock. Use `card_scope_tx`.
pub(crate) async fn card_scope(
    repo: &dyn RepoRead,
    card: CardId,
    track: TrackId,
) -> Result<EventScope> {
    let w = repo
        .track_get(track.as_str())
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("track {track}")))?;
    Ok(EventScope::Card {
        card,
        track: w.id,
        area: w.area_id,
    })
}

/// The in-transaction twin of [`card_scope`].
/// A transaction must not read, off the pool, any table it has itself written, before it commits: under SQLite's shared cache (every in-memory test DB) the read blocks on a lock only the caller can release; under WAL production gets a pre-write read instead. Every terminal-creating transaction writes `tracks`, `cards` and `terminals`.
/// Nothing scans for violations; any `prepare_tx` that mints a card + terminal resolves its scope through this function, or before the write.
pub(crate) async fn card_scope_tx(
    tx: &mut crate::operation::Tx<'_>,
    card: CardId,
    track: TrackId,
) -> Result<EventScope> {
    let area: Option<(String,)> = sqlx::query_as("SELECT area_id FROM tracks WHERE id = ?1")
        .bind(track.as_str())
        .fetch_optional(&mut **tx)
        .await?;
    let (area,) = area.ok_or_else(|| CalmError::NotFound(format!("track {track}")))?;
    Ok(EventScope::Card {
        card,
        track,
        area: area.into(),
    })
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/tracks/{track_id}/cards",
            get(list_cards_by_track).post(create_card),
        )
        .route(
            "/api/cards/{id}",
            axum::routing::patch(update_card).delete(delete_card),
        )
        .route("/api/cards/{id}/harness/items", get(get_harness_items))
        .route(
            "/api/cards/{id}/harness/live",
            get(crate::routes::harness_live::get_harness_live),
        )
        .route(
            "/api/cards/{id}/planner/input",
            post(crate::routes::planner_input_send::send_planner_input),
        )
        // Mounted here because this router owns `/api/cards/{id}/**`; a second router on the same prefix is how two mounts start disagreeing about a middleware.
        .route(
            "/api/cards/{id}/planner/input/{entry_id}",
            axum::routing::patch(crate::routes::planner_input::edit_planner_input)
                .delete(crate::routes::planner_input::delete_planner_input),
        )
        .route(
            "/api/cards/{id}/planner/input/{entry_id}/steer",
            post(crate::routes::planner_input::steer_planner_input),
        )
        .route("/api/cards/{id}/ratify", post(ratify_card))
        .route(
            "/api/cards/{id}/planner/interrupt",
            post(interrupt_planner_card),
        )
        .route(
            "/api/cards/{id}/planner/model",
            axum::routing::put(crate::routes::planner_model::set_planner_model),
        )
        .route("/api/cards/{id}/planner/run", get(get_planner_run))
        .route("/api/cards/{id}/planner/reset", post(reset_planner_card))
}

#[utoipa::path(
    get,
    path = "/api/tracks/{track_id}/cards",
    tag = "cards",
    params(("track_id" = String, Path, description = "Track id")),
    responses(
        (status = 200, description = "Cards in track (sorted)", body = Vec<Card>),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn list_cards_by_track(
    State(s): State<RouteState>,
    Path(track_id): Path<String>,
) -> Result<Json<Vec<Card>>> {
    let mut cards = s.repo.cards_by_track(&track_id).await?;
    project_runtime_into_cards_payload(s.repo.as_ref(), &mut cards).await?;
    Ok(Json(cards))
}

#[derive(Debug, Clone, Copy, Default, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum HarnessItemsDirection {
    #[default]
    Asc,
    Desc,
}

#[derive(Debug, Deserialize, IntoParams, ToSchema)]
pub struct HarnessItemsQuery {
    /// Return items with database ids greater than this value.
    #[serde(default)]
    pub after_id: Option<i64>,
    /// Maximum number of rows to return. Defaults to 100 and is capped at 500.
    #[serde(default)]
    pub limit: Option<i64>,
    /// Fetch the oldest (`asc`) or latest (`desc`) matching rows. Defaults to `asc`.
    #[serde(default)]
    pub direction: HarnessItemsDirection,
}

#[utoipa::path(
    get,
    path = "/api/cards/{id}/harness/items",
    tag = "cards",
    params(
        ("id" = String, Path, description = "Planner card id"),
        HarnessItemsQuery,
    ),
    responses(
        (status = 200, description = "Persisted planner harness items", body = Vec<HarnessItem>),
        (status = 403, description = "Card is not a planner codex card", body = ErrorBody),
        (status = 404, description = "Card not found", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn get_harness_items(
    State(s): State<RouteState>,
    Path(id): Path<String>,
    Query(q): Query<HarnessItemsQuery>,
) -> Result<Json<Vec<HarnessItem>>> {
    let card = harness_card(&s, &id).await?;
    let after_id = q.after_id.unwrap_or(0).max(0);
    let limit = q.limit.unwrap_or(100).clamp(0, 500);
    let descending = q.direction == HarnessItemsDirection::Desc;
    // The transcript-only read: `limit` is the frontend's page budget and must be spent on rows the transcript renders, not captured `turn/plan/updated` frames.
    let mut items = s
        .repo
        .harness_item_list_transcript_by_card(card.id.as_str(), after_id, limit, descending)
        .await?;
    // Redacted at the serialization boundary, not at the write: the stored blob is a verbatim record of what codex sent. Attachments reach the transcript as an id and a server-built url.
    for item in &mut items {
        item.params = crate::planner_attachments::redact_local_image_paths(&item.params);
    }
    Ok(Json(items))
}

/// The card a `harness/*` read names, refused unless it runs a headless Planner harness.
pub(crate) async fn harness_card(s: &RouteState, id: &str) -> Result<Card> {
    let card = s
        .repo
        .card_get(id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("card {id}")))?;
    let role = s
        .write
        .verify_role(&card.id)
        .ok_or_else(|| CalmError::NotFound(format!("card {id}")))?;
    if !card_runs_headless_harness(&card, role) {
        return Err(CalmError::Forbidden(format!(
            "card {id} is not a planner codex card",
        )));
    }
    Ok(card)
}

/// Body payload accepted by `POST /api/tracks/:track_id/cards`: direct create (`kind`, `sort`, `payload`, `title`) or `via_tool_call`, which wins when both are sent.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CreateCardBody {
    /// Legacy direct-create fields; `track_id` comes from the path.
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub sort: Option<f64>,
    #[serde(default)]
    #[schema(value_type = Option<Object>)]
    pub payload: Option<Value>,
    #[serde(default)]
    pub title: Option<String>,
    /// When present, the kernel calls the plugin and the `kind` / `payload` fields above are ignored.
    #[serde(default)]
    pub via_tool_call: Option<ViaToolCall>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ViaToolCall {
    pub plugin_id: String,
    pub tool_name: String,
    #[serde(default)]
    #[schema(value_type = Object)]
    pub arguments: Value,
}

#[utoipa::path(
    post,
    path = "/api/tracks/{track_id}/cards",
    tag = "cards",
    params(
        ("track_id" = String, Path, description = "Track id this card belongs to"),
        ("Idempotency-Key" = Option<String>, Header, description = "Optional; the key binds at most one card. A retry before the card is stored may call the tool again."),
    ),
    request_body = CreateCardBody,
    responses(
        (status = 201, description = "Card created", body = Card),
        (status = 400, description = "Missing `kind` and no `via_tool_call`, or a blank, non-ASCII or over-long `Idempotency-Key` (`idempotency_key_invalid`)", body = ErrorBody),
        (status = 403, description = "Plugin lacks `permissions.cards_create`", body = ErrorBody),
        (status = 404, description = "Track not found, or plugin not running / not in registry", body = ErrorBody),
        (status = 409, description = "`idempotency_key_reused`: the key names another request; `conflict`: refused, and the key binds nothing", body = ErrorBody),
        (status = 422, description = "Tool returned no `_meta.ui.resourceUri`", body = ErrorBody),
        (status = 502, description = "Plugin tool call failed", body = ErrorBody),
        (status = 500, description = "Internal error; final for this key: `operation_failed` (the create failed), `operation_stuck` (the card may exist)", body = ErrorBody),
    ),
)]
#[allow(deprecated)]
#[allow(clippy::result_large_err)]
pub(crate) async fn create_card(
    State(s): State<AppState>,
    State(route): State<RouteState>,
    actor: Actor,
    headers: HeaderMap,
    Path(track_id): Path<String>,
    JsonBody(body): JsonBody<CreateCardBody>,
) -> Result<Response, Response> {
    let key = OperationKey {
        operation_key: new_id(),
        idempotency_key: parse_idempotency_key_header(&headers)
            .map_err(IntoResponse::into_response)?,
        // The request as sent: what a retry under the key must repeat to be answered by the first attempt.
        payload_hash: stable_payload_hash(&json!({
            "actor": actor.as_str(),
            "track_id": &track_id,
            "request": &body,
        }))
        .map_err(IntoResponse::into_response)?,
    };
    // The tool-call branch overrides the actor to `plugin:<id>` regardless of any `X-Calm-Actor` header — plugins cannot spoof their own actor via REST.
    if let Some(via) = body.via_tool_call {
        return create_via_tool_call(&s, &route, track_id, via, key).await;
    }

    let kind = body.kind.ok_or_else(|| {
        CalmError::BadRequest("create card body needs either `kind` or `via_tool_call`".into())
            .into_response()
    })?;
    let payload = body.payload.unwrap_or(Value::Null);
    // Server-owned payload keys are kernel-stamped, never accepted from a client.
    reject_client_supplied_server_owned_keys(&payload)
        .map_err(|e| CalmError::from(e).into_response())?;
    // Plugin-defined (`ui://*`) kinds remain opaque.
    s.card_kind_registry()
        .validate_payload(&kind, &payload)
        .map_err(|e| CalmError::from(e).into_response())?;
    let card = CardCreateOperationPayload {
        actor: actor.to_actor_id(),
        correlation: None,
        track_id,
        kind,
        sort: body.sort,
        payload,
        title: body.title,
    };
    commit_card_create(&s, key, card)
        .await
        .map_err(IntoResponse::into_response)
}

/// Kernel invokes `tools/call` on the plugin, then writes a Card row keyed off `_meta.ui.resourceUri`.
/// plugin not running → 404; `permissions.cards_create` not granted → 403; `isError: true` → 502; no `_meta.ui.resourceUri` → 422 `not_a_card_tool`.
/// A key that already holds a card is answered with it before any of that.
#[allow(deprecated)]
#[allow(clippy::result_large_err)]
async fn create_via_tool_call(
    s: &AppState,
    route: &RouteState,
    track_id: String,
    via: ViaToolCall,
    key: OperationKey,
) -> Result<Response, Response> {
    // Held from the replay check through the commit, so a concurrent retry under the key in this
    // process waits for this request and is answered by its card. The key binds at most one card; a
    // retry before the card is stored (lost answer, tool failure, a second process) may call the
    // tool again.
    let _same_key = match key.idempotency_key.as_deref() {
        Some(idempotency_key) => Some(
            lock_key(
                &route.conversation_first_message_locks,
                &format!("{CARD_CREATE}:{idempotency_key}"),
            )
            .await,
        ),
        None => None,
    };
    if let Some(op_id) = s
        .operation_runtime
        .keyed_replay(CARD_CREATE, &key)
        .await
        .map_err(IntoResponse::into_response)?
    {
        return card_create_answer(s, op_id)
            .await
            .map_err(IntoResponse::into_response);
    }

    // 1. Plugin must be a RUNNING `app`. Card creation is stdio-only: it depends on the plugin owning a `ui://` view, which a connector structurally cannot. A Running connector must not be told it 'is not running'.
    let mcp = match s.plugin.mcp_client(&via.plugin_id).await {
        Some(c) => c,
        None => {
            if let Some(client) = s.plugin.connector_client(&via.plugin_id).await {
                return Err(CalmError::BadRequest(format!(
                    "plugin `{}` is a `{}` connector; connectors cannot create cards \
                     (no `ui://` view to bind a card to)",
                    via.plugin_id,
                    client.variant_name()
                ))
                .into_response());
            }
            return Err(
                CalmError::NotFound(format!("plugin `{}` is not running", via.plugin_id))
                    .into_response(),
            );
        }
    };

    // 2. Manifest-based permission gate, mirroring the autonomous `neige.card.create` gate in `callbacks.rs`.
    let perms = match s.plugin.registry().get(&via.plugin_id) {
        Some(m) => m.permissions,
        None => {
            return Err(
                CalmError::NotFound(format!("plugin `{}` not in registry", via.plugin_id))
                    .into_response(),
            );
        }
    };
    if !perms.cards_create {
        return Err(CalmError::PluginPermission(format!(
            "plugin `{}` lacks permissions.cards_create",
            via.plugin_id
        ))
        .into_response());
    }

    // 3. Invoke the tool. Transport-level failures propagate as 502.
    let result = mcp
        // No Track: an iframe asking its own plugin to mint a card already names the destination in `via`.
        .tools_call(&via.tool_name, via.arguments, None, None)
        .await
        .map_err(|e| tool_call_bad_gateway(&via.plugin_id, &via.tool_name, &e.to_string()))?;

    // 4. Tool-reported failure (`isError: true`) → 502.
    if matches!(result.is_error, Some(true)) {
        let joined = result
            .content
            .iter()
            .filter_map(|b| b.text.as_deref())
            .collect::<Vec<_>>()
            .join("\n");
        let msg = if joined.is_empty() {
            "plugin tool returned isError without content".to_string()
        } else {
            joined
        };
        return Err(tool_call_bad_gateway(&via.plugin_id, &via.tool_name, &msg));
    }

    // 5. Pull `_meta.ui.resourceUri`. Absent → 422.
    let creation = match extract_card_creation_from_tool_call_result(&result) {
        Some(c) => c,
        None => {
            let body = json!({
                "error": "tool did not return _meta.ui.resourceUri",
                "code": "not_a_card_tool",
            });
            return Err((StatusCode::UNPROCESSABLE_ENTITY, Json(body)).into_response());
        }
    };

    // 6. Persist. `kind` is the bare `ui://...` URI; `payload` defaults to null.
    let payload = creation.structured_content.unwrap_or(Value::Null);
    // A plugin's `structuredContent` is client input here: server-owned keys are never accepted from it.
    reject_client_supplied_server_owned_keys(&payload)
        .map_err(|e| CalmError::from(e).into_response())?;
    // `ui://*` kinds are opaque, but a tool naming a kernel kind via resourceUri is rejected here rather than after the DB write.
    s.card_kind_registry()
        .validate_payload(&creation.resource_uri, &payload)
        .map_err(|e| CalmError::from(e).into_response())?;
    // Actor stays `Plugin(<id>)`; `correlation` records the user-driven invocation so audit queries can reconstruct the causal chain.
    let card = CardCreateOperationPayload {
        actor: ActorId::Plugin(via.plugin_id.clone()),
        correlation: Some(format!("user_tool_call:{}", via.tool_name)),
        track_id,
        kind: creation.resource_uri,
        sort: None,
        payload,
        title: None,
    };
    commit_card_create(s, key, card)
        .await
        .map_err(IntoResponse::into_response)
}

/// Commit the card's one write under the request's key and answer with what that key holds.
async fn commit_card_create(
    s: &AppState,
    key: OperationKey,
    card: CardCreateOperationPayload,
) -> Result<Response> {
    let committed = s
        .operation_runtime
        .commit_keyed(CARD_CREATE, key, serde_json::to_value(card)?)
        .await?;
    Ok(keyed_card_answer(s.repo.as_ref(), committed.outcome)
        .await?
        .into_response())
}

/// The stored card (with its runtime projected), or the stored failure, of a `card-create` operation.
async fn card_create_answer(s: &AppState, op_id: OperationId) -> Result<Response> {
    let outcome = s.operation_runtime.wait(&op_id).await?.outcome;
    Ok(keyed_card_answer(s.repo.as_ref(), outcome)
        .await?
        .into_response())
}

fn tool_call_bad_gateway(plugin_id: &str, tool_name: &str, detail: &str) -> Response {
    let body = json!({
        "error": format!("plugin `{plugin_id}` tool `{tool_name}` failed: {detail}"),
        "code": "tool_call_failed",
    });
    (StatusCode::BAD_GATEWAY, Json(body)).into_response()
}

#[utoipa::path(
    patch,
    path = "/api/cards/{id}",
    tag = "cards",
    params(("id" = String, Path, description = "Card id")),
    request_body = CardPatch,
    responses(
        (status = 200, description = "Card updated", body = Card),
        (status = 400, description = "Card patch violates an invariant (see `CardPatch.payload`)", body = ErrorBody),
        (status = 404, description = "Card not found", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn update_card(
    State(s): State<AppState>,
    actor: Actor,
    Path(id): Path<String>,
    JsonBody(p): JsonBody<CardPatch>,
) -> Result<Json<Card>> {
    // `deletable` is a kernel-owned bit, not patchable; rejected loudly with 400 so a client doesn't think it silently updated.
    if p.deletable.is_some() {
        return Err(CalmError::BadRequest(
            "`deletable` is a kernel-managed field and cannot be patched via API".into(),
        ));
    }
    // The existing card's track_id is needed for the EventScope chain regardless.
    let existing = s
        .repo
        .card_get(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("card {id}")))?;
    // Validate the payload against the kind that will land in the DB. A server-owned key in the payload is a 400 for any kind; `card_update_tx` re-stamps every stored server-owned key onto any replacement payload.
    if let Some(payload) = p.payload.as_ref() {
        reject_client_supplied_server_owned_keys(payload)?;
        let kind = p.kind.as_deref().unwrap_or(existing.kind.as_str());
        s.card_kind_registry().validate_payload(kind, payload)?;
    }
    let scope = card_scope(s.repo.as_ref(), existing.id.clone(), existing.track_id).await?;
    let (mut card, _id) = write_with_event_typed(
        s.repo.as_ref(),
        actor.to_actor_id(),
        scope,
        None,
        &s.events,
        s.write(),
        move |tx| {
            Box::pin(async move {
                let card = card_update_tx(tx, &id, p).await?;
                Ok((card.clone(), Event::CardUpdated(card)))
            })
        },
    )
    .await?;
    project_runtime_into_card_payload(s.repo.as_ref(), &mut card).await?;
    Ok(Json(card))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RatifyCardRequest {
    pub decision: RatifyCardDecision,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RatifyCardDecision {
    Grant,
    Deny,
}

impl From<RatifyCardDecision> for RatifyDecision {
    fn from(value: RatifyCardDecision) -> Self {
        match value {
            RatifyCardDecision::Grant => RatifyDecision::Grant,
            RatifyCardDecision::Deny => RatifyDecision::Deny,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct RatifyCardResponse {
    #[schema(value_type = String)]
    pub card_id: CardId,
    #[schema(value_type = String)]
    pub track_id: TrackId,
    pub decision: RatifyCardDecision,
}

#[utoipa::path(
    post,
    path = "/api/cards/{id}/ratify",
    tag = "cards",
    params(("id" = String, Path, description = "Planner card id")),
    request_body = RatifyCardRequest,
    responses(
        (status = 200, description = "Human ratify verdict recorded", body = RatifyCardResponse),
        (status = 400, description = "Malformed request", body = ErrorBody),
        (status = 403, description = "Card is not a planner codex card, or actor is not the authenticated user", body = ErrorBody),
        (status = 404, description = "Card or track not found", body = ErrorBody),
        (status = 409, description = "Track is not awaiting ratification", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn ratify_card(
    State(s): State<RouteState>,
    actor: Actor,
    Path(id): Path<String>,
    JsonBody(body): JsonBody<RatifyCardRequest>,
) -> Result<Json<RatifyCardResponse>> {
    if actor.as_str() != "user" {
        return Err(CalmError::Forbidden(
            "ratify verdicts must be authored by the authenticated user".into(),
        ));
    }

    let card = s
        .repo
        .card_get(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("card {id}")))?;
    let role = s
        .write
        .verify_role(&card.id)
        .ok_or_else(|| CalmError::NotFound(format!("card {id}")))?;
    if card.kind != "codex" || role != CardRole::Planner {
        return Err(CalmError::Forbidden(format!(
            "card {id} is not a planner codex card",
        )));
    }
    let track = s
        .repo
        .track_get(card.track_id.as_str())
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("track {} for card {id}", card.track_id)))?;

    let actor_id = ActorId::User;
    let scope = EventScope::Track {
        track: track.id.clone(),
        area: track.area_id.clone(),
    };
    let track_id = track.id.clone();
    let card_id = card.id.clone();
    let decision = body.decision;
    let resolved_message = body
        .message
        .as_deref()
        .map(str::trim)
        .filter(|message| !message.is_empty())
        .map(str::to_string);

    write_with_actor_events_typed::<(), _>(s.repo.as_ref(), None, &s.events, &s.write, move |tx| {
        let actor_id = actor_id.clone();
        let scope = scope.clone();
        let track_id = track_id.clone();
        let resolved_message = resolved_message.clone();
        Box::pin(async move {
            if !ratify_request_pending_tx(tx, &track_id).await? {
                return Err(CalmError::Conflict(
                    "ratify: track is not awaiting ratification".into(),
                ));
            }

            Ok((
                (),
                vec![(
                    actor_id,
                    scope,
                    Event::RatifyResolved {
                        track_id,
                        decision: decision.into(),
                        message: resolved_message,
                    },
                )],
            ))
        })
    })
    .await?;

    Ok(Json(RatifyCardResponse {
        card_id,
        track_id: track.id,
        decision,
    }))
}

#[utoipa::path(
    delete,
    path = "/api/cards/{id}",
    tag = "cards",
    params(("id" = String, Path, description = "Card id")),
    responses(
        (status = 204, description = "Card deleted"),
        (status = 403, description = "The card is kernel-owned (`deletable = false`); delete its track instead", body = ErrorBody),
        (status = 404, description = "Card not found", body = ErrorBody),
        (status = 409, description = "The card's terminal launch is not resolved yet (`conflict`)", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
#[allow(deprecated)]
pub(crate) async fn delete_card(
    State(s): State<RouteState>,
    State(w): State<WorkerState>,
    State(cs): State<CodexShellState>,
    actor: Actor,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    let _operation_guard = s.operation_runtime.lock_for_track_delete().await;
    let card = s
        .repo
        .card_get(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("card {id}")))?;
    // Kernel-owned cards carry `deletable = false`; refuse direct REST delete. Track delete still cascades through the FK chain.
    if !card.deletable {
        return Err(CalmError::Forbidden(format!(
            "card {id} is kernel-owned and cannot be deleted via this endpoint; \
             delete the parent track to remove it",
        )));
    }
    let _delete_guard =
        crate::per_card_lock::lock_key(&s.track_delete_locks, card.track_id.as_str()).await;
    let card = s
        .repo
        .card_get(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("card {id}")))?;
    crate::operation::terminal_disposal::require_safe(
        s.repo.as_ref(),
        crate::operation::terminal_disposal::Scope::Card(id.clone()),
        w.daemon.proc_supervisor_sock.as_deref(),
    )
    .await?;
    let card_id = card.id.clone();
    let track_id = card.track_id.clone();
    let scope = card_scope(s.repo.as_ref(), card_id.clone(), track_id.clone()).await?;

    interrupt_shared_card_active_turn(s.repo.as_ref(), &cs, &card).await;

    // Eager teardown: `terminals.card_id` is `ON DELETE RESTRICT`, so the terminal row must be removed, and its daemon + socket reaped, before the card row delete. Cleanup runs outside the write txn; if it fails the row stays and the sweeper retries next tick. The txn then deletes both rows in one commit under `Event::CardDeleted`.
    let term = s.repo.terminal_get_by_card(card_id.as_str()).await?;
    if let Some(t) = term.as_ref() {
        reap_terminal_artifacts_with_renderer(Some(w.terminal_renderer.as_ref()), t).await;
    }
    let terminal_id = term.map(|t| t.id);

    let write_for_tx = s.write.clone();
    let delete_actor = actor.to_actor_id();
    let (_unit, _ids) =
        write_with_actor_events_typed(s.repo.as_ref(), None, &s.events, &s.write, move |tx| {
            Box::pin(async move {
                crate::operation::terminal_disposal::require_safe_tx(
                    tx,
                    &crate::operation::terminal_disposal::Scope::Card(card_id.to_string()),
                )
                .await?;
                // Drop the terminal row first so the RESTRICT FK lets the card delete through. NotFound is OK (the sweeper may have raced us).
                if let Some(tid) = terminal_id.as_deref() {
                    match terminal_delete_tx(tx, tid).await.map_err(CalmError::from) {
                        Ok(()) => {}
                        Err(CalmError::NotFound(_)) => {}
                        Err(e) => return Err(e),
                    }
                }
                // #1830 S2 D7: after the best-effort interrupt above, the attempt is committed as
                // `interrupted` in this delete transaction.
                let mut events =
                    crate::scheduler::fail_tasks_for_deleted_card_tx(tx, &card).await?;
                events.extend(
                    release_workspace_lease_for_card_tx(
                        tx,
                        card_id.as_ref(),
                        ReleaseDelivery::Commit(AttemptOutcome::Interrupted),
                    )
                    .await?,
                );
                card_delete_tx(tx, card_id.as_ref(), write_for_tx.role_cache()).await?;
                events.push((
                    delete_actor,
                    scope,
                    Event::CardDeleted {
                        id: card_id,
                        track_id,
                    },
                ));
                Ok(((), events))
            })
        })
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
