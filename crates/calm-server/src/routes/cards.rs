//! `/api/cards`, `/api/tracks/:id/cards` — Card CRUD. The create route accepts an optional `via_tool_call` variant, which wins over the direct-create fields when both are sent.

use crate::actor::Actor;
use crate::db::sqlite::{card_delete_tx, card_update_tx, terminal_delete_tx};
use crate::db::{RepoRead, RouteRepo};
use crate::db::{write_with_actor_events_typed, write_with_event_typed};
use crate::error::{CalmError, ErrorBody, Result};
use crate::event::{Event, EventScope, RatifyDecision};
use crate::git_candidate::delivery::AttemptOutcome;
use crate::harness::{HarnessPhaseTag, QueueEntry, RunningTurn, TokenUsage};
use crate::ids::{ActorId, CardId, TrackId};
use crate::json_body::JsonBody;
use crate::model::{Card, CardPatch, CardRole, HarnessItem, Track, new_id};
use crate::operation::card_create_adapter::{CARD_CREATE, CardCreateOperationPayload};
use crate::operation::planner_harness_interrupt_adapter::PlannerHarnessInterruptOperationPayload;
use crate::operation::planner_harness_shutdown_adapter::PlannerHarnessShutdownOperationPayload;
use crate::operation::planner_harness_start_adapter::{
    HarnessProfile, PlannerHarnessStartOperationPayload,
};
use crate::operation::workspace_lease::{ReleaseDelivery, release_workspace_lease_for_card_tx};
use crate::operation::{OperationId, OperationKey, OperationOutcome};
use crate::per_card_lock::{lock_card, lock_key};
use crate::plugin_host::callbacks::extract_card_creation_from_tool_call_result;
use crate::ratify_state::ratify_request_pending_tx;
use crate::routes::idempotency_key::{
    calm_error_from_operation_failure, keyed_card_answer, parse_idempotency_key_header,
    stable_payload_hash,
};
use crate::session_projection_lookup::{
    card_is_shared_planner, project_runtime_into_card_payload, project_runtime_into_cards_payload,
};
use crate::session_projection_repo::WorkerSessionProjection;
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
use calm_types::planner_attachment::PlannerAttachment;
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

/// Whether the persisted card shape is allowed to use the headless harness routes. Unknown/malformed profile values fail closed.
pub(crate) fn card_runs_headless_harness(card: &Card, role: CardRole) -> bool {
    crate::harness::profile::PlannerBinding::from_card(card, role).is_some()
}

pub(crate) async fn interrupt_shared_card_active_turn(
    repo: &dyn RouteRepo,
    cs: &CodexShellState,
    card: &Card,
) {
    let active_runtime = match repo
        .session_projection_active_for_card(&card.id.to_string())
        .await
    {
        Ok(runtime) => runtime,
        Err(e) => {
            tracing::warn!(
                target: "session_projection_lookup::fallback",
                card_id = %card.id,
                error = %e,
                "runtime shared-card discriminator query failed; falling back to card payload"
            );
            None
        }
    };
    if !card_is_shared_planner(card, active_runtime.as_ref()) {
        return;
    }
    if let Err(e) = cs
        .shared_codex_appserver
        .interrupt_active_turn_for_card(card.id.as_str())
        .await
    {
        tracing::warn!(
            target: "shared_codex_daemon::orphan_turn",
            card_id = %card.id,
            track_id = %card.track_id,
            error = %e,
            "failed to interrupt active shared codex turn during card teardown"
        );
    }
}

