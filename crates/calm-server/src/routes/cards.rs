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
use crate::routes::settings::load_settings;
use crate::routes::waves::{SpawnPushAppserverMode, spawn_push_appserver};
use crate::spec_card::seed_and_spawn_spec_daemon;
use crate::state::AppState;
use crate::terminal_sweeper::{reap_spec_push, reap_terminal_artifacts};
use crate::validation::validate_card_payload;

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
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
    State(s): State<AppState>,
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
    validate_card_payload(&kind, &payload).map_err(|e| e.into_response())?;
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
    let cache = s.card_role_cache.clone();
    let (card, _id) = write_with_event_typed(
        s.repo.as_ref(),
        actor.to_actor_id(),
        scope,
        None,
        &s.events,
        &s.card_role_cache,
        &s.wave_cove_cache,
        move |tx| {
            Box::pin(async move {
                // Issue #229 PR A — plain user-driven creates are
                // user-deletable. The `false` path is reserved for
                // kernel-owned cards minted by internal code (spec card
                // here in PR A; report card in PR B).
                let card =
                    card_create_with_id_tx(tx, card_id_for_tx, new, CardRole::Plain, true, &cache)
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
    validate_card_payload(&creation.resource_uri, &payload).map_err(|e| e.into_response())?;
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
    let cache = s.card_role_cache.clone();
    let (card, _id) = write_with_event_typed(
        s.repo.as_ref(),
        actor,
        scope,
        Some(&correlation),
        &s.events,
        &s.card_role_cache,
        &s.wave_cove_cache,
        move |tx| {
            Box::pin(async move {
                // Issue #229 PR A — plain user-driven creates are
                // user-deletable. The `false` path is reserved for
                // kernel-owned cards minted by internal code (spec card
                // here in PR A; report card in PR B).
                let card =
                    card_create_with_id_tx(tx, card_id_for_tx, new, CardRole::Plain, true, &cache)
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
        validate_card_payload(kind, payload)?;
    }
    let scope = card_scope(s.repo.as_ref(), existing.id.clone(), existing.wave_id).await?;
    let (card, _id) = write_with_event_typed(
        s.repo.as_ref(),
        actor.to_actor_id(),
        scope,
        None,
        &s.events,
        &s.card_role_cache,
        &s.wave_cove_cache,
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
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ResetSpecCardResponse>> {
    let card = s
        .repo
        .card_get(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("card {id}")))?;
    let role = s
        .card_role_cache
        .get(&card.id)
        .ok_or_else(|| CalmError::NotFound(format!("card {id}")))?;
    if card.kind != "codex" || role != CardRole::Spec {
        return Err(CalmError::Forbidden(format!(
            "card {id} is not a spec codex card",
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
    let settings = load_settings(s.repo.as_ref()).await?;
    let mcp_token = crate::mcp_server::auth::CardMcpToken::generate().into_inner();
    if s.mcp_server.is_some() {
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
    if let (Some(server), Some(map)) = (s.mcp_server.as_ref(), env_for_spawn.as_object_mut()) {
        map.insert(
            "NEIGE_MCP_TOKEN".into(),
            serde_json::Value::String(mcp_token.clone()),
        );
        map.insert(
            "NEIGE_MCP_SOCKET".into(),
            serde_json::Value::String(server.shim_config.socket_path.to_string_lossy().to_string()),
        );
    }

    let card_id = card.id.clone();
    let wave_id = card.wave_id.clone();
    let terminal_id = terminal.id.clone();
    let dispatcher = s.dispatcher.clone();
    let reset = dispatcher
        .with_push_lock(&wave_id, async {
            reap_spec_push(&s, &wave_id).await;
            reap_terminal_artifacts(&s, &terminal).await;
            s.repo.terminal_set_pid(&terminal.id, None).await?;
            s.repo.terminal_set_exit(&terminal.id, None, false).await?;
            let card_at_lock = s
                .repo
                .card_get(card_id.as_str())
                .await?
                .ok_or_else(|| CalmError::NotFound(format!("card {card_id}")))?;
            let watermark = push_watermark_from_payload(&card_at_lock.payload);

            let push_args = spawn_push_appserver(
                &s,
                card_id.as_str(),
                &wave,
                &env_for_spawn,
                &settings,
                Some(mcp_token.as_str()),
                SpawnPushAppserverMode::ResetExisting {
                    mcp_token: mcp_token.clone(),
                },
            )
            .await?;

            crate::rehydrate_and_catch_up_parked_spec_push_under_lock(
                &s,
                card_id.as_str(),
                &wave_id,
                watermark,
            )
            .await;

            if let Err(e) = seed_and_spawn_spec_daemon(
                s.clone(),
                card_id.to_string(),
                wave_id.to_string(),
                wave.cwd.clone(),
                env_for_spawn,
                Some(mcp_token),
                push_args.clone(),
            )
            .await
            {
                reap_spec_push(&s, &wave_id).await;
                if let Err(clear_err) = s.repo.spec_card_clear_runtime_after_reset_failure(card_id.as_str()).await {
                    tracing::warn!(
                        card_id = %card_id,
                        error = %clear_err,
                        "spec reset: failed to clear reset-created appserver fields after terminal daemon spawn failure",
                    );
                }
                return Err(e);
            }

            Ok::<_, CalmError>(push_args.thread_id)
        })
        .await?;

    Ok(Json(ResetSpecCardResponse {
        card_id,
        terminal_id,
        new_thread_id: reset,
    }))
}

fn push_watermark_from_payload(payload: &serde_json::Value) -> i64 {
    payload
        .get("push_watermark")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0)
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
pub(crate) async fn delete_card(
    State(s): State<AppState>,
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
        reap_terminal_artifacts(&s, t).await;
    }
    let terminal_id = term.map(|t| t.id);

    let cache = s.card_role_cache.clone();
    let (_unit, _id) = write_with_event_typed(
        s.repo.as_ref(),
        actor.to_actor_id(),
        scope,
        None,
        &s.events,
        &s.card_role_cache,
        &s.wave_cove_cache,
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
                card_delete_tx(tx, card_id.as_ref(), &cache).await?;
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
