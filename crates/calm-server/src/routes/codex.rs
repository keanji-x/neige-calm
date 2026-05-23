//! `/internal/codex/hook` — receive codex CLI hook events from the bridge
//! subprocess and re-emit them on the WS event bus.
//!
//! ## Why a loopback ingest
//!
//! Codex CLI invokes a configured "bridge" command on every lifecycle hook
//! (SessionStart / PreToolUse / PostToolUse / Stop / …) via the policy-
//! managed hook entries in `/etc/codex/requirements.toml` (bind-mounted
//! via docker-compose; see `docker/codex-requirements.toml`). The bridge
//! — `neige-codex-bridge` — POSTs the raw hook payload here; we extract
//! `hook_event_name`, tag it `hook.codex.<snake_case_name>`, and emit
//! `Event::CodexHook` on the bus.
//!
//! The handler is mounted under `/internal/*` rather than `/api/*` because
//! the frontend never calls it directly — it's an internal contract between
//! the codex CLI (via the bridge) and the kernel. The codex daemon is spawned
//! with `NEIGE_CALM_BASE_URL` pointing at the server loopback, so the bridge
//! resolves the URL from env at hook time.
//!
//! ## Card creation moved to `routes/codex_cards.rs`
//!
//! The old `POST /api/cards/:id/codex` endpoint that bound an existing card
//! to a live codex PTY is gone (#117). The atomic
//! `POST /api/waves/:wave_id/codex-cards` replaces it — see
//! `routes::codex_cards`. The card-creation helpers (`host_codex_dir`,
//! `copy_dir_recursive`, `default_cwd`) moved along with the endpoint.
//! This file keeps only the loopback ingest.

use crate::actor::Actor;
use crate::error::{CalmError, Result};
use crate::event::{Event, EventScope};
use crate::ids::{ActorId, CardId};
use crate::mcp_server::tools::wait::{
    DEFAULT_TIMEOUT_MS, MAX_TIMEOUT_MS, render_response, wait_for_events_for_card,
};
use crate::state::AppState;
use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::Value;

pub fn router() -> Router<AppState> {
    Router::new()
        // Loopback-only ingest. The bridge subprocess is spawned by codex
        // itself with env vars pointing here. Not exposed under `/api/*`
        // because the frontend never calls it directly.
        .route("/internal/codex/hook", post(ingest_hook))
        // PR8 (#136) — long-poll fallback consumed by the codex bridge's
        // Stop hook handler. The bridge is short-lived (per hook process)
        // and has no JSON-RPC client of its own, so an HTTP endpoint with
        // the same semantics as `calm.wait_for_events` lets the Stop hook
        // surface pending events as a codex `{decision:"block", reason:...}`
        // observation without going through MCP.
        //
        // GET (not POST) because semantically the call is "fetch events
        // since X"; the long-poll wait is a server-side decision, not a
        // mutation. Plays nicer with existing telemetry / cache stacks
        // that distinguish read endpoints.
        .route("/internal/codex/pending_events", get(pending_events))
}

#[derive(Debug, Deserialize)]
pub struct IngestQuery {
    pub card_id: String,
}