/// Deletion-grade form of [`interrupt_shared_card_active_turn`]: every failure is propagated, since a destructive workspace move may only follow a confirmed quiesce.
pub(crate) async fn quiesce_shared_card_active_turn(
    s: &RouteState,
    cs: &CodexShellState,
    card: &Card,
) -> Result<Option<String>> {
    let active_runtime = s
        .repo
        .session_projection_active_for_card(&card.id.to_string())
        .await?;
    if card_is_shared_planner(card, active_runtime.as_ref()) {
        let thread_id = active_runtime
            .as_ref()
            .and_then(crate::harness::effective_runtime_thread_id);
        let mut seals = crate::thread_seals::DeletionThreadSeals::new(s.thread_seals.clone());
        if let Some(thread_id) = thread_id.clone() {
            seals.seal(thread_id);
        }
        if let Some(thread_id) = thread_id.as_deref() {
            cs.shared_codex_appserver
                .interrupt_active_turn(thread_id)
                .await?;
        }
        return Ok(seals.retain().pop());
    }
    Ok(None)
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
        ("Idempotency-Key" = Option<String>, Header, description = "Optional; without one a retry creates another card. A retry under the key returns its card and calls no tool again."),
    ),
    request_body = CreateCardBody,
    responses(
        (status = 201, description = "Card created", body = Card),
        (status = 400, description = "Missing `kind` and no `via_tool_call`, or a blank, non-ASCII or over-long `Idempotency-Key` (`idempotency_key_invalid`)", body = ErrorBody),
        (status = 403, description = "Plugin lacks `permissions.cards_create`", body = ErrorBody),
        (status = 404, description = "Track not found, or plugin not running / not in registry", body = ErrorBody),
        (status = 409, description = "`idempotency_key_reused`: the key names another request; `conflict`: refused before its commit. Both final for the key", body = ErrorBody),
        (status = 422, description = "Tool returned no `_meta.ui.resourceUri`", body = ErrorBody),
        (status = 502, description = "Plugin tool call failed", body = ErrorBody),
        (status = 500, description = "Internal error; `operation_failed` when the create under this key failed and is final for it", body = ErrorBody),
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
    submit_card_create(&s, key, card)
        .await
        .map_err(IntoResponse::into_response)
}

/// Kernel invokes `tools/call` on the plugin, then writes a Card row keyed off `_meta.ui.resourceUri`.
/// plugin not running → 404; `permissions.cards_create` not granted → 403; `isError: true` → 502; no `_meta.ui.resourceUri` → 422 `not_a_card_tool`.
/// A key that already holds a card is answered with it before any of that, so the tool runs once per key.
#[allow(deprecated)]
#[allow(clippy::result_large_err)]
async fn create_via_tool_call(
    s: &AppState,
    route: &RouteState,
    track_id: String,
    via: ViaToolCall,
    key: OperationKey,
) -> Result<Response, Response> {
    // Held from the replay check through the submit, so a concurrent retry under the key waits for
    // this request and is answered by its card instead of calling the tool again. In-process only;
    // the `operations` UNIQUE index is the cross-process wall (a second tool call, one card).
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
    submit_card_create(s, key, card)
        .await
        .map_err(IntoResponse::into_response)
}

