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
use crate::codex_appserver::{InputItem, Notification};
use crate::db::RepoRead;
use crate::db::sqlite::{
    card_create_with_id_tx, card_delete_tx, card_mcp_token_set_tx, card_update_tx,
    terminal_delete_tx,
};
use crate::db::{write_in_tx_typed, write_with_event_typed};
use crate::error::{CalmError, ErrorBody, Result};
use crate::event::{Event, EventScope};
use crate::ids::{ActorId, CardId, WaveId};
use crate::model::new_id;
use crate::model::{Card, CardPatch, CardRole, NewCard};
use crate::plugin_host::callbacks::extract_card_creation_from_tool_call_result;
use crate::routes::waves::{
    await_shared_spec_initial_turn_lifecycle, install_spec_push_sinks_and_park,
};
use crate::spec_card::{SpecPushDaemonArgs, seed_and_spawn_spec_daemon};
use crate::spec_push::{self, SpecPushPhase, SpecPushStatus};
use crate::state::{AppState, CodexShellState, RouteState, WorkerState};
use crate::terminal_sweeper::{
    reap_spec_push_from_registry, reap_terminal_artifacts_with_renderer,
};

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::broadcast;
use utoipa::ToSchema;

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

fn is_shared_codex_card(card: &Card) -> bool {
    card.payload.get("codex_source").and_then(Value::as_str) == Some("shared")
}

