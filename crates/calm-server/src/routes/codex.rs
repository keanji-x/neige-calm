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
use crate::error::Result;
use crate::event::{Event, EventScope};
use crate::ids::{ActorId, CardId};
use crate::state::{AppState, RouteState};
use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    routing::post,
};
use serde::Deserialize;
use serde_json::Value;

pub fn router() -> Router<AppState> {
    Router::new()
        // Loopback-only ingest. The bridge subprocess is spawned by codex
        // itself with env vars pointing here. Not exposed under `/api/*`
        // because the frontend never calls it directly.
        //
        // #293 cutover removed `/internal/codex/pending_events` — the old
        // Stop-hook long-poll fallback. Spec agents are now driven by pushed
        // turn inputs, so there's no pull endpoint to back.
        .route("/internal/codex/hook", post(ingest_hook))
}

#[derive(Debug, Deserialize)]
pub struct IngestQuery {
    pub card_id: String,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum HookProvider {
    Codex,
    Claude,
}

impl HookProvider {
    fn kind_prefix(self) -> &'static str {
        match self {
            Self::Codex => "hook.codex",
            Self::Claude => "hook.claude",
        }
    }

    fn actor(self, card_id: CardId) -> ActorId {
        match self {
            Self::Codex => ActorId::AiCodex(card_id),
            Self::Claude => ActorId::AiClaude(card_id),
        }
    }

    fn event(self, card_id: CardId, kind: String, payload: Value) -> Event {
        match self {
            Self::Codex => Event::CodexHook {
                card_id,
                kind,
                payload,
            },
            Self::Claude => Event::ClaudeHook {
                card_id,
                kind,
                payload,
            },
        }
    }
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
    State(s): State<RouteState>,
    _actor: Actor,
    Query(q): Query<IngestQuery>,
    Json(payload): Json<Value>,
) -> Result<StatusCode> {
    ingest_provider_hook(&s, q.card_id, payload, HookProvider::Codex).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[allow(deprecated)]
pub(crate) async fn ingest_provider_hook(
    s: &RouteState,
    card_id_str: String,
    payload: Value,
    provider: HookProvider,
) -> Result<()> {
    let event_name = payload
        .get("hook_event_name")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let kind = format!("{}.{}", provider.kind_prefix(), to_snake_case(event_name));

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
            provider.actor(card_id_typed.clone()),
            scope,
            None,
            &s.events,
            s.write.role_cache(),
            s.write.cove_cache(),
            provider.event(card_id_typed, kind, payload),
        )
        .await?;
    Ok(())
}

/// Convert codex's `PascalCase` event names (`PreToolUse`) to snake.
/// Keeps the same shape as Claude hook discriminators on the wire, so
/// the frontend's pattern matching stays consistent across providers.
pub(crate) fn to_snake_case(s: &str) -> String {
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
