//! `/internal/codex/hook` — receive codex CLI hook events from the bridge
//! subprocess and re-emit them on the WS event bus.
//!
//! ## Why a loopback ingest
//!
//! Codex CLI invokes a configured "bridge" command on every lifecycle hook
//! (SessionStart / PreToolUse / PostToolUse / Stop / …) via the
//! `hooks.json` we seed into its `CODEX_HOME`. The bridge — `neige-codex-bridge`
//! — POSTs the raw hook payload here; we extract `hook_event_name`, tag it
//! `hook.codex.<snake_case_name>`, and emit `Event::CodexHook` on the bus.
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
//! `copy_dir_recursive`, `build_hooks_json`, `default_cwd`) moved along with
//! the endpoint. This file keeps only the loopback ingest.

use crate::actor::Actor;
use crate::error::Result;
use crate::event::Event;
use crate::state::AppState;
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
        .route("/internal/codex/hook", post(ingest_hook))
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
    actor: Actor,
    Query(q): Query<IngestQuery>,
    Json(payload): Json<Value>,
) -> Result<StatusCode> {
    let event_name = payload
        .get("hook_event_name")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let kind = format!("hook.codex.{}", to_snake_case(event_name));

    s.repo
        .log_pure_event(
            actor.as_str(),
            None,
            &s.events,
            Event::CodexHook {
                card_id: q.card_id,
                kind,
                payload,
            },
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
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
