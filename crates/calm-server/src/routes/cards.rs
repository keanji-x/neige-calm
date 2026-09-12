//! `/api/cards`, `/api/tracks/:id/cards` — Card CRUD. **Owned by Track B.**
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
use crate::db::{RepoRead, RouteRepo};
use crate::db::{write_with_actor_events_typed, write_with_event_typed};
use crate::error::{CalmError, ErrorBody, Result};
use crate::event::{Event, EventScope, RatifyDecision};
use crate::harness::{HarnessPhaseTag, QueueEntry, TokenUsage, is_harness_snapshot_value};
use crate::ids::{ActorId, CardId, TrackId};
use crate::model::{
    Card, CardPatch, CardRole, HarnessItem, NewCard, Track, TrackLifecycle, new_id,
};
use crate::operation::planner_harness_interrupt_adapter::PlannerHarnessInterruptOperationPayload;
use crate::operation::planner_harness_shutdown_adapter::PlannerHarnessShutdownOperationPayload;
use crate::operation::planner_harness_start_adapter::{
    HarnessProfile, PlannerHarnessStartOperationPayload,
};
use crate::operation::workspace_lease::release_workspace_lease_for_card_tx;
use crate::operation::{OperationKey, OperationOutcome};
use crate::per_card_lock::{PerCardLockGuard, lock_card};
use crate::plugin_host::callbacks::extract_card_creation_from_tool_call_result;
use crate::ratify_state::ratify_request_pending_tx;
use crate::routes::terminal_cards::{calm_error_from_operation_failure, stable_payload_hash};
use crate::session_projection_lookup::{
    card_is_shared_planner, project_runtime_into_card_payload, project_runtime_into_cards_payload,
};
use crate::session_projection_repo::{WorkerSessionProjection, WorkerSessionState};
use crate::state::{AppState, CodexShellState, RouteState, WorkerState};
use crate::terminal_sweeper::reap_terminal_artifacts_with_renderer;
use crate::track_lifecycle::apply_requested_transition_in_tx;
use crate::validation::reject_client_supplied_terminal_signals;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use calm_types::planner_attachment::{AttachmentId, PlannerAttachment};
use calm_types::worker::WorkerSessionId;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use utoipa::{IntoParams, ToSchema};

/// Resolve the (track, area) ancestor pair for a track id, returning a
/// pre-built [`EventScope::Card`] for the given card. PR2 of #136 needs
/// this at every card-emit site so the event row's `scope_*` columns
/// carry the full ancestor chain. Looking up the track outside the txn
/// is fine — track rows are immutable wrt their parent area.
///
/// # ⚠️ Do not call this from inside a transaction that has written `tracks`
///
/// This reads through the **pool**, i.e. a second connection. Since #1147 S6
/// every terminal-row creation writes `tracks` (the workspace freeze), so calling
/// this after one — in the same transaction, before it commits — deadlocks the
/// task against its own lock. Use [`card_scope_tx`], whose doc comment states
/// the general rule and its measurement. Resolving the scope *before* the write,
/// which is what most adapters do, is equally correct.
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
///
/// # The rule, stated at its real width (#1147 S6)
///
/// **A transaction must not read, off the pool, any table it has itself written,
/// before it commits.** The pool hands out a *second* connection; under SQLite's
/// shared cache (which every in-memory test database uses) locks are per table,
/// so that read blocks on a lock only the caller can release and the task waits
/// on itself forever in `sqlx_sqlite::statement::unlock_notify::wait`.
///
/// `tracks` is not special — it is merely the table S6 added to the write set of
/// every terminal-creating transaction (each terminal row freezes its track's
/// workspace). `cards` and `terminals` were already in that set. Measured:
/// `ClaudeRestartAdapter::prepare_tx` created the terminal row and then read
/// `tracks` through [`card_scope`], and hung
/// `post_claude_restart_recreates_missing_terminal_row_and_resumes_session`
/// forever.
///
/// Scope of the hang, stated precisely rather than generously: in-memory
/// shared-cache databases deadlock hard. A file-backed production database runs
/// WAL, where the pool connection reads a snapshot instead of blocking — so
/// production gets a *pre-write read*, not a hang. Neither is acceptable and the
/// fix is the same, but do not call this "test-only" and do not claim production
/// hangs.
///
/// # No mechanical enforcement
///
/// Nothing scans for violations. The coverage is a single 20-second wall clock
/// on a single flow
/// (`claude_card_endpoint::post_claude_restart_does_not_deadlock_on_the_workspace_freeze`),
/// which makes a regression *on that flow* a legible red test instead of a
/// wedged job. A new adapter that mints a terminal row and then reads `tracks`
/// off `self.repo` will hang CI with no diagnosis and no test naming the cause.
/// The tree was swept once, at S6 — every other adapter resolves its scope
/// BEFORE creating the card, which is equally correct — and a sweep is a
/// measurement of one moment, not a guarantee.
///
/// So: any `prepare_tx` that mints a card + terminal resolves its scope through
/// this function, or resolves it before the write.
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