/// Loopback-only ingest. The bridge subprocess POSTs the raw codex hook
/// payload here; we extract `hook_event_name`, tag it, and emit on the
/// bus.
///
/// Scope A — codex hook events flow through the sync engine's pure-event
/// log (`Repo::log_pure_event`) so the wire envelope carries an `_id`
/// the same way entity-write events do. The events row records every
/// hook payload verbatim; that's intentional — codex card UIs are
/// append-only ephemeral on the frontend, but the persistent event log
/// is the audit/replay store the design doc §2.3 calls out.
///
/// Scope β — the actor is now declarative: the codex bridge stamps
/// `X-Calm-Actor: ai:codex` on every POST and the `actor_middleware`
/// validates + injects an `Actor`. Pre-β this handler hardcoded `"kernel"`,
/// which was wrong on two counts: codex's lifecycle signal is an *AI*
/// write, not a server-internal one, and the audit log conflated the two.
///
/// Default-actor decision: we deliberately keep the middleware's `"user"`
/// fallback for this route. An older bridge with no header is the only
/// way to hit it, and tagging those hooks as `"user"` is honest — we
/// don't actually know it was codex. The fix is to redeploy the bridge,
/// not to silently re-attribute. (Overriding the default here would also
/// require the middleware to admit `kernel`/`ai:codex` from this path,
/// which conflicts with its "reserved namespace" gate.)
pub(crate) async fn ingest_hook(
    State(s): State<AppState>,
    _actor: Actor,
    Query(q): Query<IngestQuery>,
    Json(payload): Json<Value>,
) -> Result<StatusCode> {
    let event_name = payload
        .get("hook_event_name")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let kind = format!("hook.codex.{}", to_snake_case(event_name));

    // PR3 (#136) — reattribute the hook to the codex card that produced
    // it. PR2's stopgap stamped `ActorId::Kernel` because there was no
    // typed card id at the ingest boundary; PR3 now resolves the card
    // through the `card_id` query parameter and stamps
    // `ActorId::AiCodex(CardId)`. The role gate's empty-CardId guard
    // catches the case where `card_id` is empty / unresolvable, and
    // the unknown-card guard catches a card that was deleted between
    // hook fire and ingest.
    //
    // Scope: same as before — try to resolve `card → wave → cove`;
    // fall back to `EventScope::System` when the card has been
    // deleted. The gate's unknown-card branch then refuses the write,
    // which is what we want: a hook for a deleted card is an audit
    // smell.
    let card_id_str = q.card_id.clone();
    let card_id_typed = CardId::from(card_id_str.clone());
    let scope = match s.repo.card_get(&card_id_str).await? {
        Some(c) => match s.repo.wave_get(c.wave_id.as_str()).await? {
            Some(w) => EventScope::Card {
                card: c.id,
                wave: w.id,
                cove: w.cove_id,
            },
            None => EventScope::System,
        },
        None => EventScope::System,
    };

    s.repo
        .log_pure_event(
            ActorId::AiCodex(card_id_typed.clone()),
            scope,
            None,
            &s.events,
            &s.card_role_cache,
            Event::CodexHook {
                card_id: card_id_typed,
                kind,
                payload,
            },
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// PR8 (#136) — `/internal/codex/pending_events` long-poll fallback.
// ---------------------------------------------------------------------------

/// Query string for `/internal/codex/pending_events`.
///
/// `card_id` is required — the bridge knows it from the `NEIGE_CARD_ID`
/// env that the per-card codex daemon was spawned with. `timeout_ms`
/// is clamped at 30s server-side (same ceiling as the MCP tool);
/// omitting it defaults to the ceiling. `since` is optional — omit it
/// to use the per-card cursor cache the kernel maintains.
#[derive(Debug, Deserialize)]
pub struct PendingEventsQuery {
    pub card_id: String,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub since: Option<i64>,
}

/// GET `/internal/codex/pending_events?card_id=<id>&timeout_ms=<ms>&since=<id>`
///
/// Returns `{events: [...], since: <max_id>}` — identical wire shape to
/// `calm.wait_for_events`'s `structuredContent`. The bridge's Stop hook
/// reads `events` and, when non-empty, prints
/// `{"decision":"block", "reason":<JSON of events>}` to stdout so codex
/// re-prompts the agent with the new observations as a turn input.
///
/// Errors:
///   * 400 Bad Request — missing / empty `card_id`, or negative `since`
///   * 404 Not Found — `card_id` doesn't resolve to a known card
///   * 500 Internal — DB / bus error
pub(crate) async fn pending_events(
    State(s): State<AppState>,
    Query(q): Query<PendingEventsQuery>,
) -> Result<Json<Value>> {
    let card_id_str = q.card_id.trim().to_string();
    if card_id_str.is_empty() {
        return Err(CalmError::BadRequest(
            "pending_events: `card_id` query param required".into(),
        ));
    }
    if let Some(s) = q.since
        && s < 0
    {
        return Err(CalmError::BadRequest(
            "pending_events: `since` must be non-negative".into(),
        ));
    }

    // Resolve the card to confirm it exists and to pick up its wave for
    // the long-poll's scope filter. 404 on miss (vs 500) gives the bridge
    // a clear signal "this card_id is stale; codex daemon spec/worker
    // pairing has drifted" so the operator can rebuild the per-card
    // CODEX_HOME (hooks come from the managed requirements.toml).
    let card_id_typed = CardId::from(card_id_str);
    let card = s
        .repo
        .card_get(card_id_typed.as_str())
        .await?
        .ok_or_else(|| {
            CalmError::NotFound(format!("pending_events: card {}", card_id_typed.as_str()))
        })?;
    let wave_id = card.wave_id;

    let timeout_ms = q
        .timeout_ms
        .unwrap_or(DEFAULT_TIMEOUT_MS)
        .min(MAX_TIMEOUT_MS);

    let (envelopes, max_id) = wait_for_events_for_card(
        s.repo.as_ref(),
        &s.events,
        &s.event_cursor_cache,
        &card_id_typed,
        &wave_id,
        q.since,
        timeout_ms,
    )
    .await
    .map_err(|e| CalmError::Internal(format!("pending_events: {e}")))?;

    Ok(Json(render_response(envelopes, max_id)))
}

/// Convert codex's `PascalCase` event names (`PreToolUse`) to snake.
/// Keeps the same shape as Claude hook discriminators on the wire, so
/// the frontend's pattern matching stays consistent across providers.
fn to_snake_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for (i, c) in s.chars().enumerate() {
        if c.is_uppercase() {
            if i > 0 {
                out.push('_');
            }
            for lc in c.to_lowercase() {
                out.push(lc);
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snake_case_examples() {
        assert_eq!(to_snake_case("PreToolUse"), "pre_tool_use");
        assert_eq!(to_snake_case("Stop"), "stop");
        assert_eq!(to_snake_case("SessionStart"), "session_start");
        assert_eq!(to_snake_case("unknown"), "unknown");
    }
}