pub(crate) async fn interrupt_shared_card_active_turn(cs: &CodexShellState, card: &Card) {
    if !is_shared_codex_card(card) {
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
    let cards = s.repo.cards_by_wave(&wave_id).await?;
    Ok(Json(cards))
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
    let (card, _id) = write_with_event_typed(
        s.repo.as_ref(),
        actor.to_actor_id(),
        scope,
        None,
        &s.events,
        s.write(),
        move |tx| {
            Box::pin(async move {
                // Issue #229 PR A — plain user-driven creates are
                // user-deletable. The `false` path is reserved for
                // kernel-owned cards minted by internal code (spec card
                // here in PR A; report card in PR B).
                let card = card_create_with_id_tx(
                    tx,
                    card_id_for_tx,
                    new,
                    CardRole::Plain,
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
    let (card, _id) = write_with_event_typed(
        s.repo.as_ref(),
        actor,
        scope,
        Some(&correlation),
        &s.events,
        s.write(),
        move |tx| {
            Box::pin(async move {
                // Issue #229 PR A — plain user-driven creates are
                // user-deletable. The `false` path is reserved for
                // kernel-owned cards minted by internal code (spec card
                // here in PR A; report card in PR B).
                let card = card_create_with_id_tx(
                    tx,
                    card_id_for_tx,
                    new,
                    CardRole::Plain,
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
    let (card, _id) = write_with_event_typed(
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
    Ok(Json(card))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ResetSpecCardResponse {
    #[schema(value_type = String)]
    pub card_id: CardId,
    pub terminal_id: String,
    pub new_thread_id: String,
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
    State(w): State<WorkerState>,
    State(cs): State<CodexShellState>,
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
    let response = reset_spec_card_shared(s, w, cs, card).await?;
    Ok(Json(response))
}

fn push_watermark_from_payload(payload: &serde_json::Value) -> i64 {
    payload
        .get("push_watermark")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0)
}

struct SharedResetStarted {
    thread_id: String,
    notifications: broadcast::Receiver<Notification>,
    status: spec_push::SharedStatus,
    push_args: SpecPushDaemonArgs,
}

async fn reset_spec_card_shared(
    s: RouteState,
    w: WorkerState,
    cs: CodexShellState,
    card: Card,
) -> Result<ResetSpecCardResponse> {
    if !cs.shared_codex_appserver.is_running() {
        return Err(CalmError::Internal(format!(
            "shared codex daemon is not running for spec card reset {}",
            card.id
        )));
    }
    let terminal = s
        .repo
        .terminal_get_by_card(card.id.as_str())
        .await?
        .ok_or_else(|| {
            CalmError::Internal(format!("spec terminal row missing for card {}", card.id))
        })?;
    let wave = s
        .repo
        .wave_get(card.wave_id.as_str())
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("wave {}", card.wave_id)))?;
    if wave.title.trim().is_empty() {
        return Err(CalmError::Internal(
            "reset succeeded without a thread_id; empty-goal waves cannot be reset before their first turn".to_string(),
        ));
    }

    // Token rotation is DEFERRED until after spawn_reset_via_shared_daemon's
    // lifecycle wait has succeeded — otherwise an early thread_start/turn_start
    // failure would leave the still-running OLD TUI with an invalidated
    // NEIGE_MCP_TOKEN (the new token hash is already in card_mcp_tokens, and
    // the OLD TUI can no longer authenticate MCP calls). After the shared
    // thread is committed, the OLD TUI is reaped a few lines later so
    // invalidating its token is safe.

    let card_id = card.id.clone();
    let wave_id = card.wave_id.clone();
    let terminal_id = terminal.id.clone();
    let dispatcher = w.dispatcher.clone();
    let reset = dispatcher
        .with_push_lock(&wave_id, async {
            tracing::info!(
                target: "shared_codex_daemon::spec_card_reset",
                card_id = %card_id,
                wave_id = %wave_id,
                "reset_started"
            );
            let card_at_lock = s
                .repo
                .card_get(card_id.as_str())
                .await?
                .ok_or_else(|| CalmError::NotFound(format!("card {card_id}")))?;
            let watermark = push_watermark_from_payload(&card_at_lock.payload);

            let started = spawn_reset_via_shared_daemon(&s, &cs, card_id.as_str(), &wave).await?;
            // Rotate the per-card MCP token only AFTER the shared thread is
            // committed (thread_start + turn_start + lifecycle all passed).
            // From here on the OLD TUI is going to be reaped immediately, so
            // invalidating its token is the safe ordering.
            let mcp_token = crate::mcp_server::auth::CardMcpToken::generate().into_inner();
            if w.mcp_server.is_some() {
                let card_id_for_tx = card.id.to_string();
                let hashed = crate::mcp_server::auth::hash_token(&mcp_token);
                write_in_tx_typed(s.repo.as_ref(), move |tx| {
                    Box::pin(async move {
                        card_mcp_token_set_tx(tx, &card_id_for_tx, &hashed).await?;
                        Ok(())
                    })
                })
                .await?;
            }
            let mut env_for_spawn = terminal.env.clone();
            if let Some(map) = env_for_spawn.as_object_mut() {
                map.insert(
                    "CODEX_HOME".into(),
                    serde_json::Value::String(
                        cs.codex.codex_home_dir().to_string_lossy().to_string(),
                    ),
                );
                if let Some(server) = w.mcp_server.as_ref() {
                    map.insert(
                        "NEIGE_MCP_TOKEN".into(),
                        serde_json::Value::String(mcp_token.clone()),
                    );
                    map.insert(
                        "NEIGE_MCP_SOCKET".into(),
                        serde_json::Value::String(
                            server.shim_config.socket_path.to_string_lossy().to_string(),
                        ),
                    );
                }
            }
            reap_spec_push_from_registry(&w.spec_push, &wave_id).await;
            reap_terminal_artifacts_with_renderer(Some(w.terminal_renderer.as_ref()), &terminal)
                .await;
            s.repo.terminal_set_pid(&terminal.id, None).await?;
            s.repo.terminal_set_exit(&terminal.id, None, false).await?;
            persist_shared_reset_runtime_fields(
                &s,
                &cs,
                card_id.as_str(),
                &wave,
                &started.thread_id,
            )
            .await?;

            let handle = spec_push::park_shared_handle(
                cs.shared_codex_appserver.clone(),
                Some(started.thread_id.clone()),
                started.notifications,
                started.status,
                None,
                spec_push::TurnWatchdogConfig::default(),
            );
            install_spec_push_sinks_and_park(&s, &w, card_id.as_str(), &wave, handle).await;
            tracing::info!(
                target: "shared_codex_daemon::spec_card_reset",
                card_id = %card_id,
                wave_id = %wave_id,
                thread_id = %started.thread_id,
                "handle_replaced"
            );

            crate::rehydrate_and_catch_up_parked_spec_push_under_lock_parts(
                &s,
                &w,
                card_id.as_str(),
                &wave_id,
                watermark,
            )
            .await;

            if let Err(e) = seed_and_spawn_spec_daemon(
                s.clone(),
                w.clone(),
                card_id.to_string(),
                wave_id.to_string(),
                wave.cwd.clone(),
                env_for_spawn,
                Some(mcp_token),
                started.push_args.clone(),
            )
            .await
            {
                // TUI spawn failed but the shared thread + handle are healthy.
                // KEEP the handle parked — without it, the new thread's
                // notifications would have no consumer until a server restart
                // re-parked it, stranding spec output and queue catch-up.
                // The card payload + card_codex_threads row continue to point
                // at the new thread; the user retries the reset (or reloads
                // the card UI to remount the xterm onto the now-orphaned
                // backend). The 5xx response signals the partial failure.
                tracing::warn!(
                    target: "shared_codex_daemon::spec_card_reset",
                    card_id = %card_id,
                    wave_id = %wave_id,
                    thread_id = %started.thread_id,
                    error = %e,
                    "tui_spawn_failed_handle_kept_parked"
                );
                return Err(e);
            }

            tracing::info!(
                target: "shared_codex_daemon::spec_card_reset",
                card_id = %card_id,
                wave_id = %wave_id,
                thread_id = %started.thread_id,
                "reset_completed"
            );
            Ok::<_, CalmError>(started.thread_id)
        })
        .await?;

    Ok(ResetSpecCardResponse {
        card_id,
        terminal_id,
        new_thread_id: reset,
    })
}

async fn spawn_reset_via_shared_daemon(
    s: &RouteState,
    cs: &CodexShellState,
    spec_card_id: &str,
    wave: &crate::model::Wave,
) -> Result<SharedResetStarted> {
    if wave.title.trim().is_empty() {
        return Err(CalmError::Internal(
            "reset succeeded without a thread_id; empty-goal waves cannot be reset before their first turn".to_string(),
        ));
    }
    let old_mapping = s.repo.card_codex_thread_get_by_card(spec_card_id).await?;
    let mut notifications = cs.shared_codex_appserver.subscribe_notifications();
    let status: spec_push::SharedStatus =
        std::sync::Arc::new(tokio::sync::Mutex::new(SpecPushStatus::default()));
    let developer_instructions = crate::spec_card::render_system_prompt(
        crate::spec_card::SeededCardRole::Spec.prompt_template(),
        wave.id.as_str(),
    );
    let thread_id = cs
        .shared_codex_appserver
        .thread_start_for_card(
            spec_card_id,
            CardRole::Spec,
            Some(wave.id.as_str()),
            crate::shared_codex_appserver::SharedThreadStartParams {
                cwd: wave.cwd.clone(),
                approval_policy: "never".into(),
                sandbox_mode: "workspace-write".into(),
                developer_instructions: Some(developer_instructions),
            },
        )
        .await?;
    {
        let mut g = status.lock().await;
        g.phase = SpecPushPhase::Issuing;
        g.last_thread_id = Some(thread_id.clone());
    }
    tracing::info!(
        target: "shared_codex_daemon::spec_card_reset",
        card_id = %spec_card_id,
        wave_id = %wave.id,
        thread_id = %thread_id,
        "new_thread_started"
    );

    let turn_result = async {
        cs.shared_codex_appserver
            .turn_start(&thread_id, vec![InputItem::text(wave.title.trim())])
            .await?;
        await_shared_spec_initial_turn_lifecycle(&mut notifications, &thread_id, &status).await?;
        Ok::<(), CalmError>(())
    }
    .await;
    if let Err(e) = turn_result {
        if let Some(row) = old_mapping.as_ref() {
            if let Err(rollback_err) = s
                .repo
                .card_codex_thread_upsert(
                    spec_card_id,
                    &row.thread_id,
                    row.role,
                    row.wave_id.as_deref(),
                )
                .await
            {
                tracing::warn!(
                    target: "shared_codex_daemon::spec_card_reset",
                    card_id = %spec_card_id,
                    thread_id = %thread_id,
                    rollback_error = %rollback_err,
                    "failed to restore old card_codex_thread mapping after shared reset turn_start failure"
                );
            }
        } else if let Err(rollback_err) =
            s.repo.card_codex_thread_delete_by_card(spec_card_id).await
        {
            tracing::warn!(
                target: "shared_codex_daemon::spec_card_reset",
                card_id = %spec_card_id,
                thread_id = %thread_id,
                rollback_error = %rollback_err,
                "failed to delete new card_codex_thread mapping after shared reset turn_start failure"
            );
        }
        tracing::warn!(
            target: "shared_codex_daemon::spec_card_reset",
            card_id = %spec_card_id,
            wave_id = %wave.id,
            thread_id = %thread_id,
            error = %e,
            "turn_start_failed_rolled_back"
        );
        return Err(e);
    }

    if let Some(row) = old_mapping.as_ref()
        && row.thread_id != thread_id
        && let Err(e) = cs
            .shared_codex_appserver
            .interrupt_active_turn(&row.thread_id)
            .await
    {
        tracing::warn!(
            target: "shared_codex_daemon::spec_card_reset",
            card_id = %spec_card_id,
            wave_id = %wave.id,
            old_thread_id = %row.thread_id,
            new_thread_id = %thread_id,
            error = %e,
            "failed to interrupt old active shared codex turn after reset"
        );
    }

    Ok(SharedResetStarted {
        thread_id: thread_id.clone(),
        notifications,
        status,
        push_args: SpecPushDaemonArgs {
            thread_id: Some(thread_id),
            sock_uri: cs.shared_codex_appserver.remote_uri(),
            developer_instructions: None,
        },
    })
}

async fn persist_shared_reset_runtime_fields(
    s: &RouteState,
    cs: &CodexShellState,
    spec_card_id: &str,
    wave: &crate::model::Wave,
    thread_id: &str,
) -> Result<()> {
    let scope = EventScope::Card {
        card: spec_card_id.into(),
        wave: wave.id.clone(),
        cove: wave.cove_id.clone(),
    };
    let card_id_for_tx = spec_card_id.to_string();
    let thread_id_for_tx = thread_id.to_string();
    let remote_uri = cs.shared_codex_appserver.remote_uri();
    let (_card, _id) = write_with_event_typed(
        s.repo.as_ref(),
        ActorId::Kernel,
        scope,
        None,
        &s.events,
        &s.write,
        move |tx| {
            Box::pin(async move {
                let mut payload = s_repo_card_get(tx, &card_id_for_tx).await?;
                let Some(map) = payload.as_object_mut() else {
                    return Err(CalmError::Internal(format!(
                        "spec card {card_id_for_tx} payload is not a JSON object; cannot persist shared reset runtime fields"
                    )));
                };
                map.insert(
                    "codex_thread_id".into(),
                    serde_json::Value::String(thread_id_for_tx),
                );
                map.insert(
                    "codex_source".into(),
                    serde_json::Value::String("shared".into()),
                );
                map.insert("appserver_sock".into(), serde_json::Value::String(remote_uri));
                map.remove("appserver_pgid");
                map.remove("appserver_start_time");
                map.remove("appserver_boot_id");
                map.remove("appserver_needs_initial_prompt");
                let card = card_update_tx(
                    tx,
                    &card_id_for_tx,
                    CardPatch {
                        kind: None,
                        sort: None,
                        payload: Some(payload),
                        deletable: None,
                    },
                )
                .await?;
                Ok((card.clone(), Event::CardUpdated(card)))
            })
        },
    )
    .await?;
    Ok(())
}

async fn s_repo_card_get(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    card_id: &str,
) -> Result<serde_json::Value> {
    let row: Option<(String,)> = sqlx::query_as("SELECT payload FROM cards WHERE id = ?1")
        .bind(card_id)
        .fetch_optional(&mut **tx)
        .await?;
    let payload_text = row
        .ok_or_else(|| CalmError::NotFound(format!("card {card_id}")))?
        .0;
    serde_json::from_str(&payload_text)
        .map_err(|e| CalmError::Internal(format!("card {card_id} payload is not valid JSON: {e}")))
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

    interrupt_shared_card_active_turn(&cs, &card).await;

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
    // same path: both plain and spec terminals live in the same
    // `terminals` table with no role-specific cleanup divergence.
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
                    match terminal_delete_tx(tx, tid).await {
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