/// Whether the persisted card shape is allowed to use the headless harness
/// routes. Unknown/malformed profile values deliberately fail closed.
///
/// #1189 added the assistant arm, and it is not decoration: this predicate
/// gates `POST /api/cards/{id}/planner/input`, which is how a track conversation's
/// messages — including the first one, sent by
/// `POST /api/tracks/{id}/conversations` — reach the harness. Without it the
/// endpoint mints a card it can then never talk to.
pub(crate) fn card_runs_headless_harness(card: &Card, role: CardRole) -> bool {
    card.kind == "codex"
        && (role == CardRole::Planner
            || crate::plain_chat::card_is_plain_chat(card, Some(role), true)
            || crate::plain_chat::card_is_track_assistant(card, Some(role), true))
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

/// Deletion-grade form of [`interrupt_shared_card_active_turn`]. Every lookup
/// and interrupt failure is propagated; a destructive workspace move may only
/// follow a confirmed quiesce, never a best-effort warning.
pub(crate) async fn quiesce_shared_card_active_turn(
    repo: &dyn RouteRepo,
    cs: &CodexShellState,
    card: &Card,
) -> Result<Option<String>> {
    let active_runtime = repo
        .session_projection_active_for_card(&card.id.to_string())
        .await?;
    if card_is_shared_planner(card, active_runtime.as_ref()) {
        let thread_id = active_runtime
            .as_ref()
            .and_then(crate::harness::effective_runtime_thread_id);
        let mut seals = crate::shared_codex_appserver::DeletionThreadSeals::new(
            cs.shared_codex_appserver.clone(),
        );
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
        .route("/api/cards/{id}/planner/input", post(send_planner_input))
        // #1505 PR2. The handlers live in `routes::planner_input`, but the
        // route is mounted here because this router owns `/api/cards/{id}/**`
        // and a second router on the same prefix is how two mounts start
        // disagreeing about a middleware.
        .route(
            "/api/cards/{id}/planner/input/{entry_id}",
            axum::routing::patch(crate::routes::planner_input::edit_planner_input)
                .delete(crate::routes::planner_input::delete_planner_input),
        )
        .route("/api/cards/{id}/ratify", post(ratify_card))
        .route(
            "/api/cards/{id}/planner/interrupt",
            post(interrupt_planner_card),
        )
        // #1505 S4-3. Same reason as the PR2 route above: this router owns
        // `/api/cards/{id}/**`, and a second router on the prefix is how two
        // mounts start disagreeing about a middleware.
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

    let after_id = q.after_id.unwrap_or(0).max(0);
    let limit = q.limit.unwrap_or(100).clamp(0, 500);
    let descending = q.direction == HarnessItemsDirection::Desc;
    // The transcript-only read, not the raw one (#1255). `limit` is the
    // frontend's page budget (300 rows), and it must be spent entirely on rows
    // the transcript renders: `harness_items` also holds captured
    // `turn/plan/updated` frames, and every one of those returned here would
    // displace a real transcript row behind "Load earlier". The narrowing is in
    // the SQL — see `RepoRead::harness_item_list_transcript_by_card`, including
    // the note for the UI slice that will want to read plan rows.
    let mut items = s
        .repo
        .harness_item_list_transcript_by_card(card.id.as_str(), after_id, limit, descending)
        .await?;
    // #1505 S6 review — the stored blob keeps the path, the wire does not.
    //
    // Redacted at the serialization boundary rather than at the write, because
    // the stored blob is a verbatim record of what codex sent and is read for
    // replay and diagnosis; rewriting it on the way IN would make the stored
    // row a second, quieter truth. The frontend never reads a path from
    // here — attachments reach the transcript through
    // `HarnessInputSegment.attachments`, as an id and a server-built url — so
    // nothing downstream loses anything.
    for item in &mut items {
        item.params = crate::planner_attachments::redact_local_image_paths(&item.params);
    }
    Ok(Json(items))
}

/// Body payload accepted by `POST /api/tracks/:track_id/cards`.
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
    /// Legacy direct-create fields. Mirrors `NewCard` shape; `track_id` is
    /// taken from the path so we omit it here.
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub sort: Option<f64>,
    #[serde(default)]
    #[schema(value_type = Option<Object>)]
    pub payload: Option<Value>,
    #[serde(default)]
    pub title: Option<String>,
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
    path = "/api/tracks/{track_id}/cards",
    tag = "cards",
    params(("track_id" = String, Path, description = "Track id this card belongs to")),
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
#[allow(clippy::result_large_err)]
pub(crate) async fn create_card(
    State(s): State<AppState>,
    actor: Actor,
    Path(track_id): Path<String>,
    Json(body): Json<CreateCardBody>,
) -> Result<Response, Response> {
    // M2: tool-call path wins over direct-create. The tool-call branch
    // overrides the actor to `"plugin:<id>"` (the entity actually making
    // the kernel write) regardless of any `X-Calm-Actor` header — plugins
    // cannot spoof their own actor via REST (design §9 bullet 2/3).
    if let Some(via) = body.via_tool_call {
        return create_via_tool_call(&s, track_id, via).await;
    }

    // Direct-create path (legacy / pre-M2). `kind` is required here — for
    // tool-call the kernel synthesizes it from the resource URI.
    let kind = body.kind.ok_or_else(|| {
        CalmError::BadRequest("create card body needs either `kind` or `via_tool_call`".into())
            .into_response()
    })?;
    let payload = body.payload.unwrap_or(Value::Null);
    // #1620 — hook-routing provenance is kernel-stamped, never accepted from
    // a client (any kind): see `validation::reject_client_supplied_terminal_signals`.
    reject_client_supplied_terminal_signals(&payload)
        .map_err(|e| CalmError::from(e).into_response())?;
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
    let track_id: TrackId = track_id.into();
    let scope = card_scope(s.repo.as_ref(), card_id.clone(), track_id.clone())
        .await
        .map_err(|e| e.into_response())?;
    let new = NewCard {
        track_id,
        kind,
        sort: body.sort,
        payload,
        title: body.title,
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
                // kernel-owned cards minted by internal code (planner card
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
/// doc's M2 planner:
///   * plugin not running → 404
///   * `permissions.cards_create` not granted → 403
///   * tool returned `isError: true` → 502 with content joined as text
///   * tool succeeded but omitted `_meta.ui.resourceUri` → 422
///     `{"error":"...","code":"not_a_card_tool"}`
#[allow(deprecated)]
#[allow(clippy::result_large_err)]
async fn create_via_tool_call(
    s: &AppState,
    track_id: String,
    via: ViaToolCall,
) -> Result<Response, Response> {
    // 1. Plugin must be a RUNNING `app`. `mcp_client` returns None when the
    //    plugin is Disabled / Crashed / not yet spawned — and, since #1164
    //    §2.6, also when it is a connector (`mcp-http` / `cli-query`), which
    //    has no stdio client.
    //
    //    Card creation stays stdio-only on purpose: it depends on the plugin
    //    answering with `_meta.ui.resourceUri`, i.e. on it owning a `ui://`
    //    view, which a remote MCP server or a query CLI structurally cannot.
    //
    //    The two cases must NOT share an error. Telling an operator that a
    //    demonstrably-Running connector "is not running" sends them to debug
    //    the wrong thing (design doc §2.6, "一处措辞修正").
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

    // 2. Manifest-based permission gate. Mirrors the autonomous
    //    `neige.card.create` gate in `callbacks.rs::card_create`: the
    //    plugin must have `permissions.cards_create == true`. The
    //    migration doc speaks of `permissions.cards.create` with `track`
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
        // No Track: this path is an iframe asking its own plugin to mint a
        // card, and the plugin already names the destination in `via`. When a
        // view needs per-Track state, the Track should come from the view's
        // own binding rather than be inferred here.
        .tools_call(&via.tool_name, via.arguments, None)
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
    // #1620 — a plugin's `structuredContent` is client input for this
    // purpose: the provenance marker is never accepted from it.
    reject_client_supplied_terminal_signals(&payload)
        .map_err(|e| CalmError::from(e).into_response())?;
    // D4: validate even on the tool-call path. In practice `ui://*` kinds
    // are opaque so this is a no-op for plugin-defined views — but if a
    // tool ever names a kernel kind (e.g. `"terminal"`) via resourceUri,
    // we reject a malformed payload here rather than after the DB write.
    s.card_kind_registry()
        .validate_payload(&creation.resource_uri, &payload)
        .map_err(|e| CalmError::from(e).into_response())?;
    let new = NewCard {
        track_id: track_id.into(),
        kind: creation.resource_uri,
        sort: None,
        payload,
        title: None,
    };
    // M2 tool-call writes: actor stays `Plugin(<id>)` (the entity making
    // the kernel write), `correlation` records the user-driven invocation
    // so audit queries can reconstruct the causal chain (design §9 bullet 3).
    // PR2 of #136 pre-mints the card id so `EventScope::Card { card, .. }`
    // is determinable before the txn opens.
    let actor = ActorId::Plugin(via.plugin_id.clone());
    let correlation = format!("user_tool_call:{}", via.tool_name);
    let card_id = CardId::from(new_id());
    let track_id_for_scope: TrackId = new.track_id.clone();
    let scope = card_scope(s.repo.as_ref(), card_id.clone(), track_id_for_scope)
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
                // kernel-owned cards minted by internal code (planner card
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
        (status = 400, description = "Card patch violates an invariant", body = ErrorBody),
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
    // We need the existing card's track_id for the EventScope chain
    // regardless of whether validation needs the kind. Fetch once.
    let existing = s
        .repo
        .card_get(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("card {id}")))?;
    // D4: if the patch carries a payload, validate it against the kind that
    // will land in the DB. The kind is either the patch's new kind (when the
    // patch retargets) or the existing card's kind.
    //
    // #1620 — the payload may not carry the hook-routing provenance marker
    // (server-owned, any kind → 400). Dropping it by omission is impossible:
    // `card_update_tx` re-stamps the marker onto any replacement payload of
    // a card that already carries it (and refuses a non-object replacement).
    if let Some(payload) = p.payload.as_ref() {
        reject_client_supplied_terminal_signals(payload)?;
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
pub struct SendPlannerInputRequest {
    pub text: String,
    /// #1505 S6 — ids returned by `POST /api/cards/{id}/planner/attachments`.
    ///
    /// Naming an attachment here is what BINDS it: the bytes move out of the
    /// server's sweepable staging area before this request writes anything to
    /// the queue. So a message that reaches the queue always names files that
    /// are already permanent, and an upload that is never named expires.
    ///
    /// `#[serde(default)]` so every existing client keeps working unchanged.
    /// An id belonging to another card is a 400, as is naming the same one
    /// twice or naming more than eight.
    #[serde(default)]
    pub attachments: Vec<AttachmentId>,
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
pub struct SendPlannerInputResponse {
    #[schema(value_type = String)]
    pub card_id: CardId,
    pub worker_session_id: String,
    /// #1505 PR1 — stable id of the queue entry this text landed in, so the
    /// client can match its optimistic echo against `GET /planner/run`'s
    /// `pending` instead of against the text.
    ///
    /// Null in exactly one accepted case: the text folded into a queue entry
    /// written before #1505 PR1, which has no id and never gains one. The other
    /// ways a client sees no id are refusals with a non-200 status (dormant
    /// harness, 503 saturated queue, 409 shutting down), not this field.
    pub entry_id: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct InterruptPlannerCardResponse {
    #[schema(value_type = String)]
    pub card_id: CardId,
    pub worker_session_id: String,
    /// True when a turn was actually running and an interrupt was
    /// dispatched at it; false when the harness was idle (graceful no-op)
    /// or a `turn/start` was still in flight (interrupt dispatched
    /// best-effort, but not guaranteed to land — press Stop again once the
    /// turn is running). "stopped: true" means the interrupt was *issued* —
    /// completion is asynchronous (`turn/aborted` lands via the harness
    /// FSM, with an interrupt-timeout watchdog as backstop).
    pub stopped: bool,
}

/// Issue #668 fix — current planner-harness run snapshot for a card.
///
/// `harness.phase.changed` is the only live phase signal, so a page opened
/// mid-turn would otherwise sit on `phase: null` until the next transition.
/// This read endpoint lets the client seed its initial phase. Dormancy (no
/// active runtime row, or no registered harness) is NOT an error here —
/// it's the `{worker_session_id: null, phase: null}` answer.
#[derive(Debug, Serialize, ToSchema)]
pub struct GetPlannerRunResponse {
    #[schema(value_type = String)]
    pub card_id: CardId,
    /// Active worker-session id, or null when the harness is dormant.
    pub worker_session_id: Option<String>,
    /// Current harness phase, or null when the harness is dormant.
    pub phase: Option<HarnessPhaseTag>,
    /// #1255 S3 — latest context-window usage, or null when the harness is
    /// dormant or codex has not pushed a `thread/tokenUsage/updated` frame yet.
    ///
    /// Known consequence, recorded rather than fixed in this commit: a dormant
    /// conversation reports `null` here even though the reading IS on disk in
    /// `worker_sessions.handle_state`. This whole endpoint reads the live
    /// in-memory harness (registry hit on the active runtime row) and answers
    /// `phase: null` when there is none — see the handler. Token usage
    /// inherits that exactly, so a card whose harness has been shut down shows
    /// no meter until something respawns it. The UI slice decides whether
    /// that is acceptable or whether the dormant path should fall back to the
    /// persisted snapshot; the kernel slice does not pick for it.
    pub token_usage: Option<PlannerRunTokenUsage>,
    /// #1505 S4-3 — the model slug this conversation's turns run with, or
    /// `null` for "follow whatever this installation is configured to use".
    ///
    /// Read off the CARD, not off the harness, and therefore answered for a
    /// dormant conversation as well: the selection is a property of the
    /// conversation and outlives every runtime that serves it. That is the
    /// opposite of `phase` and `token_usage` above, which are properties of a
    /// live runtime and are `null` without one.
    pub model: Option<String>,
    /// The chosen reasoning effort, or `null` for the default. Same source and
    /// same reasoning as [`GetPlannerRunResponse::model`].
    pub reasoning_effort: Option<String>,
    /// #1505 S4 — why this conversation's queue is not draining, or `null`
    /// when there is nothing worth saying. `null` almost always.
    ///
    /// Three things fill it, and a client should render all three as the same
    /// kind of standing notice rather than as an error about a request it just
    /// made:
    ///
    ///  * the model or effort to run under cannot be determined (codex's
    ///    configuration names none, or the stored selection is unreadable) —
    ///    the text names the choice that fixes it;
    ///  * codex refused to start the turn — the text says the message was NOT
    ///    sent, and promises no delivery;
    ///  * codex has been unreachable long enough that silence would look like
    ///    a hang — the text says the message is still queued and will go out.
    ///
    /// A brief outage fills nothing, so this staying `null` is not evidence
    /// that anything succeeded.
    ///
    /// It is not a general per-turn error channel and does not diagnose why a
    /// model failed mid-turn; that is #1507's.
    pub blocked_reason: Option<String>,
    /// #1505 PR1 — the addressable user entries still waiting for the next
    /// turn, in queue order. Empty when the harness is dormant.
    ///
    /// Only entries minted at or after PR1 appear here. Dispatcher
    /// observations never do (they are not the user's and cannot be edited),
    /// and neither do user entries from pre-PR1 snapshots, which have no id to
    /// address them by; both kinds of omission are counted in
    /// `pending_overflow` only for the user-authored ones.
    // Cost, paid now and collected in PR4 (#1514 review) — kept off the wire
    // description because it is a note to this repo, not to an API consumer.
    // This ships unconditionally, with full bodies, and nothing reads it yet:
    // `fe`'s non-strict zod object strips it and the legacy `web` tree casts
    // past it. The endpoint is invalidated after every send and every
    // interrupt, and the server clones the texts three times on the way out
    // (`snapshot()`, `pending_entries()`, then `to_string()` per entry). The
    // bound is the page budget below, and a normal queue holds nought to two
    // short entries, so this is charged rather than fixed. If it ever needs
    // fixing the shape is an opt-in (`?include=pending`) — deliberately not
    // done here, because a query parameter that exactly one future consumer
    // will set is a wire change made on speculation.
    pub pending: Vec<PendingQueueEntry>,
    /// User-authored entries that exist in the queue but are NOT in `pending`:
    /// pre-PR1 entries with no id, plus anything past the page budget. The UI
    /// can say "N more not shown" and be honest about not offering buttons.
    pub pending_overflow: u32,
    /// #1505 S6 — whether this card can take image attachments at all.
    ///
    /// It cannot when the track's workspace is a directory the person already
    /// owns: attachments are written under `<workspace>/.neige/`, and neige
    /// never writes into an attached workspace, so the upload endpoint answers
    /// 400 there.
    ///
    /// Answered here rather than left for the client to work out, and answered
    /// before the attempt rather than by the attempt. Half the tracks in
    /// production were created with a `cwd` and are attached, so a paperclip
    /// that looks available and then refuses would be the common case rather
    /// than the edge. The criterion is the same function the upload runs
    /// (`planner_attachments::attachment_root`), called rather than restated.
    pub attachments_supported: bool,
}

/// #1505 PR1 — one addressable user entry from the harness pending queue.
#[derive(Debug, Serialize, ToSchema)]
pub struct PendingQueueEntry {
    /// Stable identity. Never empty, and unique within one response.
    // The reason is the read boundary, not this page: what reaches here is the
    // `QueueEntry::User` variant, and the only way to get one is a minted uuid
    // or a snapshot slot that `HarnessSnapshot::deserialize_pending_entry_meta`
    // accepted — and that decoder refuses an empty or duplicated id, demoting
    // the slot to `LegacyUser`. Without it a hand-edited `handle_state_json`
    // could put `entry_id: ""`, or the same id twice, on the wire.
    pub entry_id: String,
    /// The complete text. Never truncated — an entry that would not fit the
    /// page budget is left out of the page entirely rather than shown in a
    /// form the user cannot safely edit.
    pub text: String,
    /// CAS token for the edit/delete endpoints (#1505 PR2). Bumped whenever
    /// the text is rewritten, folding under backpressure included.
    pub rev: u32,
    /// Wall-clock ms at which the entry entered the queue.
    pub queued_at_ms: i64,
    /// #1505 S6 — the images this queued message carries.
    ///
    /// Each is already bound, so its read-back url resolves now and will keep
    /// resolving. The absolute host path the server holds beside each of these
    /// is deliberately not here: the client addresses an attachment by id and
    /// reads it back through `GET /planner/attachments/{id}`.
    pub attachments: Vec<PlannerAttachment>,
}

/// Hard cap on entries in one `pending` page.
const PENDING_PAGE_MAX: usize = 64;

/// Soft cap on the UTF-8 size of one `pending` page.
///
/// The whole response is re-fetched whenever the queue changes, and a single
/// `/planner/input` body may be `MAX_PLANNER_INPUT_CHARS` = 32_768
/// *characters*. UTF-8 encodes a `char` in at most 4 bytes, so one body is at
/// most `32_768 * 4` = 131_072 B = 128 KiB, and 64 of them would be
/// `64 * 128` KiB = 8 MiB. Entries are packed whole until the next one would
/// cross this line.
///
/// This is a judgement about acceptable response size, not a measurement of any
/// real queue.
const PENDING_PAGE_BYTES: usize = 1_536 * 1_024;

/// Split the queue into one page of addressable entries plus a count of the
/// user-authored entries that did not make it.
///
/// The budget ALWAYS admits at least one entry. Today that rule is unreachable:
/// the largest single entry the fold path can build is
/// `MAX_FOLDED_USER_MESSAGE_CHARS` = `4 * 32_768` = 131_072 characters, hence
/// at most `131_072 * 4` = 524_288 B = 512 KiB of UTF-8, against a budget of
/// 1.5 MiB. It is written rather than argued because the failure it prevents is
/// severe and silent: a head entry over budget would make the whole queue
/// unpageable, so the user could not even delete the thing that was blocking
/// it, and the "the user can just delete it" answer that justifies the budget
/// would be false.
fn page_pending_entries(card_id: &CardId, entries: &[QueueEntry]) -> (Vec<PendingQueueEntry>, u32) {
    let mut page = Vec::new();
    let mut used_bytes = 0usize;
    let mut overflow = 0u32;
    let mut budget_exhausted = false;
    for entry in entries {
        let Some(view) = entry.user_view() else {
            // Not addressable. A dispatcher observation is not the user's to
            // begin with; a pre-PR1 user entry has no id, so it is counted.
            if entry.is_user_authored() {
                overflow = overflow.saturating_add(1);
            }
            continue;
        };
        // The page is a PREFIX of the queue, not a greedy pack: once one entry
        // does not fit, every later one is overflow even if it would have.
        // Packing around a hole would show the user a list whose order and
        // adjacency lie, and "N more below" would no longer be where they are.
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

/// #1255 S3 — the context-usage half of [`GetPlannerRunResponse`].
///
/// A wire type distinct from the stored [`TokenUsage`], and the differences
/// are the point rather than an accident of layering:
///
/// - **`percent` is computed here, on the server.** One baseline adjustment,
///   one over-window rule, one place they can be got wrong. Shipping a
///   numerator and a denominator instead would invite the client to divide
///   them its own way, and the correct division is not the obvious one.
/// - **`total_tokens` is NOT shipped.** The stored value keeps it (it is the
///   honest lifetime cost), but `tokenUsage.total` is a cumulative sum across
///   every response in the thread — unbounded, and measured at 253.8x the
///   window in the captured frame this slice's tests run on — and the single
///   most likely bug in any future UI is a meter
///   drawn from it. Handing the frontend both numbers and trusting it to pick
///   the right one is how that bug gets written. It cannot pick wrong if only
///   one number crosses the wire.
#[derive(Debug, Serialize, ToSchema)]
pub struct PlannerRunTokenUsage {
    /// Tokens in the model's context as of the most recent response
    /// (`tokenUsage.last.totalTokens` upstream). Always present — this is the
    /// raw evidence, and it ships even when `percent` does not.
    pub used_tokens: i64,
    /// The model's context window, or null when codex has never reported one.
    pub context_window: Option<i64>,
    /// Context occupancy as a whole percentage, `0.0..=100.0`.
    ///
    /// Null means "no percentage can honestly be stated": no known window, a
    /// window at or below the 12000-token baseline, or `used_tokens` above
    /// the window. That last case is deliberately NOT clamped to 100 — see
    /// `TokenUsage::percent`. Render the raw count with no meter.
    pub percent: Option<f64>,
    /// Wall-clock ms of the codex frame this reading came from.
    ///
    /// Shipped because the reading survives a reboot: it rides the runtime
    /// snapshot, so a harness respawned by boot recovery or by lazy recovery
    /// serves whatever number was last observed — possibly months ago — and
    /// without this field a rehydrated reading is indistinguishable on the
    /// wire from a live one. A UI that draws a meter needs to be able to say
    /// "as of then", or to stop drawing it. The kernel does not pick a
    /// staleness threshold; it ships the timestamp so a reader can.
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

/// The one body check for planner input, shared by the send route and the
/// #1505 PR2 edit route.
///
/// An edit is a send by another name: the text it leaves in the queue is the
/// text a turn will carry, so admitting through `PATCH` something `POST`
/// refuses would make the limit a formality. Shared rather than restated —
/// a second copy of "not empty, at most N characters" is a copy that can
/// disagree.
pub(crate) fn validate_planner_input_text(text: &str) -> Result<usize> {
    validate_planner_input(text, false)
}

/// The same check, told whether the message carries an image.
///
/// #1505 S6. Pasting a screenshot and pressing enter is the single most common
/// thing this feature is for, so an empty text beside an attachment has to be
/// a message rather than a refusal. The length limit is unchanged and still
/// applies to whatever text there is: an attachment does not buy room, it
/// buys the right to send none.
///
/// The edit route keeps calling [`validate_planner_input_text`], i.e. keeps
/// requiring text. `PATCH` cannot change an entry's attachments in this slice,
/// so it has no way to tell "this message is its picture" from "this message
/// is now empty", and clearing the text of an image message is the second of
/// those.
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

fn planner_input_audit_actor(actor: &Actor, card_id: &CardId) -> ActorId {
    match actor.to_actor_id() {
        ActorId::AiCodex(c) if c.as_str().is_empty() => ActorId::AiCodex(card_id.clone()),
        // Middleware currently only admits `ai:codex`, but keep these
        // branches ready if REST actor validation later gains more AI kinds.
        ActorId::AiClaude(c) if c.as_str().is_empty() => ActorId::AiClaude(card_id.clone()),
        ActorId::AiPlanner(c) if c.as_str().is_empty() => ActorId::AiPlanner(card_id.clone()),
        other => other,
    }
}

#[utoipa::path(
    post,
    path = "/api/cards/{id}/planner/input",
    tag = "cards",
    params(("id" = String, Path, description = "Planner card id")),
    request_body = SendPlannerInputRequest,
    responses(
        (status = 200, description = "User text queued for next harness turn", body = SendPlannerInputResponse),
        (status = 400, description = "Empty text", body = ErrorBody),
        (status = 403, description = "Card is not a planner codex card", body = ErrorBody),
        (status = 404, description = "Card or track not found", body = ErrorBody),
        (status = 409, description = "Runtime is shutting down (code `conflict`); the planner harness session is dormant and not recoverable — reset to start a session (code `planner_harness_dormant`); or the runtime is no longer this card's and the text was NOT stored, so re-sending it reaches the successor (code `planner_harness_runtime_superseded`)", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
        (status = 503, description = "Observation queue saturated, shared codex app-server not running, or a planner-harness start is still in flight — retry shortly", body = ErrorBody),
    ),
)]
#[allow(deprecated)]
pub(crate) async fn send_planner_input(
    State(s): State<RouteState>,
    State(w): State<WorkerState>,
    State(cs): State<CodexShellState>,
    actor: Actor,
    Path(id): Path<String>,
    Json(body): Json<SendPlannerInputRequest>,
) -> Result<Json<SendPlannerInputResponse>> {
    let SendPlannerInputRequest { text, attachments } = body;
    let (s, w, cs, id) = (s, w, cs, id);
    let char_count = validate_planner_input(&text, !attachments.is_empty())?;

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

    // `_recovery_guard` (Some only on the lazy-recovery path) holds the
    // per-card recovery lock until end of handler scope, so a concurrent
    // `/planner/reset` can't supersede the just-recovered runtime between
    // recovery and the observe/audit below.
    let (runtime, harness, _recovery_guard) =
        ensure_live_planner_harness(&s, &w, &cs, &card.id).await?;
    let track = s
        .repo
        .track_get(card.track_id.as_str())
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("track {} for card {id}", card.track_id)))?;
    let scope = EventScope::Card {
        card: card.id.clone(),
        track: track.id.clone(),
        area: track.area_id.clone(),
    };
    // #1505 S6 — bind BEFORE the entry exists.
    //
    // The order is the whole of the durability argument: the bytes leave the
    // sweepable staging directory first, so an entry that reaches the queue
    // always names files nothing will reclaim. The reverse order would leave a
    // window in which a queued message points at a file the orphan sweep is
    // still entitled to remove, and codex answers an unreadable image with
    // placeholder text and no error.
    //
    // The failure direction this buys is a leak: a bind that succeeds and is
    // followed by a failed enqueue leaves bytes in `bound/` that no message
    // names. They cost the card's budget and nothing reclaims them (#1505
    // GAP-A12). A dangling reference would cost a silently degraded turn,
    // which is worse and is unobservable.
    //
    // `attachment_root` refuses an attached workspace, so a card on a track
    // pointed at a directory the user owns gets a 400 here — and only when it
    // actually names an attachment. A text-only message on such a track is
    // untouched by any of this.
    let attachments = if attachments.is_empty() {
        Vec::new()
    } else {
        let root =
            crate::planner_attachments::attachment_root(&track.workspace, &s.workspace_root)?;
        crate::planner_attachments::bind::bind_attachments(
            &root,
            &card.id,
            &attachments,
            &s.planner_attachment_locks,
        )
        .await?
    };
    // Migrate ONLY the AI-header path (empty placeholder card) to the live planner
    // session actor; the human web-UI path (`actor` == User) and any other actor
    // MUST stay unchanged so the audit log keeps distinguishing human input from
    // agent actions. Falls back to the existing card-shaped rebind when the
    // runtime is not in an active-authority state.
    let audit_actor = match actor.to_actor_id() {
        ActorId::AiCodex(c) | ActorId::AiClaude(c) | ActorId::AiPlanner(c)
            if c.as_str().is_empty() && runtime.status.is_active_authority() =>
        {
            ActorId::AiPlannerSession(WorkerSessionId::from(runtime.id.clone()))
        }
        _ => planner_input_audit_actor(&actor, &card.id),
    };

    let attachment_count = attachments.len();
    let ack = harness
        .observe_user_message_durable(text, attachments)
        .await?;

    tracing::info!(
        actor = %actor.as_str(),
        card_id = %card.id,
        runtime_id = %runtime.id,
        char_count,
        attachment_count,
        "planner harness user message enqueued"
    );

    if let Err(error) = s
        .repo
        .log_pure_event(
            audit_actor,
            scope,
            None,
            &s.events,
            s.write.role_cache(),
            s.write.area_cache(),
            Event::HarnessUserMessageEnqueued {
                worker_session_id: runtime.id.clone(),
                card_id: card.id.clone(),
                track_id: card.track_id.clone(),
                char_count: char_count as u32,
            },
        )
        .await
    {
        // The user message (and any kernel context paired with it) is already
        // durably accepted. Returning 500 here would invite a retry and execute
        // the same intent twice; keep the accepted response and surface the
        // audit failure operationally.
        tracing::error!(
            card_id = %card.id,
            runtime_id = %runtime.id,
            error = %error,
            "planner input was accepted but its audit event failed"
        );
    }

    Ok(Json(SendPlannerInputResponse {
        card_id: card.id,
        worker_session_id: runtime.id.clone(),
        entry_id: ack.entry_id.map(|id| id.as_str().to_string()),
    }))
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
    Json(body): Json<RatifyCardRequest>,
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
    let message = body.message.unwrap_or_default();

    write_with_actor_events_typed::<(), _>(s.repo.as_ref(), None, &s.events, &s.write, move |tx| {
        let actor_id = actor_id.clone();
        let scope = scope.clone();
        let track_id = track_id.clone();
        let message = message.clone();
        Box::pin(async move {
            if !ratify_request_pending_tx(tx, &track_id).await? {
                return Err(CalmError::Conflict(
                    "ratify: track is not awaiting ratification".into(),
                ));
            }

            let mut events = Vec::new();
            if decision == RatifyCardDecision::Grant
                && let Some(lifecycle_events) = apply_requested_transition_in_tx(
                    tx,
                    &track_id,
                    TrackLifecycle::Working,
                    &actor_id,
                    message,
                )
                .await?
            {
                events.extend(
                    lifecycle_events
                        .into_iter()
                        .map(|event| (actor_id.clone(), scope.clone(), event)),
                );
            }
            events.push((
                actor_id,
                scope,
                Event::RatifyResolved {
                    track_id,
                    decision: decision.into(),
                },
            ));
            Ok(((), events))
        })
    })
    .await?;

    Ok(Json(RatifyCardResponse {
        card_id,
        track_id: track.id,
        decision,
    }))
}

/// Issue #668 — stop the running planner turn.
///
/// Guard chain mirrors `/planner/input` (card → role → kind), but deliberately
/// WITHOUT the lazy-recovery path and its per-card lock: a harness that
/// needs recovering has, by construction, no running turn to stop, so a
/// registry miss (or no active runtime row) is the same typed 409
/// `planner_harness_dormant` the input route uses — the client steers the user
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
    // Dispatch for IssuingTurn too (best-effort), but only TurnRunning —
    // where an interrupt target is guaranteed — reports `stopped: true`.
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

/// Issue #668 fix — read the current planner-harness phase for a card.
///
/// Guard chain mirrors `/planner/interrupt` (card → role → kind), but unlike
/// the write routes a dormant harness is a normal answer for a read: no
/// active runtime row, or an active row with no registered harness, is
/// `200 {worker_session_id: null, phase: null}` rather than a 409.
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

    // A payload whose model keys are unreadable is reported as "no selection"
    // by this READ rather than as a 500. The turn-issuing path refuses on the
    // same payload (`planner_model`'s header), so the conversation still stops;
    // making the read fail as well would only take away the surface that has
    // to show why.
    let selection =
        crate::planner_model::CardModelSelection::from_payload(&card.payload).unwrap_or_default();
    // The same predicate the upload endpoint enforces, asked of the same
    // function, so the answer cannot drift from the refusal.
    let attachments_supported = match s.repo.track_get(card.track_id.as_str()).await? {
        Some(track) => {
            crate::planner_attachments::attachment_root(&track.workspace, &s.workspace_root).is_ok()
        }
        // No track means no workspace to write into. A missing track is
        // already fatal for everything else on this card, but this field is
        // not the place to raise it, and "supported" would be the wrong guess.
        None => false,
    };
    let dormant = GetPlannerRunResponse {
        card_id: card.id.clone(),
        worker_session_id: None,
        phase: None,
        model: selection.model.clone(),
        reasoning_effort: selection.reasoning_effort.clone(),
        // A dormant conversation has no harness to be blocked, and nothing is
        // waiting: there is no queue and no unsent sentence.
        blocked_reason: None,
        token_usage: None,
        pending: Vec::new(),
        pending_overflow: 0,
        attachments_supported,
    };
    let Some(runtime) = s
        .repo
        .session_projection_active_for_card(&card.id.to_string())
        .await?
    else {
        return Ok(Json(dormant));
    };
    let Some(harness) = s.harness.get(&runtime.id) else {
        return Ok(Json(dormant));
    };
    // One snapshot read for both fields (#1255 S3). Taking two would let the
    // phase and the usage come from different instants for no benefit —
    // `snapshot_for` acquires a fistful of mutexes, so it is also the cheaper
    // way round.
    let snapshot = harness.snapshot().await;
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
    }))
}

/// Issue #649 i2 — resolve a live [`PlannerHarness`] handle for a planner card.
///
/// Fast path: active runtime row + registry hit (untouched behavior).
///
/// Registry miss with an active runtime row (e.g. server restart on a
/// `done`-lifecycle track, where boot recovery deliberately skips the track)
/// → lazily re-spawn the harness in place via
/// [`crate::harness::spawn_recovered_harness`] — the exact function boot
/// recovery uses (snapshot load, catch-up event replay, run, registry
/// insert). Spawning does no Codex RPC, so recovery is cheap.
///
/// No active runtime row, or an active row that is unrecoverable
/// (no thread anywhere — neither `runtime.thread_id` nor the snapshot's
/// `last_thread_id` — from a half-failed start, or a corrupt snapshot)
/// → typed 409 [`CalmError::PlannerHarnessDormant`] so the client can steer
/// the user to `/planner/reset` instead of retrying.
///
/// Hardenings (design review):
/// 1. per-card async lock + re-fetch/re-probe under the lock, so racing
///    Sends can't double-spawn (the second spawn shuts the first down);
/// 2. snapshot pre-validated with [`is_harness_snapshot_value`] — the
///    strict deserializer panics on unknown shapes;
/// 3. a thread must exist (row `thread_id`, or the snapshot's
///    `last_thread_id` — the same fallback boot recovery applies), else a
///    recovered harness would queue messages forever;
/// 4. `/planner/reset` takes the SAME per-card lock (see
///    [`reset_planner_card_shared`]), and the recovery path RETURNS its guard
///    to the caller (`send_planner_input` holds it through enqueue/audit), so
///    a reset can't supersede the runtime between the in-lock refetch here
///    and harness registration — nor in the gap between recovery and the
///    caller's `observe` enqueue — eliminating the resurrect-stale-session
///    race;
/// 5. row-intrinsic dormancy (409) is checked before daemon liveness
///    (503), so an unrecoverable row tells the user to Reset rather than
///    to retry.
#[allow(deprecated)]
async fn ensure_live_planner_harness(
    s: &RouteState,
    w: &WorkerState,
    cs: &CodexShellState,
    card_id: &CardId,
) -> Result<(
    WorkerSessionProjection,
    crate::harness::PlannerHarness,
    Option<PerCardLockGuard>,
)> {
    let dormant = || {
        CalmError::PlannerHarnessDormant(format!(
            "no recoverable planner harness session for card {card_id}; reset to start a session",
        ))
    };
    let runtime = s
        .repo
        .session_projection_active_for_card(&card_id.to_string())
        .await?
        .ok_or_else(dormant)?;
    if let Some(harness) = s.harness.get(&runtime.id) {
        return Ok((runtime, harness, None));
    }

    let guard = lock_card(&s.planner_recovery_locks, card_id.as_str()).await;
    // Re-fetch under the lock and use only this row: `/planner/reset` may have
    // superseded the pre-lock runtime, and a racing Send may have already
    // recovered the harness.
    let runtime = s
        .repo
        .session_projection_active_for_card(&card_id.to_string())
        .await?
        .ok_or_else(dormant)?;
    if let Some(harness) = s.harness.get(&runtime.id) {
        return Ok((runtime, harness, Some(guard)));
    }
    // #649 review round 3 — a `starting` row means `planner-harness-start` is
    // still in flight: the adapter writes the row (and, in the deferred
    // path, the thread id + snapshot) BEFORE `spawn_side_effect` registers
    // the harness. Recovering here would spawn a harness the start op then
    // shuts down and replaces, silently dropping any input queued on it.
    // 503 so the client retries once the start lands (a failed start is
    // compensated to `failed`/deleted, after which this 409s as dormant).
    // Recovery below is only for statuses that imply a previously-live
    // harness (running / idle / turn_pending).
    if runtime.status == WorkerSessionState::Starting {
        return Err(CalmError::ServiceUnavailable(
            "planner harness is starting; retry shortly".into(),
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
        // #953 — same variant/status (503); message-only enrichment.
        return Err(CalmError::ServiceUnavailable(
            cs.shared_codex_appserver.not_running_message(),
        ));
    }
    let runtime_id = runtime.id.clone();
    let harness = crate::harness::spawn_recovered_harness(
        w.repo.clone(),
        s.events.clone(),
        s.write.role_cache().clone(),
        s.write.area_cache().clone(),
        cs.shared_codex_appserver.clone(),
        &s.harness,
        &s.track_delete_locks,
        runtime.clone(),
        crate::harness::ClaimMode::Replace,
    )
    .await?
    .installed()
    .ok_or_else(dormant)?;
    tracing::info!(
        card_id = %card_id,
        runtime_id = %runtime_id,
        "planner harness lazily recovered on /planner/input registry miss"
    );
    // #649 review round 4 — return the guard so the caller keeps the
    // per-card lock alive through `harness.observe` and the audit event;
    // dropping it here would let a concurrent `/planner/reset` supersede the
    // recovered runtime before the message is enqueued.
    Ok((runtime, harness, Some(guard)))
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
    // Recovery deliberately declines malformed persisted Planner runtimes so a
    // boot pass can continue. This user-facing reset boundary preserves the
    // established HTTP 403 contract instead of turning that decline into a
    // generic operation failure.
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
    // #649 review round 1 — reset takes the SAME per-card lock as the
    // `/planner/input` lazy-recovery path (`ensure_live_planner_harness`).
    // Without it, a reset racing a registry-miss Send could supersede the
    // runtime after recovery's in-lock refetch but before harness
    // registration, resurrecting the reset-away session (and routing the
    // just-sent message to the dead thread). Holding the lock across the
    // start+shutdown operations is deadlock-free: both adapters either
    // take no locks (shutdown) or use their own private map
    // (`per_card_mint_locks` in the start adapter) — neither can re-enter
    // `planner_recovery_locks`.
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

    // #1098 §5.3 / #1189: a marked conversation card restarts under its OWN
    // profile. Restarting an assistant under `Planner` would re-mint its thread
    // with the planner prompt and the planner role — the card row would still say
    // `assistant`, so the thread and the card would disagree about what the
    // session may do.
    //
    // #1211 S1: no profile inherits the track title as a goal on this
    // user-driven reset path. A seeded `Observation::TrackGoal` makes the agent
    // speak before the user does, and this path no longer treats the title as
    // intent — whether the title is currently blank or not, and whoever wrote
    // it. (Child tracks are the one remaining place where a title IS intent:
    // `operation/child_track_adapter.rs` copies the parent planner's declared task
    // goal into it. That is machine-written and stays; it just is not read
    // here.)
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
        cwd: track.workspace.path.clone(),
        goal: None,
        reset_harness_items: true,
        force_new_thread: true,
        profile,
        create_card: None,
        first_message: None,
        create_request_sha256: None,
        // #1343 — not a conversation create; nothing to brief. `None` is
        // skipped by serde, so this payload's bytes are unchanged.
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

/// Submit one planner-card operation and wait for it, mapping its outcome onto a
/// `CalmError`.
///
/// `pub(crate)` since #1253: `routes::today_summary`'s dormant recovery
/// re-submits `planner-harness-start` through it. Calling this rather than
/// re-implementing the submit/wait/map is the point — an operation that mapped
/// its failure classes differently would answer 500 where this answers 400 or
/// 503, and the divergence would only show up under failure.
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
    // Look up first so we have the track_id for the delete event.
    let card = s
        .repo
        .card_get(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("card {id}")))?;
    // Issue #229 PR A — kernel-owned card guard. Planner cards (and PR B's
    // report cards) carry `deletable = false`; refuse direct REST delete.
    // Track delete via `DELETE /api/tracks/:id` still cascades through the
    // FK chain — the guard fires only on this `/api/cards/:id` path.
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
    // same shape as track-delete cascading through cards). If cleanup
    // fails *before* the txn opens we surface 500; the row stays and
    // the sweeper retries on the next tick, so we don't end up with
    // a half-torn-down terminal. Planner cards (CardRole::Planner) take the
    // same path: terminals share one table with no role-specific cleanup
    // divergence.
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
                let mut events = release_workspace_lease_for_card_tx(tx, card_id.as_ref()).await?;
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

    /// Any card. These cases are about which entries reach the page, not about
    /// which card they belong to; the id only reaches the read-back urls.
    fn test_card_id() -> CardId {
        CardId::from("card-paging")
    }

    /// A legacy entry built the ONLY way production can produce one: by
    /// deserializing a row whose `pending_entry_meta` slot is absent.
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

    /// The budget always admits the head entry, however large.
    ///
    /// Unreachable today — the largest entry the fold path can build is
    /// `4 * 32_768` chars, comfortably under the budget — and written anyway,
    /// because the failure it prevents is that an over-budget head entry makes
    /// the whole queue unaddressable: the user could not delete the very thing
    /// blocking the page, which is the answer this budget's design rests on.
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
