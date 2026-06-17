//! `/api/cards`, `/api/waves/:id/cards` — Card CRUD. **Owned by Track B.**
//!
//! M3-mcp-apps M2: the create route accepts an optional `via_tool_call`
//! payload variant. When present, the kernel invokes the named tool on the
//! running plugin via standard MCP `tools/call`, extracts
//! `_meta.ui.resourceUri` from the result, and persists a Card with that URI
//! as `Card.kind` and `structuredContent` as the payload. The two paths
//! (direct create vs `via_tool_call`) are mutually exclusive at runtime; when
//! a client sends both, `via_tool_call` wins (the tool-call result overrides
//! the direct-create fields).

use crate::actor::Actor;
use crate::db::sqlite::{
    card_create_with_id_tx, card_delete_tx, card_update_tx, terminal_delete_tx,
};
use crate::db::write_with_event_typed;
use crate::db::{RepoRead, RouteRepo};
use crate::error::{CalmError, ErrorBody, Result};
use crate::event::{Event, EventScope};
use crate::harness::{HarnessPhaseTag, Observation, is_harness_snapshot_value};
use crate::ids::{ActorId, CardId, WaveId};
use crate::model::{Card, CardPatch, CardRole, HarnessItem, NewCard, Wave, new_id};
use crate::operation::spec_harness_interrupt_adapter::SpecHarnessInterruptOperationPayload;
use crate::operation::spec_harness_shutdown_adapter::SpecHarnessShutdownOperationPayload;
use crate::operation::spec_harness_start_adapter::SpecHarnessStartOperationPayload;
use crate::operation::{OperationKey, OperationOutcome};
use crate::per_card_lock::{PerCardLockGuard, lock_card};
use crate::plugin_host::callbacks::extract_card_creation_from_tool_call_result;
use crate::routes::terminal_cards::{calm_error_from_operation_failure, stable_payload_hash};
use crate::runtime_lookup::{
    card_is_shared_spec, project_runtime_into_card_payload, project_runtime_into_cards_payload,
};
use crate::runtime_repo::{RunStatus, WorkerSessionProjection};
use crate::state::{AppState, CodexShellState, RouteState, WorkerState};
use crate::terminal_sweeper::reap_terminal_artifacts_with_renderer;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use utoipa::{IntoParams, ToSchema};

/// Resolve the (wave, cove) ancestor pair for a wave id, returning a
/// pre-built [`EventScope::Card`] for the given card. PR2 of #136 needs
/// this at every card-emit site so the event row's `scope_*` columns
/// carry the full ancestor chain. Looking up the wave outside the txn
/// is fine — wave rows are immutable wrt their parent cove.
pub(crate) async fn card_scope(
    repo: &dyn RepoRead,
    card: CardId,
    wave: WaveId,
) -> Result<EventScope> {
    let w = repo
        .wave_get(wave.as_str())
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("wave {wave}")))?;
    Ok(EventScope::Card {
        card,
        wave: w.id,
        cove: w.cove_id,
    })
}

pub(crate) async fn interrupt_shared_card_active_turn(
    repo: &dyn RouteRepo,
    cs: &CodexShellState,
    card: &Card,
) {
    let active_runtime = match repo.runtime_get_active_for_card(&card.id.to_string()).await {
        Ok(runtime) => runtime,
        Err(e) => {
            tracing::warn!(
                target: "runtime_lookup::fallback",
                card_id = %card.id,
                error = %e,
                "runtime shared-card discriminator query failed; falling back to card payload"
            );
            None
        }
    };
    if !card_is_shared_spec(card, active_runtime.as_ref()) {
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
            wave_id = %card.wave_id,
            error = %e,
            "failed to interrupt active shared codex turn during card teardown"
        );
    }
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/waves/{wave_id}/cards",
            get(list_cards_by_wave).post(create_card),
        )
        .route(
            "/api/cards/{id}",
            axum::routing::patch(update_card).delete(delete_card),
        )
        .route("/api/cards/{id}/harness/items", get(get_harness_items))
        .route("/api/cards/{id}/spec/input", post(send_spec_input))
        .route("/api/cards/{id}/spec/interrupt", post(interrupt_spec_card))
        .route("/api/cards/{id}/spec/run", get(get_spec_run))
        .route("/api/cards/{id}/spec/reset", post(reset_spec_card))
}