/// Submit the card's one write under the request's key and answer with what that key holds.
async fn submit_card_create(
    s: &AppState,
    key: OperationKey,
    card: CardCreateOperationPayload,
) -> Result<Response> {
    let op_id = s
        .operation_runtime
        .submit(CARD_CREATE, key, serde_json::to_value(card)?)
        .await?;
    card_create_answer(s, op_id).await
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

#[derive(Debug, Serialize, ToSchema)]
pub struct ResetPlannerCardResponse {
    #[schema(value_type = String)]
    pub card_id: CardId,
    pub terminal_id: String,
    pub new_thread_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track: Option<Track>,
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

#[derive(Debug, Serialize, ToSchema)]
pub struct InterruptPlannerCardResponse {
    #[schema(value_type = String)]
    pub card_id: CardId,
    pub worker_session_id: String,
    /// True when a turn was running and an interrupt was dispatched at it; false when idle or a `turn/start` was still in flight. Means the interrupt was *issued* — completion is asynchronous.
    pub stopped: bool,
}

/// Current planner-harness run snapshot for a card, so a page opened mid-turn can seed its phase. Dormancy is NOT an error: it is the `{worker_session_id: null, phase: null}` answer.
#[derive(Debug, Serialize, ToSchema)]
pub struct GetPlannerRunResponse {
    #[schema(value_type = String)]
    pub card_id: CardId,
    /// Active worker-session id, or null when the harness is dormant.
    pub worker_session_id: Option<String>,
    /// Current harness phase, or null when the harness is dormant.
    pub phase: Option<HarnessPhaseTag>,
    /// Latest context-window usage, or null when the harness is dormant or codex has not pushed a `thread/tokenUsage/updated` frame yet. A dormant conversation reports `null` even though the reading is on disk.
    pub token_usage: Option<PlannerRunTokenUsage>,
    /// The model slug this conversation's turns run with, or `null` to follow the installation default. Read off the CARD, so answered for a dormant conversation too.
    pub model: Option<String>,
    /// The chosen reasoning effort, or `null` for the default. Same source as `model`.
    pub reasoning_effort: Option<String>,
    /// Why this conversation's queue is not draining, or `null` when there is nothing worth saying: an undeterminable model/effort, a refused turn start, or a long codex outage. A client should render it as a standing notice, not a request error.
    /// A brief outage fills nothing, so `null` is not evidence that anything succeeded.
    pub blocked_reason: Option<String>,
    /// The addressable user entries still waiting for the next turn, in queue order. Empty when dormant. Dispatcher observations and pre-id user entries never appear; the latter are counted in `pending_overflow`.
    pub pending: Vec<PendingQueueEntry>,
    /// User-authored entries that exist in the queue but are NOT in `pending`: pre-id entries, plus anything past the page budget.
    pub pending_overflow: u32,
    /// Whether this card can take image attachments at all: not when the track's workspace is an attached directory, since neige never writes into one. Answered by the same function the upload runs (`planner_attachments::attachment_root`).
    pub attachments_supported: bool,
    /// The turn the harness is running. Non-null exactly when this response's `phase` is `turn_running`: both come from one read of the harness state.
    pub running_turn: Option<PlannerRunningTurn>,
}

/// A running turn and how long it has run, by the harness's monotonic clock since it accepted that turn's `TurnStarted`. A duplicate or stale start never resets it. Not a wall-clock time and not persisted: a client anchors it to when the response arrived.
#[derive(Debug, Serialize, ToSchema)]
pub struct PlannerRunningTurn {
    pub turn_id: String,
    pub elapsed_ms: u64,
}

impl From<RunningTurn> for PlannerRunningTurn {
    fn from(running: RunningTurn) -> Self {
        Self {
            turn_id: running.turn_id,
            elapsed_ms: u64::try_from(running.elapsed.as_millis()).unwrap_or(u64::MAX),
        }
    }
}

/// One addressable user entry from the harness pending queue.
#[derive(Debug, Serialize, ToSchema)]
pub struct PendingQueueEntry {
    /// Stable identity. Never empty, and unique within one response: the snapshot decoder refuses an empty or duplicated id, demoting the slot to `LegacyUser`.
    pub entry_id: String,
    /// The complete text. Never truncated — an entry that would not fit the page budget is left out entirely.
    pub text: String,
    /// CAS token for the edit/delete endpoints. Bumped whenever the text is rewritten, folding included.
    pub rev: u32,
    /// Wall-clock ms at which the entry entered the queue.
    pub queued_at_ms: i64,
    /// The images this queued message carries. Each is already bound; the client addresses an attachment by id, never by host path.
    pub attachments: Vec<PlannerAttachment>,
}

/// Hard cap on entries in one `pending` page.
const PENDING_PAGE_MAX: usize = 64;

/// Soft cap on the UTF-8 size of one `pending` page; entries are packed whole until the next one would cross it. A judgement about acceptable response size, not a measurement.
const PENDING_PAGE_BYTES: usize = 1_536 * 1_024;

/// Split the queue into one page of addressable entries plus a count of the user-authored entries that did not make it.
/// The budget ALWAYS admits at least one entry: a head entry over budget would make the whole queue unpageable, so the user could not delete the thing blocking it.
fn page_pending_entries(card_id: &CardId, entries: &[QueueEntry]) -> (Vec<PendingQueueEntry>, u32) {
    let mut page = Vec::new();
    let mut used_bytes = 0usize;
    let mut overflow = 0u32;
    let mut budget_exhausted = false;
    for entry in entries {
        let Some(view) = entry.user_view() else {
            // Not addressable. A dispatcher observation is not the user's; a pre-id user entry is counted.
            if entry.is_user_authored() {
                overflow = overflow.saturating_add(1);
            }
            continue;
        };
        // The page is a PREFIX of the queue, not a greedy pack: packing around a hole would show a list whose order and adjacency lie.
        budget_exhausted = budget_exhausted
            || page.len() >= PENDING_PAGE_MAX
            || (!page.is_empty()
                && used_bytes.saturating_add(view.text.len()) > PENDING_PAGE_BYTES);
        if budget_exhausted {
            overflow = overflow.saturating_add(1);
            continue;
        }
        used_bytes = used_bytes.saturating_add(view.text.len());
        page.push(PendingQueueEntry {
            entry_id: view.id.as_str().to_string(),
            text: view.text.to_string(),
            rev: view.rev,
            queued_at_ms: view.queued_at_ms,
            attachments: view
                .attachments
                .iter()
                .map(|attachment| attachment.wire(card_id))
                .collect(),
        });
    }
    (page, overflow)
}

/// The context-usage half of [`GetPlannerRunResponse`]. `percent` is computed on the server so there is one place to get it wrong, and `total_tokens` is NOT shipped: it is a cumulative sum across the thread, and a meter drawn from it is the most likely UI bug.
#[derive(Debug, Serialize, ToSchema)]
pub struct PlannerRunTokenUsage {
    /// Tokens in the model's context as of the most recent response. Always present, even when `percent` is not.
    pub used_tokens: i64,
    /// The model's context window, or null when codex has never reported one.
    pub context_window: Option<i64>,
    /// Context occupancy as a whole percentage, `0.0..=100.0`. Null when no percentage can honestly be stated (no window, window at or below baseline, or usage above the window — deliberately NOT clamped).
    pub percent: Option<f64>,
    /// Wall-clock ms of the codex frame this reading came from. The reading survives a reboot via the runtime snapshot, so without this a rehydrated reading is indistinguishable from a live one.
    pub at_ms: i64,
}

impl From<&TokenUsage> for PlannerRunTokenUsage {
    fn from(usage: &TokenUsage) -> Self {
        Self {
            used_tokens: usage.used_tokens,
            context_window: usage.context_window,
            percent: usage.percent(),
            at_ms: usage.at_ms,
        }
    }
}

pub(crate) const MAX_PLANNER_INPUT_CHARS: usize = 32_768;

/// The one body check for planner input, shared by the send route and the edit route: an edit is a send by another name.
pub(crate) fn validate_planner_input_text(text: &str) -> Result<usize> {
    validate_planner_input(text, false)
}

/// The same check, told whether the message carries an image: an empty text beside an attachment is a message, not a refusal. The length limit still applies to whatever text there is.
/// The edit route keeps requiring text: `PATCH` cannot change attachments, so it cannot tell 'this message is its picture' from 'this message is now empty'.
pub(crate) fn validate_planner_input(text: &str, has_attachments: bool) -> Result<usize> {
    if text.trim().is_empty() && !has_attachments {
        return Err(CalmError::BadRequest("text must not be empty".into()));
    }
    let char_count = text.chars().count();
    if char_count > MAX_PLANNER_INPUT_CHARS {
        return Err(CalmError::BadRequest(format!(
            "text must be at most {MAX_PLANNER_INPUT_CHARS} characters",
        )));
    }
    Ok(char_count)
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

/// Stop the running planner turn. Guard chain mirrors `/planner/input` but WITHOUT lazy recovery: a harness that needs recovering has no running turn to stop, so a registry miss is the same 409 `planner_harness_dormant`.
/// Idle is a graceful no-op (`stopped: false`). The phase read and the dispatch are not atomic; the user presses Stop again.
/// `IssuingTurn` also reports `stopped: false`: while `turn/start` is in flight the app-server may not know the turn yet, so the interrupt is dispatched best-effort but only `TurnRunning` guarantees a target.
#[utoipa::path(
    post,
    path = "/api/cards/{id}/planner/interrupt",
    tag = "cards",
    params(("id" = String, Path, description = "Planner card id")),
    responses(
        (status = 200, description = "Interrupt dispatched at the running turn (`stopped: true`); `stopped: false` when no turn was running (graceful no-op) or a turn was still being issued (best-effort dispatch only — press Stop again once the turn is running)", body = InterruptPlannerCardResponse),
        (status = 403, description = "Card is not a planner codex card", body = ErrorBody),
        (status = 404, description = "Card not found", body = ErrorBody),
        (status = 409, description = "No live planner harness session for this card — reset to start a session (code `planner_harness_dormant`)", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn interrupt_planner_card(
    State(s): State<RouteState>,
    actor: Actor,
    Path(id): Path<String>,
) -> Result<Json<InterruptPlannerCardResponse>> {
    let card = s
        .repo
        .card_get(&id)
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

    let dormant = || {
        CalmError::PlannerHarnessDormant(format!(
            "no live planner harness session for card {id}; reset to start a session",
        ))
    };
    let runtime = s
        .repo
        .session_projection_active_for_card(&card.id.to_string())
        .await?
        .ok_or_else(dormant)?;
    let harness = s.harness.get(&runtime.id).ok_or_else(dormant)?;

    let phase = harness.snapshot().await.phase;
    // Dispatch for IssuingTurn too (best-effort), but only TurnRunning reports `stopped: true`.
    let dispatch = matches!(
        phase,
        HarnessPhaseTag::TurnRunning | HarnessPhaseTag::IssuingTurn
    );
    let stopped = matches!(phase, HarnessPhaseTag::TurnRunning);
    if dispatch {
        let payload = serde_json::to_value(PlannerHarnessInterruptOperationPayload {
            worker_session_id: runtime.id.clone(),
            reason: "user_stop".into(),
        })?;
        run_planner_card_operation(&s, "planner-harness-interrupt", payload).await?;
    }

    tracing::info!(
        actor = %actor.as_str(),
        card_id = %card.id,
        runtime_id = %runtime.id,
        ?phase,
        stopped,
        "planner harness user stop requested"
    );

    Ok(Json(InterruptPlannerCardResponse {
        card_id: card.id,
        worker_session_id: runtime.id.clone(),
        stopped,
    }))
}

/// Read the current planner-harness phase for a card. Unlike the write routes, a dormant harness is a normal `200 {worker_session_id: null, phase: null}`, not a 409.
#[utoipa::path(
    get,
    path = "/api/cards/{id}/planner/run",
    tag = "cards",
    params(("id" = String, Path, description = "Planner card id")),
    responses(
        (status = 200, description = "Current run snapshot; `worker_session_id`/`phase` are null when no live harness session exists (dormant is not an error for a read)", body = GetPlannerRunResponse),
        (status = 403, description = "Card is not a planner codex card", body = ErrorBody),
        (status = 404, description = "Card not found", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn get_planner_run(
    State(s): State<RouteState>,
    Path(id): Path<String>,
) -> Result<Json<GetPlannerRunResponse>> {
    let card = s
        .repo
        .card_get(&id)
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

    // Unreadable model keys are reported as 'no selection' by this READ rather than as a 500; the turn-issuing path refuses on the same payload, so the conversation still stops but this surface can show why.
    let selection =
        crate::planner_model::CardModelSelection::from_payload(&card.payload).unwrap_or_default();
    // The same predicate the upload endpoint enforces, so the answer cannot drift from the refusal.
    let attachments_supported = match s.repo.track_get(card.track_id.as_str()).await? {
        Some(track) => {
            crate::planner_attachments::attachment_root(&track.workspace, &s.workspace_root).is_ok()
        }
        // No track means no workspace to write into; this field is not the place to raise it, and 'supported' would be the wrong guess.
        None => false,
    };
    let mut dormant = GetPlannerRunResponse {
        card_id: card.id.clone(),
        worker_session_id: None,
        phase: None,
        model: selection.model.clone(),
        reasoning_effort: selection.reasoning_effort.clone(),
        // A dormant conversation has no harness to be blocked and nothing waiting.
        blocked_reason: None,
        token_usage: None,
        pending: Vec::new(),
        pending_overflow: 0,
        attachments_supported,
        running_turn: None,
    };
    let Some(runtime) = s
        .repo
        .session_projection_active_for_card(&card.id.to_string())
        .await?
    else {
        if let Some(runtime) = s
            .repo
            .session_projection_projectable_for_card(&card.id.to_string())
            .await?
        {
            if let Some(snapshot) = super::planner_recovery::unconfirmed_stop_snapshot(&runtime) {
                dormant.worker_session_id = Some(runtime.id.clone());
                dormant.phase = Some(snapshot.phase);
                dormant.blocked_reason =
                    Some(calm_types::harness::HARNESS_INTERRUPT_TIMEOUT_MESSAGE.into());
                dormant.token_usage = snapshot
                    .token_usage
                    .as_ref()
                    .map(PlannerRunTokenUsage::from);
                (dormant.pending, dormant.pending_overflow) =
                    page_pending_entries(&card.id, &snapshot.pending_entries());
            } else if let Some(snapshot) =
                super::planner_recovery::recoverable_snapshot(&s, &runtime).await?
            {
                dormant.blocked_reason = Some(super::planner_recovery::RECOVERY_NOTICE.into());
                (dormant.pending, dormant.pending_overflow) =
                    page_pending_entries(&card.id, &snapshot.pending_entries());
            }
        }
        return Ok(Json(dormant));
    };
    let Some(harness) = s.harness.get(&runtime.id) else {
        return Ok(Json(dormant));
    };
    // Phase and the running turn come from one state read, so a running turn is never paired with another phase.
    let (snapshot, running_turn) = harness.snapshot_with_running_turn().await;
    let (pending, pending_overflow) = page_pending_entries(&card.id, &snapshot.pending_entries());
    Ok(Json(GetPlannerRunResponse {
        attachments_supported,
        card_id: card.id,
        worker_session_id: Some(runtime.id.clone()),
        phase: Some(snapshot.phase),
        model: selection.model,
        reasoning_effort: selection.reasoning_effort,
        blocked_reason: harness.issuance_block().await,
        token_usage: snapshot
            .token_usage
            .as_ref()
            .map(PlannerRunTokenUsage::from),
        pending,
        pending_overflow,
        running_turn: running_turn.map(PlannerRunningTurn::from),
    }))
}

#[utoipa::path(
    post,
    path = "/api/cards/{id}/planner/reset",
    tag = "cards",
    params(("id" = String, Path, description = "Planner card id")),
    responses(
        (status = 200, description = "Planner session reset", body = ResetPlannerCardResponse),
        (status = 403, description = "Card is not a planner codex card", body = ErrorBody),
        (status = 404, description = "Card not found", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn reset_planner_card(
    State(s): State<RouteState>,
    actor: Actor,
    Path(id): Path<String>,
) -> Result<Json<ResetPlannerCardResponse>> {
    let card = s
        .repo
        .card_get(&id)
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
    // Recovery declines malformed persisted Planner runtimes so a boot pass can continue; this reset boundary keeps the HTTP 403 contract rather than a generic operation failure.
    if role == CardRole::Planner {
        let track = s
            .repo
            .track_get(card.track_id.as_str())
            .await?
            .ok_or_else(|| CalmError::NotFound(format!("track {}", card.track_id)))?;
        if track.purpose.as_deref() == Some(crate::AREA_CHAT_PURPOSE) {
            return Err(CalmError::Forbidden(format!(
                "planner harness is disabled for area chat track {}",
                track.id
            )));
        }
    }
    let response = reset_planner_card_shared(s, actor, card).await?;
    Ok(Json(response))
}

async fn reset_planner_card_shared(
    s: RouteState,
    actor: Actor,
    card: Card,
) -> Result<ResetPlannerCardResponse> {
    // Reset takes the SAME per-card lock as `/planner/input` lazy recovery, or a reset racing a registry-miss Send could resurrect the reset-away session. Deadlock-free: neither adapter re-enters `planner_recovery_locks`.
    let _recovery_guard = lock_card(&s.planner_recovery_locks, card.id.as_str()).await;
    let active_runtime = s
        .repo
        .session_projection_active_for_card(&card.id.to_string())
        .await?;
    reset_planner_harness_card(s, actor, card, active_runtime).await
}

async fn reset_planner_harness_card(
    s: RouteState,
    actor: Actor,
    card: Card,
    runtime: Option<WorkerSessionProjection>,
) -> Result<ResetPlannerCardResponse> {
    let track = s
        .repo
        .track_get(card.track_id.as_str())
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("track {}", card.track_id)))?;

    // A marked conversation card restarts under its OWN profile: restarting an assistant under `Planner` would re-mint its thread with the planner prompt while the card row still says `assistant`.
    // No profile inherits the track title as a goal on this user-driven reset path.
    let role = s.write.verify_role(&card.id);
    let profile = if crate::plain_chat::card_is_plain_chat(&card, role, true) {
        HarnessProfile::PlainChat
    } else if crate::plain_chat::card_is_track_assistant(&card, role, true) {
        HarnessProfile::Assistant
    } else {
        HarnessProfile::Planner
    };
    let start_request = PlannerHarnessStartOperationPayload {
        actor: actor.to_actor_id(),
        track_id: track.id.to_string(),
        planner_card_id: card.id.clone(),
        report_card_id: None,
        sort: None,
        cwd: track.workspace.agent_cwd().to_string(),
        goal: None,
        reset_harness_items: true,
        force_new_thread: true,
        profile,
        create_card: None,
        first_message: None,
        create_request_sha256: None,
        // Not a conversation create; nothing to brief. `None` is skipped by serde.
        opening_briefing: None,
    };
    let start_payload = serde_json::to_value(start_request)?;
    run_planner_card_operation(&s, "planner-harness-start", start_payload).await?;

    if let Some(runtime) = runtime {
        let shutdown_payload = serde_json::to_value(PlannerHarnessShutdownOperationPayload {
            worker_session_id: runtime.id.clone(),
        })?;
        run_planner_card_operation(&s, "planner-harness-shutdown", shutdown_payload).await?;
    }

    let active = s
        .repo
        .session_projection_active_for_card(&card.id.to_string())
        .await?
        .ok_or_else(|| CalmError::Internal(format!("runtime for card {} missing", card.id)))?;
    let new_thread_id = active.thread_id.clone().ok_or_else(|| {
        CalmError::Internal(format!(
            "planner harness reset succeeded without a thread_id for card {}",
            card.id
        ))
    })?;
    let track = s
        .repo
        .track_get(card.track_id.as_str())
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("track {}", card.track_id)))?;

    Ok(ResetPlannerCardResponse {
        card_id: card.id,
        terminal_id: String::new(),
        new_thread_id,
        track: Some(track),
    })
}

/// Submit one planner-card operation and wait for it, mapping its outcome onto a `CalmError`. Shared with `routes::today_summary`'s dormant recovery so failure classes map identically.
pub(crate) async fn run_planner_card_operation(
    s: &RouteState,
    kind: &str,
    payload: Value,
) -> Result<()> {
    let payload_hash = stable_payload_hash(&payload)?;
    let op_id = s
        .operation_runtime
        .submit(
            kind,
            OperationKey {
                operation_key: new_id(),
                idempotency_key: None,
                payload_hash,
            },
            payload,
        )
        .await?;
    let result = s.operation_runtime.wait(&op_id).await?;
    match result.outcome {
        OperationOutcome::Succeeded { .. } | OperationOutcome::SucceededViaCollision { .. } => {
            Ok(())
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

#[utoipa::path(
    delete,
    path = "/api/cards/{id}",
    tag = "cards",
    params(("id" = String, Path, description = "Card id")),
    responses(
        (status = 204, description = "Card deleted"),
        (status = 404, description = "Card not found", body = ErrorBody),
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

#[cfg(test)]
mod pending_page_tests {
    use super::{PENDING_PAGE_BYTES, PENDING_PAGE_MAX, page_pending_entries};
    use crate::harness::{HARNESS_MODE, HarnessSnapshot, Observation, QueueEntry};
    use crate::ids::CardId;
    use serde_json::json;

    fn test_card_id() -> CardId {
        CardId::from("card-paging")
    }

    /// A legacy entry built the ONLY way production can produce one: a row whose `pending_entry_meta` slot is absent.
    fn legacy(text: &str) -> QueueEntry {
        let row = json!({
            "schema_version": 1,
            "mode": HARNESS_MODE,
            "phase": "idle",
            "pending_queue": [{"type": "user_message", "text": text}],
        });
        HarnessSnapshot::from_value_strict(row)
            .pending_entries()
            .remove(0)
    }

    fn user(text: &str) -> QueueEntry {
        QueueEntry::user_message(text.to_string(), None, Vec::new())
    }

    fn system() -> QueueEntry {
        QueueEntry::system(
            Observation::TrackGoal {
                text: "goal".into(),
            },
            None,
        )
        .expect("a track goal is a system entry")
    }

    #[test]
    fn only_addressable_user_entries_reach_the_page() {
        let entries = vec![system(), user("mine"), legacy("older")];
        let (page, overflow) = page_pending_entries(&test_card_id(), &entries);
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].text, "mine");
        assert_eq!(
            overflow, 1,
            "the legacy entry is counted, the system entry is not"
        );
    }

    #[test]
    fn the_page_is_capped_by_entry_count() {
        let entries = (0..PENDING_PAGE_MAX + 5)
            .map(|i| user(&format!("m{i}")))
            .collect::<Vec<_>>();
        let (page, overflow) = page_pending_entries(&test_card_id(), &entries);
        assert_eq!(page.len(), PENDING_PAGE_MAX);
        assert_eq!(overflow, 5);
        assert_eq!(page[0].text, "m0", "the page starts at the queue head");
    }

    #[test]
    fn the_page_is_capped_by_byte_budget_and_entries_stay_whole() {
        let big = "x".repeat(PENDING_PAGE_BYTES / 2 + 1);
        let entries = vec![user(&big), user(&big), user("tiny")];
        let (page, overflow) = page_pending_entries(&test_card_id(), &entries);
        assert_eq!(page.len(), 1, "the second entry would cross the budget");
        assert_eq!(
            page[0].text.len(),
            big.len(),
            "an entry that IS returned is returned whole; nothing is truncated"
        );
        assert_eq!(
            overflow, 2,
            "`tiny` would have fitted, but the page is a prefix: packing around \
             the entry that did not fit would put a hole in the middle of the \
             queue the user is looking at"
        );
    }

    /// The budget always admits the head entry, however large; unreachable today, but an over-budget head entry would make the whole queue unaddressable.
    #[test]
    fn an_over_budget_head_entry_is_still_returned_whole() {
        let huge = "y".repeat(PENDING_PAGE_BYTES + 4_096);
        let entries = vec![user(&huge), user("behind it")];
        let (page, overflow) = page_pending_entries(&test_card_id(), &entries);
        assert_eq!(page.len(), 1, "the budget never returns an empty page");
        assert_eq!(page[0].text.len(), huge.len());
        assert_eq!(overflow, 1);
    }

    #[test]
    fn an_empty_queue_pages_to_nothing() {
        let (page, overflow) = page_pending_entries(&test_card_id(), &[]);
        assert!(page.is_empty());
        assert_eq!(overflow, 0);
    }
}