#[utoipa::path(
    get,
    path = "/api/waves/{wave_id}/cards",
    tag = "cards",
    params(("wave_id" = String, Path, description = "Wave id")),
    responses(
        (status = 200, description = "Cards in wave (sorted)", body = Vec<Card>),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn list_cards_by_wave(
    State(s): State<RouteState>,
    Path(wave_id): Path<String>,
) -> Result<Json<Vec<Card>>> {
    let mut cards = s.repo.cards_by_wave(&wave_id).await?;
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
        ("id" = String, Path, description = "Spec card id"),
        HarnessItemsQuery,
    ),
    responses(
        (status = 200, description = "Persisted spec harness items", body = Vec<HarnessItem>),
        (status = 403, description = "Card is not a spec codex card", body = ErrorBody),
        (status = 404, description = "Card not found", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn get_harness_items(
    State(s): State<RouteState>,
    Path(id): Path<String>,
    Query(q): Query<HarnessItemsQuery>,
) -> Result<Json<Vec<HarnessItem>>> {
    let card = s
        .repo
        .card_get(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("card {id}")))?;
    let role = s
        .write
        .verify_role(&card.id)
        .ok_or_else(|| CalmError::NotFound(format!("card {id}")))?;
    if card.kind != "codex" || role != CardRole::Spec {
        return Err(CalmError::Forbidden(format!(
            "card {id} is not a spec codex card",
        )));
    }

    let after_id = q.after_id.unwrap_or(0).max(0);
    let limit = q.limit.unwrap_or(100).clamp(0, 500);
    let descending = q.direction == HarnessItemsDirection::Desc;
    let items = s
        .repo
        .harness_item_list_by_card(card.id.as_str(), after_id, limit, descending)
        .await?;
    Ok(Json(items))
}

/// Body payload accepted by `POST /api/waves/:wave_id/cards`.
///
/// Two mutually-exclusive paths:
///   * **Direct create** — `kind`, `sort`, `payload`, `title` set (legacy
///     pre-M2 wire). The kernel writes the row verbatim.
///   * **`via_tool_call`** — kernel invokes the plugin's tool, extracts the
///     `ui://` resource URI from `_meta.ui.resourceUri`, persists a Card with
///     `kind = <resource_uri>` and `payload = structuredContent`.
///
/// When both are sent, `via_tool_call` wins. Documented in this module's
/// header. We keep the legacy fields alongside via `#[serde(flatten)]` so
/// existing clients (web-calm AddPanel for terminal/doc cards) keep working
/// unchanged.
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateCardBody {
    /// Legacy direct-create fields. Mirrors `NewCard` shape; `wave_id` is
    /// taken from the path so we omit it here.
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub sort: Option<f64>,
    #[serde(default)]
    #[schema(value_type = Option<Object>)]
    pub payload: Option<Value>,
    /// M2: plugin tool-call descriptor. When present, the kernel calls the
    /// plugin and the `kind` / `payload` fields above are ignored.
    #[serde(default)]
    pub via_tool_call: Option<ViaToolCall>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ViaToolCall {
    pub plugin_id: String,
    pub tool_name: String,
    #[serde(default)]
    #[schema(value_type = Object)]
    pub arguments: Value,
}

#[utoipa::path(
    post,
    path = "/api/waves/{wave_id}/cards",
    tag = "cards",
    params(("wave_id" = String, Path, description = "Wave id this card belongs to")),
    request_body = CreateCardBody,
    responses(
        (status = 201, description = "Card created", body = Card),
        (status = 400, description = "Missing `kind` and no `via_tool_call`", body = ErrorBody),
        (status = 403, description = "Plugin lacks `permissions.cards_create`", body = ErrorBody),
        (status = 404, description = "Plugin not running / not in registry", body = ErrorBody),
        (status = 422, description = "Tool returned no `_meta.ui.resourceUri`", body = ErrorBody),
        (status = 502, description = "Plugin tool call failed", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
#[allow(deprecated)]
pub(crate) async fn create_card(
    State(s): State<AppState>,
    actor: Actor,
    Path(wave_id): Path<String>,
    Json(body): Json<CreateCardBody>,
) -> Result<Response, Response> {
    // M2: tool-call path wins over direct-create. The tool-call branch
    // overrides the actor to `"plugin:<id>"` (the entity actually making
    // the kernel write) regardless of any `X-Calm-Actor` header — plugins
    // cannot spoof their own actor via REST (design §9 bullet 2/3).
    if let Some(via) = body.via_tool_call {
        return create_via_tool_call(&s, wave_id, via).await;
    }

    // Direct-create path (legacy / pre-M2). `kind` is required here — for
    // tool-call the kernel synthesizes it from the resource URI.
    let kind = body.kind.ok_or_else(|| {
        CalmError::BadRequest("create card body needs either `kind` or `via_tool_call`".into())
            .into_response()
    })?;
    let payload = body.payload.unwrap_or(Value::Null);
    // D4: reject malformed payloads for kernel-owned kinds. Plugin-defined
    // (`ui://*`) kinds remain opaque per the architectural invariant.
    s.card_kind_registry()
        .validate_payload(&kind, &payload)
        .map_err(|e| CalmError::from(e).into_response())?;
    // Pre-mint the card id so we can stamp `EventScope::Card { card, .. }`
    // deterministically before the txn opens. The kernel's `new_id()` is
    // a UUID — collision risk is negligible. Using
    // `card_create_with_id_tx` (the carved-out variant the codex/terminal
    // atomic endpoints already use) keeps the actual SQL identical.
    let card_id = CardId::from(new_id());
    let wave_id: WaveId = wave_id.into();
    let scope = card_scope(s.repo.as_ref(), card_id.clone(), wave_id.clone())
        .await
        .map_err(|e| e.into_response())?;
    let new = NewCard {
        wave_id,
        kind,
        sort: body.sort,
        payload,
    };
    let card_id_for_tx = card_id.0.clone();
    let write_for_tx = s.write().clone();
    let (mut card, _id) = write_with_event_typed(
        s.repo.as_ref(),
        actor.to_actor_id(),
        scope,
        None,
        &s.events,
        s.write(),
        move |tx| {
            Box::pin(async move {
                // Issue #585 — user-driven creates mint Worker cards and are
                // user-deletable. The `false` path is reserved for
                // kernel-owned cards minted by internal code (spec card
                // here in PR A; report card in PR B).
                let card = card_create_with_id_tx(
                    tx,
                    card_id_for_tx,
                    new,
                    CardRole::Worker,
                    true,
                    write_for_tx.role_cache(),
                )
                .await?;
                Ok((card.clone(), Event::CardAdded(card)))
            })
        },
    )
    .await
    .map_err(|e| e.into_response())?;
    project_runtime_into_card_payload(s.repo.as_ref(), &mut card)
        .await
        .map_err(CalmError::from)
        .map_err(|e| e.into_response())?;
    Ok((StatusCode::CREATED, Json(card)).into_response())
}

/// M2 handler: kernel invokes `tools/call` on the plugin, then writes a Card
/// row keyed off `_meta.ui.resourceUri`. Error mapping per the migration
/// doc's M2 spec:
///   * plugin not running → 404
///   * `permissions.cards_create` not granted → 403
///   * tool returned `isError: true` → 502 with content joined as text
///   * tool succeeded but omitted `_meta.ui.resourceUri` → 422
///     `{"error":"...","code":"not_a_card_tool"}`
#[allow(deprecated)]
async fn create_via_tool_call(
    s: &AppState,
    wave_id: String,
    via: ViaToolCall,
) -> Result<Response, Response> {
    // 1. Plugin must be running. `mcp_client` returns None when the plugin is
    //    Disabled / Crashed / not yet spawned.
    let mcp = match s.plugin.mcp_client(&via.plugin_id).await {
        Some(c) => c,
        None => {
            return Err(
                CalmError::NotFound(format!("plugin `{}` is not running", via.plugin_id))
                    .into_response(),
            );
        }
    };

    // 2. Manifest-based permission gate. Mirrors the autonomous
    //    `neige.card.create` gate in `callbacks.rs::card_create`: the
    //    plugin must have `permissions.cards_create == true`. The
    //    migration doc speaks of `permissions.cards.create` with `wave`
    //    scope; today's manifest shape only has a boolean — that's the
    //    canonical gate per `perms.rs`.
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

    // 3. Invoke the tool. Transport-level / RpcError failures propagate as
    //    502 with the error message inline so the client gets a clear signal.
    let result = mcp
        .tools_call(&via.tool_name, via.arguments)
        .await
        .map_err(|e| tool_call_bad_gateway(&via.plugin_id, &via.tool_name, &e.to_string()))?;

    // 4. Tool-reported failure (`isError: true`) → 502, content joined.
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

    // 5. Pull `_meta.ui.resourceUri`. Absent → 422; this is the "you tried
    //    to use a non-card tool as a card-create handle" path.
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

    // 6. Persist. `kind` is the bare `ui://...` URI (M4 will fully dispatch
    //    on this); `payload` defaults to JSON null when the tool omits
    //    `structuredContent`.
    let payload = creation.structured_content.unwrap_or(Value::Null);
    // D4: validate even on the tool-call path. In practice `ui://*` kinds
    // are opaque so this is a no-op for plugin-defined views — but if a
    // tool ever names a kernel kind (e.g. `"terminal"`) via resourceUri,
    // we reject a malformed payload here rather than after the DB write.
    s.card_kind_registry()
        .validate_payload(&creation.resource_uri, &payload)
        .map_err(|e| CalmError::from(e).into_response())?;
    let new = NewCard {
        wave_id: wave_id.into(),
        kind: creation.resource_uri,
        sort: None,
        payload,
    };
    // M2 tool-call writes: actor stays `Plugin(<id>)` (the entity making
    // the kernel write), `correlation` records the user-driven invocation
    // so audit queries can reconstruct the causal chain (design §9 bullet 3).
    // PR2 of #136 pre-mints the card id so `EventScope::Card { card, .. }`
    // is determinable before the txn opens.
    let actor = ActorId::Plugin(via.plugin_id.clone());
    let correlation = format!("user_tool_call:{}", via.tool_name);
    let card_id = CardId::from(new_id());
    let wave_id_for_scope: WaveId = new.wave_id.clone();
    let scope = card_scope(s.repo.as_ref(), card_id.clone(), wave_id_for_scope)
        .await
        .map_err(|e| e.into_response())?;
    let card_id_for_tx = card_id.0.clone();
    let write_for_tx = s.write().clone();
    let (mut card, _id) = write_with_event_typed(
        s.repo.as_ref(),
        actor,
        scope,
        Some(&correlation),
        &s.events,
        s.write(),
        move |tx| {
            Box::pin(async move {
                // Issue #585 — user-driven creates mint Worker cards and are
                // user-deletable. The `false` path is reserved for
                // kernel-owned cards minted by internal code (spec card
                // here in PR A; report card in PR B).
                let card = card_create_with_id_tx(
                    tx,
                    card_id_for_tx,
                    new,
                    CardRole::Worker,
                    true,
                    write_for_tx.role_cache(),
                )
                .await?;
                Ok((card.clone(), Event::CardAdded(card)))
            })
        },
    )
    .await
    .map_err(|e| e.into_response())?;
    project_runtime_into_card_payload(s.repo.as_ref(), &mut card)
        .await
        .map_err(CalmError::from)
        .map_err(|e| e.into_response())?;
    Ok((StatusCode::CREATED, Json(card)).into_response())
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
        (status = 404, description = "Card not found", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn update_card(
    State(s): State<AppState>,
    actor: Actor,
    Path(id): Path<String>,
    Json(p): Json<CardPatch>,
) -> Result<Json<Card>> {
    // Issue #229 PR A — `deletable` is a kernel-owned bit, not patchable
    // from the API. Reject the request loudly with 400 so a misconfigured
    // client (or a curious script) doesn't think the field silently
    // updated. `card_update_tx` also ignores the field as a belt-and-
    // suspenders defense; this handler-level rejection is the primary
    // contract.
    if p.deletable.is_some() {
        return Err(CalmError::BadRequest(
            "`deletable` is a kernel-managed field and cannot be patched via API".into(),
        ));
    }
    // We need the existing card's wave_id for the EventScope chain
    // regardless of whether validation needs the kind. Fetch once.
    let existing = s
        .repo
        .card_get(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("card {id}")))?;
    // D4: if the patch carries a payload, validate it against the kind that
    // will land in the DB. The kind is either the patch's new kind (when the
    // patch retargets) or the existing card's kind.
    if let Some(payload) = p.payload.as_ref() {
        let kind = p.kind.as_deref().unwrap_or(existing.kind.as_str());
        s.card_kind_registry().validate_payload(kind, payload)?;
    }
    let scope = card_scope(s.repo.as_ref(), existing.id.clone(), existing.wave_id).await?;
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
pub struct ResetSpecCardResponse {
    #[schema(value_type = String)]
    pub card_id: CardId,
    pub terminal_id: String,
    pub new_thread_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wave: Option<Wave>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SendSpecInputRequest {
    pub text: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SendSpecInputResponse {
    #[schema(value_type = String)]
    pub card_id: CardId,
    pub runtime_id: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct InterruptSpecCardResponse {
    #[schema(value_type = String)]
    pub card_id: CardId,
    pub runtime_id: String,
    /// True when a turn was actually running and an interrupt was
    /// dispatched at it; false when the harness was idle (graceful no-op)
    /// or a `turn/start` was still in flight (interrupt dispatched
    /// best-effort, but not guaranteed to land — press Stop again once the
    /// turn is running). "stopped: true" means the interrupt was *issued* —
    /// completion is asynchronous (`turn/aborted` lands via the harness
    /// FSM, with an interrupt-timeout watchdog as backstop).
    pub stopped: bool,
}

/// Issue #668 fix — current spec-harness run snapshot for a card.
///
/// `harness.phase.changed` is the only live phase signal, so a page opened
/// mid-turn would otherwise sit on `phase: null` until the next transition.
/// This read endpoint lets the client seed its initial phase. Dormancy (no
/// active runtime row, or no registered harness) is NOT an error here —
/// it's the `{runtime_id: null, phase: null}` answer.
#[derive(Debug, Serialize, ToSchema)]
pub struct GetSpecRunResponse {
    #[schema(value_type = String)]
    pub card_id: CardId,
    /// Active runtime id, or null when the harness is dormant.
    pub runtime_id: Option<String>,
    /// Current harness phase, or null when the harness is dormant.
    pub phase: Option<HarnessPhaseTag>,
}

const MAX_SPEC_INPUT_CHARS: usize = 32_768;

fn spec_input_audit_actor(actor: &Actor, card_id: &CardId) -> ActorId {
    match actor.to_actor_id() {
        ActorId::AiCodex(c) if c.as_str().is_empty() => ActorId::AiCodex(card_id.clone()),
        // Middleware currently only admits `ai:codex`, but keep these
        // branches ready if REST actor validation later gains more AI kinds.
        ActorId::AiClaude(c) if c.as_str().is_empty() => ActorId::AiClaude(card_id.clone()),
        ActorId::AiSpec(c) if c.as_str().is_empty() => ActorId::AiSpec(card_id.clone()),
        other => other,
    }
}

#[utoipa::path(
    post,
    path = "/api/cards/{id}/spec/input",
    tag = "cards",
    params(("id" = String, Path, description = "Spec card id")),
    request_body = SendSpecInputRequest,
    responses(
        (status = 200, description = "User text queued for next harness turn", body = SendSpecInputResponse),
        (status = 400, description = "Empty text", body = ErrorBody),
        (status = 403, description = "Card is not a spec codex card", body = ErrorBody),
        (status = 404, description = "Card or wave not found", body = ErrorBody),
        (status = 409, description = "Runtime is shutting down (code `conflict`), or the spec harness session is dormant and not recoverable — reset to start a session (code `spec_harness_dormant`)", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
        (status = 503, description = "Observation queue saturated, shared codex app-server not running, or a spec-harness start is still in flight — retry shortly", body = ErrorBody),
    ),
)]
#[allow(deprecated)]
pub(crate) async fn send_spec_input(
    State(s): State<RouteState>,
    State(w): State<WorkerState>,
    State(cs): State<CodexShellState>,
    actor: Actor,
    Path(id): Path<String>,
    Json(body): Json<SendSpecInputRequest>,
) -> Result<Json<SendSpecInputResponse>> {
    if body.text.trim().is_empty() {
        return Err(CalmError::BadRequest("text must not be empty".into()));
    }
    let char_count = body.text.chars().count();
    if char_count > MAX_SPEC_INPUT_CHARS {
        return Err(CalmError::BadRequest(format!(
            "text must be at most {MAX_SPEC_INPUT_CHARS} characters",
        )));
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
    if card.kind != "codex" || role != CardRole::Spec {
        return Err(CalmError::Forbidden(format!(
            "card {id} is not a spec codex card",
        )));
    }

    // `_recovery_guard` (Some only on the lazy-recovery path) holds the
    // per-card recovery lock until end of handler scope, so a concurrent
    // `/spec/reset` can't supersede the just-recovered runtime between
    // recovery and the observe/audit below.
    let (runtime, harness, _recovery_guard) =
        ensure_live_spec_harness(&s, &w, &cs, &card.id).await?;
    let wave = s
        .repo
        .wave_get(card.wave_id.as_str())
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("wave {} for card {id}", card.wave_id)))?;
    let scope = EventScope::Card {
        card: card.id.clone(),
        wave: wave.id.clone(),
        cove: wave.cove_id.clone(),
    };
    let audit_actor = spec_input_audit_actor(&actor, &card.id);

    let text = body.text;
    harness.observe(Observation::UserMessage { text })?;

    tracing::info!(
        actor = %actor.as_str(),
        card_id = %card.id,
        runtime_id = %runtime.id,
        char_count,
        "spec harness user message enqueued"
    );

    s.repo
        .log_pure_event(
            audit_actor,
            scope,
            None,
            &s.events,
            s.write.role_cache(),
            s.write.cove_cache(),
            Event::HarnessUserMessageEnqueued {
                runtime_id: runtime.id.clone(),
                card_id: card.id.clone(),
                wave_id: card.wave_id.clone(),
                char_count: char_count as u32,
            },
        )
        .await?;

    Ok(Json(SendSpecInputResponse {
        card_id: card.id,
        runtime_id: runtime.id.clone(),
    }))
}

/// Issue #668 — stop the running spec turn.
///
/// Guard chain mirrors `/spec/input` (card → role → kind), but deliberately
/// WITHOUT the lazy-recovery path and its per-card lock: a harness that
/// needs recovering has, by construction, no running turn to stop, so a
/// registry miss (or no active runtime row) is the same typed 409
/// `spec_harness_dormant` the input route uses — the client steers the user
/// to Reset.
///
/// Idle is a graceful no-op, not an error: the harness's own
/// `issue_interrupt` ignores interrupts when no turn is active, so the route
/// reports `stopped: false` (decided from the harness phase just before
/// dispatch) and skips the operation entirely. The phase read and the
/// dispatch are not atomic — a turn could start in between — but the failure
/// mode is benign (the user presses Stop again). `IssuingInterrupt` also
/// reports `stopped: false`: an interrupt is already in flight and
/// re-dispatching would be ignored by the FSM anyway.
///
/// `IssuingTurn` is a best-effort window, so it reports `stopped: false`
/// too: while the `turn/start` RPC is in flight the shared app-server may
/// not have populated `active_turn_id_for_thread` yet, so the harness's
/// `issue_interrupt` can resolve no target and no-op — the turn would then
/// keep running despite a `stopped: true` answer. The route still dispatches
/// the interrupt (it lands when the app-server already knows the turn), but
/// only `TurnRunning` — where an interrupt target is guaranteed — earns
/// `stopped: true`. The user can press Stop again once the turn is running.
/// Non-goal: teaching the run loop to remember a pending interrupt across
/// the Issuing window and fire it on `turn/start` completion.
#[utoipa::path(
    post,
    path = "/api/cards/{id}/spec/interrupt",
    tag = "cards",
    params(("id" = String, Path, description = "Spec card id")),
    responses(
        (status = 200, description = "Interrupt dispatched at the running turn (`stopped: true`); `stopped: false` when no turn was running (graceful no-op) or a turn was still being issued (best-effort dispatch only — press Stop again once the turn is running)", body = InterruptSpecCardResponse),
        (status = 403, description = "Card is not a spec codex card", body = ErrorBody),
        (status = 404, description = "Card not found", body = ErrorBody),
        (status = 409, description = "No live spec harness session for this card — reset to start a session (code `spec_harness_dormant`)", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn interrupt_spec_card(
    State(s): State<RouteState>,
    actor: Actor,
    Path(id): Path<String>,
) -> Result<Json<InterruptSpecCardResponse>> {
    let card = s
        .repo
        .card_get(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("card {id}")))?;
    let role = s
        .write
        .verify_role(&card.id)
        .ok_or_else(|| CalmError::NotFound(format!("card {id}")))?;
    if card.kind != "codex" || role != CardRole::Spec {
        return Err(CalmError::Forbidden(format!(
            "card {id} is not a spec codex card",
        )));
    }

    let dormant = || {
        CalmError::SpecHarnessDormant(format!(
            "no live spec harness session for card {id}; reset to start a session",
        ))
    };
    let runtime = s
        .repo
        .runtime_get_active_for_card(&card.id.to_string())
        .await?
        .ok_or_else(dormant)?;
    let harness = s.harness.get(&runtime.id).ok_or_else(dormant)?;

    let phase = harness.snapshot().await.phase;
    // Dispatch for IssuingTurn too (best-effort), but only TurnRunning —
    // where an interrupt target is guaranteed — reports `stopped: true`.
    let dispatch = matches!(
        phase,
        HarnessPhaseTag::TurnRunning | HarnessPhaseTag::IssuingTurn
    );
    let stopped = matches!(phase, HarnessPhaseTag::TurnRunning);
    if dispatch {
        let payload = serde_json::to_value(SpecHarnessInterruptOperationPayload {
            runtime_id: runtime.id.clone(),
            reason: "user_stop".into(),
        })?;
        run_spec_card_operation(&s, "spec-harness-interrupt", payload).await?;
    }

    tracing::info!(
        actor = %actor.as_str(),
        card_id = %card.id,
        runtime_id = %runtime.id,
        ?phase,
        stopped,
        "spec harness user stop requested"
    );

    Ok(Json(InterruptSpecCardResponse {
        card_id: card.id,
        runtime_id: runtime.id.clone(),
        stopped,
    }))
}

/// Issue #668 fix — read the current spec-harness phase for a card.
///
/// Guard chain mirrors `/spec/interrupt` (card → role → kind), but unlike
/// the write routes a dormant harness is a normal answer for a read: no
/// active runtime row, or an active row with no registered harness, is
/// `200 {runtime_id: null, phase: null}` rather than a 409.
#[utoipa::path(
    get,
    path = "/api/cards/{id}/spec/run",
    tag = "cards",
    params(("id" = String, Path, description = "Spec card id")),
    responses(
        (status = 200, description = "Current run snapshot; `runtime_id`/`phase` are null when no live harness session exists (dormant is not an error for a read)", body = GetSpecRunResponse),
        (status = 403, description = "Card is not a spec codex card", body = ErrorBody),
        (status = 404, description = "Card not found", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn get_spec_run(
    State(s): State<RouteState>,
    Path(id): Path<String>,
) -> Result<Json<GetSpecRunResponse>> {
    let card = s
        .repo
        .card_get(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("card {id}")))?;
    let role = s
        .write
        .verify_role(&card.id)
        .ok_or_else(|| CalmError::NotFound(format!("card {id}")))?;
    if card.kind != "codex" || role != CardRole::Spec {
        return Err(CalmError::Forbidden(format!(
            "card {id} is not a spec codex card",
        )));
    }

    let dormant = GetSpecRunResponse {
        card_id: card.id.clone(),
        runtime_id: None,
        phase: None,
    };
    let Some(runtime) = s
        .repo
        .runtime_get_active_for_card(&card.id.to_string())
        .await?
    else {
        return Ok(Json(dormant));
    };
    let Some(harness) = s.harness.get(&runtime.id) else {
        return Ok(Json(dormant));
    };
    let phase = harness.snapshot().await.phase;
    Ok(Json(GetSpecRunResponse {
        card_id: card.id,
        runtime_id: Some(runtime.id.clone()),
        phase: Some(phase),
    }))
}

/// Issue #649 i2 — resolve a live [`SpecHarness`] handle for a spec card.
///
/// Fast path: active runtime row + registry hit (untouched behavior).
///
/// Registry miss with an active runtime row (e.g. server restart on a
/// `done`-lifecycle wave, where boot recovery deliberately skips the wave)
/// → lazily re-spawn the harness in place via
/// [`crate::harness::spawn_recovered_harness`] — the exact function boot
/// recovery uses (snapshot load, catch-up event replay, run, registry
/// insert). Spawning does no Codex RPC, so recovery is cheap.
///
/// No active runtime row, or an active row that is unrecoverable
/// (no thread anywhere — neither `runtime.thread_id` nor the snapshot's
/// `last_thread_id` — from a half-failed start, or a corrupt snapshot)
/// → typed 409 [`CalmError::SpecHarnessDormant`] so the client can steer
/// the user to `/spec/reset` instead of retrying.
///
/// Hardenings (design review):
/// 1. per-card async lock + re-fetch/re-probe under the lock, so racing
///    Sends can't double-spawn (the second spawn shuts the first down);
/// 2. snapshot pre-validated with [`is_harness_snapshot_value`] — the
///    strict deserializer panics on unknown shapes;
/// 3. a thread must exist (row `thread_id`, or the snapshot's
///    `last_thread_id` — the same fallback boot recovery applies), else a
///    recovered harness would queue messages forever;
/// 4. `/spec/reset` takes the SAME per-card lock (see
///    [`reset_spec_card_shared`]), and the recovery path RETURNS its guard
///    to the caller (`send_spec_input` holds it through enqueue/audit), so
///    a reset can't supersede the runtime between the in-lock refetch here
///    and harness registration — nor in the gap between recovery and the
///    caller's `observe` enqueue — eliminating the resurrect-stale-session
///    race;
/// 5. row-intrinsic dormancy (409) is checked before daemon liveness
///    (503), so an unrecoverable row tells the user to Reset rather than
///    to retry.
#[allow(deprecated)]
async fn ensure_live_spec_harness(
    s: &RouteState,
    w: &WorkerState,
    cs: &CodexShellState,
    card_id: &CardId,
) -> Result<(
    WorkerSessionProjection,
    crate::harness::SpecHarness,
    Option<PerCardLockGuard>,
)> {
    let dormant = || {
        CalmError::SpecHarnessDormant(format!(
            "no recoverable spec harness session for card {card_id}; reset to start a session",
        ))
    };
    let runtime = s
        .repo
        .runtime_get_active_for_card(&card_id.to_string())
        .await?
        .ok_or_else(dormant)?;
    if let Some(harness) = s.harness.get(&runtime.id) {
        return Ok((runtime, harness, None));
    }

    let guard = lock_card(&s.spec_recovery_locks, card_id.as_str()).await;
    // Re-fetch under the lock and use only this row: `/spec/reset` may have
    // superseded the pre-lock runtime, and a racing Send may have already
    // recovered the harness.
    let runtime = s
        .repo
        .runtime_get_active_for_card(&card_id.to_string())
        .await?
        .ok_or_else(dormant)?;
    if let Some(harness) = s.harness.get(&runtime.id) {
        return Ok((runtime, harness, Some(guard)));
    }
    // #649 review round 3 — a `starting` row means `spec-harness-start` is
    // still in flight: the adapter writes the row (and, in the deferred
    // path, the thread id + snapshot) BEFORE `spawn_side_effect` registers
    // the harness. Recovering here would spawn a harness the start op then
    // shuts down and replaces, silently dropping any input queued on it.
    // 503 so the client retries once the start lands (a failed start is
    // compensated to `failed`/deleted, after which this 409s as dormant).
    // Recovery below is only for statuses that imply a previously-live
    // harness (running / idle / turn_pending).
    if runtime.status == RunStatus::Starting {
        return Err(CalmError::ServiceUnavailable(
            "spec harness is starting; retry shortly".into(),
        ));
    }
    // Row-intrinsic dormancy checks run BEFORE the daemon liveness probe:
    // an unrecoverable row must 409 (steering the user to Reset) even when
    // the daemon is down, instead of hiding behind a 503 "retry shortly".
    //
    // `HarnessSnapshot::from_value_strict` (inside recovery) panics on
    // unknown shapes — pre-validate so a corrupt row degrades to the typed
    // 409 instead of a 500-by-panic.
    let snapshot_value = match runtime.handle_state_json.as_ref() {
        Some(value) if is_harness_snapshot_value(value) => value,
        _ => return Err(dormant()),
    };
    // A half-failed start can leave an active row without a thread; a
    // harness recovered from it would queue messages forever. Mirror boot
    // recovery (`spawn_recovered_harness`), which falls back to the
    // snapshot's `last_thread_id` when the row's `thread_id` is NULL —
    // only when BOTH are absent is the row truly unrecoverable.
    let has_thread = |t: Option<&str>| t.map(str::trim).is_some_and(|trimmed| !trimmed.is_empty());
    if !has_thread(runtime.thread_id.as_deref())
        && !has_thread(snapshot_value.get("last_thread_id").and_then(Value::as_str))
    {
        return Err(dormant());
    }
    // A recovered harness can't issue turns without the shared app-server;
    // surface backpressure instead of spawning a silently-wedged task.
    if !cs.shared_codex_appserver.is_running() {
        return Err(CalmError::ServiceUnavailable(
            "shared codex app-server is not running; retry shortly".into(),
        ));
    }
    let runtime_id = runtime.id.clone();
    let harness = crate::harness::spawn_recovered_harness(
        w.repo.clone(),
        s.events.clone(),
        s.write.role_cache().clone(),
        s.write.cove_cache().clone(),
        cs.shared_codex_appserver.clone(),
        &s.harness,
        runtime.clone(),
    )
    .await?
    .ok_or_else(dormant)?;
    tracing::info!(
        card_id = %card_id,
        runtime_id = %runtime_id,
        "spec harness lazily recovered on /spec/input registry miss"
    );
    // #649 review round 4 — return the guard so the caller keeps the
    // per-card lock alive through `harness.observe` and the audit event;
    // dropping it here would let a concurrent `/spec/reset` supersede the
    // recovered runtime before the message is enqueued.
    Ok((runtime, harness, Some(guard)))
}

#[utoipa::path(
    post,
    path = "/api/cards/{id}/spec/reset",
    tag = "cards",
    params(("id" = String, Path, description = "Spec card id")),
    responses(
        (status = 200, description = "Spec session reset", body = ResetSpecCardResponse),
        (status = 403, description = "Card is not a spec codex card", body = ErrorBody),
        (status = 404, description = "Card not found", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn reset_spec_card(
    State(s): State<RouteState>,
    actor: Actor,
    Path(id): Path<String>,
) -> Result<Json<ResetSpecCardResponse>> {
    let card = s
        .repo
        .card_get(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("card {id}")))?;
    let role = s
        .write
        .verify_role(&card.id)
        .ok_or_else(|| CalmError::NotFound(format!("card {id}")))?;
    if card.kind != "codex" || role != CardRole::Spec {
        return Err(CalmError::Forbidden(format!(
            "card {id} is not a spec codex card",
        )));
    }
    let response = reset_spec_card_shared(s, actor, card).await?;
    Ok(Json(response))
}

async fn reset_spec_card_shared(
    s: RouteState,
    actor: Actor,
    card: Card,
) -> Result<ResetSpecCardResponse> {
    // #649 review round 1 — reset takes the SAME per-card lock as the
    // `/spec/input` lazy-recovery path (`ensure_live_spec_harness`).
    // Without it, a reset racing a registry-miss Send could supersede the
    // runtime after recovery's in-lock refetch but before harness
    // registration, resurrecting the reset-away session (and routing the
    // just-sent message to the dead thread). Holding the lock across the
    // start+shutdown operations is deadlock-free: both adapters either
    // take no locks (shutdown) or use their own private map
    // (`per_card_mint_locks` in the start adapter) — neither can re-enter
    // `spec_recovery_locks`.
    let _recovery_guard = lock_card(&s.spec_recovery_locks, card.id.as_str()).await;
    let active_runtime = s
        .repo
        .runtime_get_active_for_card(&card.id.to_string())
        .await?;
    reset_spec_harness_card(s, actor, card, active_runtime).await
}

async fn reset_spec_harness_card(
    s: RouteState,
    actor: Actor,
    card: Card,
    runtime: Option<WorkerSessionProjection>,
) -> Result<ResetSpecCardResponse> {
    let wave = s
        .repo
        .wave_get(card.wave_id.as_str())
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("wave {}", card.wave_id)))?;

    let goal = wave.title.trim().to_string();
    let start_request = SpecHarnessStartOperationPayload {
        actor: actor.to_actor_id(),
        wave_id: wave.id.to_string(),
        spec_card_id: card.id.clone(),
        report_card_id: None,
        sort: None,
        cwd: wave.cwd.clone(),
        goal: (!goal.is_empty()).then_some(goal),
        reset_harness_items: true,
        force_new_thread: true,
    };
    let start_payload = serde_json::to_value(start_request)?;
    run_spec_card_operation(&s, "spec-harness-start", start_payload).await?;

    if let Some(runtime) = runtime {
        let shutdown_payload = serde_json::to_value(SpecHarnessShutdownOperationPayload {
            runtime_id: runtime.id.clone(),
        })?;
        run_spec_card_operation(&s, "spec-harness-shutdown", shutdown_payload).await?;
    }

    let active = s
        .repo
        .runtime_get_active_for_card(&card.id.to_string())
        .await?
        .ok_or_else(|| CalmError::Internal(format!("runtime for card {} missing", card.id)))?;
    let new_thread_id = active.thread_id.clone().ok_or_else(|| {
        CalmError::Internal(format!(
            "spec harness reset succeeded without a thread_id for card {}",
            card.id
        ))
    })?;
    let wave = s
        .repo
        .wave_get(card.wave_id.as_str())
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("wave {}", card.wave_id)))?;

    Ok(ResetSpecCardResponse {
        card_id: card.id,
        terminal_id: String::new(),
        new_thread_id,
        wave: Some(wave),
    })
}

async fn run_spec_card_operation(s: &RouteState, kind: &str, payload: Value) -> Result<()> {
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
    // Look up first so we have the wave_id for the delete event.
    let card = s
        .repo
        .card_get(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("card {id}")))?;
    // Issue #229 PR A — kernel-owned card guard. Spec cards (and PR B's
    // report cards) carry `deletable = false`; refuse direct REST delete.
    // Wave delete via `DELETE /api/waves/:id` still cascades through the
    // FK chain — the guard fires only on this `/api/cards/:id` path.
    if !card.deletable {
        return Err(CalmError::Forbidden(format!(
            "card {id} is kernel-owned and cannot be deleted via this endpoint; \
             delete the parent wave to remove it",
        )));
    }
    let card_id = card.id.clone();
    let wave_id = card.wave_id.clone();
    let scope = card_scope(s.repo.as_ref(), card_id.clone(), wave_id.clone()).await?;

    interrupt_shared_card_active_turn(s.repo.as_ref(), &cs, &card).await;

    // Issue #197 — eager teardown. The `terminals.card_id` FK is
    // `ON DELETE RESTRICT` (migration 0011); the row must be removed,
    // and its daemon + socket reaped, *before* the card row delete
    // fires. Pre-fetch the terminal (if any), kill the daemon, unlink
    // the socket — all outside the write txn (no point holding it open
    // for an I/O step that may take a few hundred ms in the worst
    // graceful-Kill-timeout case). Then the write txn deletes both the
    // terminal row and the card row inside one commit, keeping the
    // audit signal coherent (`Event::CardDeleted` is the headline; the
    // terminal row delete rides under it without a separate event —
    // same shape as wave-delete cascading through cards). If cleanup
    // fails *before* the txn opens we surface 500; the row stays and
    // the sweeper retries on the next tick, so we don't end up with
    // a half-torn-down terminal. Spec cards (CardRole::Spec) take the
    // same path: terminals share one table with no role-specific cleanup
    // divergence.
    let term = s.repo.terminal_get_by_card(card_id.as_str()).await?;
    if let Some(t) = term.as_ref() {
        reap_terminal_artifacts_with_renderer(Some(w.terminal_renderer.as_ref()), t).await;
    }
    let terminal_id = term.map(|t| t.id);

    let write_for_tx = s.write.clone();
    let (_unit, _id) = write_with_event_typed(
        s.repo.as_ref(),
        actor.to_actor_id(),
        scope,
        None,
        &s.events,
        &s.write,
        move |tx| {
            Box::pin(async move {
                // Drop the terminal row first so the RESTRICT FK lets the
                // card delete through. Idempotent: NotFound is OK (the
                // sweeper may have raced us, or the card had no terminal
                // to begin with).
                if let Some(tid) = terminal_id.as_deref() {
                    match terminal_delete_tx(tx, tid).await.map_err(CalmError::from) {
                        Ok(()) => {}
                        Err(CalmError::NotFound(_)) => {}
                        Err(e) => return Err(e),
                    }
                }
                card_delete_tx(tx, card_id.as_ref(), write_for_tx.role_cache()).await?;
                Ok((
                    (),
                    Event::CardDeleted {
                        id: card_id,
                        wave_id,
                    },
                ))
            })
        },
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}
