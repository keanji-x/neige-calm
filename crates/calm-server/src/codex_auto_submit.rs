//! Auto-submit codex agents whose card was spawned with
//! `auto_submit: true` on the kernel-side request body.
//!
//! Part of the codex hands-free spawn primitive. The matching pieces
//! live in:
//!
//!   * `routes::codex::NewCodexBody { prompt, auto_submit }` — the
//!     request fields that drive this whole flow. `prompt` becomes
//!     codex's positional `[PROMPT]` arg (composer pre-fill); the
//!     `auto_submit` bit is stamped onto `card.payload.auto_submit`
//!     so this subscriber can decide whether to nudge.
//!   * `docker/codex-requirements.toml` — policy-managed hooks so the
//!     `session_start` event we key on actually fires without a
//!     `/hooks` review modal blocking it.
//!   * `state::DaemonClient::inject_stdin` — the privileged write path
//!     that carries our `\r` to the daemon over its kernel-private Unix
//!     socket with `ClientCapabilities::kernel_originated_input = true`.
//!
//! ## What this module does
//!
//! Subscribes to the event bus and, when a `hook.codex.session_start`
//! event fires for a card whose `payload.auto_submit == true`, injects a
//! single `\r` byte over the per-terminal daemon socket ~600 ms later.
//! Codex's composer sees the carriage return as a submit, the agent
//! loop kicks off, and the user never had to touch the keyboard.
//!
//! ## Why a separate module from `card_fsm`
//!
//! `card_fsm` is a *projector* — every event is a read-only signal it
//! folds into overlay state. This module performs a *side effect* (a
//! framed write to a Unix socket) and is gated on a payload predicate
//! orthogonal to the FSM. Keeping the two separate means a future
//! contributor can reason about either in isolation, and a bug in one
//! can't desync the other. Same `spawn(repo, daemon, bus)` shape as
//! `card_fsm::spawn` and `terminal_sweeper::spawn`, called from
//! `AppState::new` alongside them.
//!
//! ## Why ~600 ms
//!
//! The `session_start` hook fires *during* codex's TUI init. Empirically
//! the input handler is wired up earlier than the composer finishes its
//! first render, so even ~200 ms would likely work — 600 ms is a
//! comfortable cushion that won't be visible to a human user and
//! comfortably exceeds any plausible wire/init jitter. A future
//! refinement could replace the sleep with a wait on `DaemonMsg::
//! ChildReady` (added by PR #70), but the current sleep is simple, the
//! ChildReady plumbing has its own per-terminal-attach machinery to
//! navigate, and the lower bound the sleep gives us is already far below
//! a human reaction time. TODO if we ever measure pain here.
//!
//! ## Failure mode
//!
//! Any error along the path (card vanished, missing `terminal_id`,
//! socket refuses connection, daemon's reader half closed) is logged at
//! `warn` and the subscriber loop continues. The worst-case user impact
//! is "you need to press Enter yourself once" — degraded, not broken.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

use crate::db::RepoRead;
use crate::event::{Event, EventBus};
use crate::state::DaemonClient;

/// Delay between the `session_start` hook firing and our `\r` injection.
/// See module docs — 600 ms is comfortably past TUI init while staying
/// invisible to a watching human.
const SUBMIT_DELAY: Duration = Duration::from_millis(600);

/// The single keystroke codex's composer recognises as "submit this
/// prompt". Pulling it into a `const` makes the protocol-level contract
/// explicit at the call site.
const SUBMIT_BYTES: &[u8] = b"\r";

/// Spawn the auto-submit subscriber. Takes `RepoRead` because we only
/// need `card_get` from the repo (the daemon-side write goes through
/// `DaemonClient`, not the repo).
pub fn spawn(repo: Arc<dyn RepoRead>, daemon: Arc<DaemonClient>, bus: EventBus) {
    let mut rx = bus.subscribe();
    // Per-card dedup: codex's `session_start` hook can fire more than
    // once for the same agent (e.g. on TUI reconnect or session resume).
    // We only want to inject `\r` the first time — a second submit lands
    // on a non-empty composer or active turn and would submit an empty
    // or stray message. Insert-on-success: if injection itself fails,
    // we leave the card un-marked so a subsequent hook can retry.
    let submitted: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(env) => handle(env.event, repo.clone(), daemon.clone(), submitted.clone()).await,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(skipped = n, "codex_auto_submit subscriber lagged");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

/// One event handled. Only `hook.codex.session_start` triggers a
/// best-effort injection; everything else is a no-op.
async fn handle(
    ev: Event,
    repo: Arc<dyn RepoRead>,
    daemon: Arc<DaemonClient>,
    submitted: Arc<Mutex<HashSet<String>>>,
) {
    let Event::CodexHook { card_id, kind, .. } = ev else {
        return;
    };
    if kind != "hook.codex.session_start" {
        return;
    }
    // Dedup early — repeated session_start for the same card means a
    // re-init (replay, reconnect), not a new agent to nudge.
    if submitted.lock().await.contains(&card_id) {
        tracing::debug!(
            card_id = %card_id,
            "codex_auto_submit: session_start re-fired, skipping (already submitted)"
        );
        return;
    }

    // Resolve the card so we can read its payload and decide whether
    // this spawn opted in to auto-submit.
    let card = match repo.card_get(&card_id).await {
        Ok(Some(c)) => c,
        Ok(None) => {
            tracing::debug!(
                card_id = %card_id,
                "codex_auto_submit: card vanished before session_start could be handled"
            );
            return;
        }
        Err(e) => {
            tracing::warn!(card_id = %card_id, error = %e, "codex_auto_submit: card_get failed");
            return;
        }
    };

    if !should_auto_submit(&card.payload) {
        tracing::debug!(
            card_id = %card_id,
            "codex_auto_submit: payload.auto_submit not set, skipping (user-initiated spawn)"
        );
        return;
    }

    // Resolve the terminal id from the codex card's payload. The spawn
    // path always stamps this before returning, but there's a wire-level
    // window between `session_start` firing and `terminal_id` landing
    // on the card. If we lose that race we just give up; the user can
    // still press Enter manually.
    let Some(terminal_id) = card
        .payload
        .get("terminal_id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
    else {
        tracing::warn!(
            card_id = %card_id,
            "codex_auto_submit: card has no terminal_id yet, skipping (cannot resolve sock_path)"
        );
        return;
    };

    let sock_path = daemon.sock_path(&terminal_id);

    // Hand off to a detached task so we don't block the bus reader for
    // the full submit delay. Each detached task is cheap (one sleep, one
    // open/close on a Unix socket) and we never need to join them.
    tokio::spawn(async move {
        tokio::time::sleep(SUBMIT_DELAY).await;
        match daemon
            .inject_stdin(&sock_path, &terminal_id, SUBMIT_BYTES)
            .await
        {
            Ok(()) => {
                // Mark after-success so a transient socket error leaves
                // a retry path for the next session_start fire.
                submitted.lock().await.insert(card_id.clone());
                tracing::info!(
                    card_id = %card_id,
                    terminal_id = %terminal_id,
                    "auto-submitted hands-free codex prompt"
                );
            }
            Err(e) => {
                tracing::warn!(
                    card_id = %card_id,
                    terminal_id = %terminal_id,
                    error = %e,
                    "codex_auto_submit: inject_stdin failed"
                );
            }
        }
    });
}

/// Decide whether a codex card's payload opted in to hands-free
/// auto-submit. The discriminator is the boolean `auto_submit` flag
/// stamped at spawn time by `routes::codex` when the request body set
/// `auto_submit: true`. Anything else — missing key, `false`, wrong
/// type, non-object payload — is "no". This keeps the call generic:
/// any caller (HTTP, future scheduler, anything else) gets hands-free
/// behavior by setting one bit; nothing about *who* or *why* the spawn
/// happened leaks into the subscriber.
fn should_auto_submit(payload: &serde_json::Value) -> bool {
    payload
        .get("auto_submit")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn should_auto_submit_requires_explicit_true() {
        assert!(should_auto_submit(&json!({ "auto_submit": true })));
        // The presence of other fields doesn't change the verdict.
        assert!(should_auto_submit(&json!({
            "auto_submit": true,
            "terminal_id": "term_42",
            "cwd": "/home/x",
        })));
    }

    #[test]
    fn should_auto_submit_rejects_user_initiated_spawns() {
        // Default user-initiated spawn — no `auto_submit` key at all.
        assert!(!should_auto_submit(&json!({ "terminal_id": "term_42" })));
        assert!(!should_auto_submit(&json!({})));
        // Explicit `false` is just as much a no.
        assert!(!should_auto_submit(&json!({ "auto_submit": false })));
    }

    #[test]
    fn should_auto_submit_rejects_wrong_types() {
        // Defense in depth — `auto_submit` should be a bool, but a stray
        // string / number / null in the payload must not be coerced
        // into a true. (`as_bool` returns `None` for those, then
        // `unwrap_or(false)`.)
        assert!(!should_auto_submit(&json!({ "auto_submit": "true" })));
        assert!(!should_auto_submit(&json!({ "auto_submit": 1 })));
        assert!(!should_auto_submit(&json!({ "auto_submit": null })));
        // Non-object payloads are tolerated and treated as "no".
        assert!(!should_auto_submit(&serde_json::Value::Null));
        assert!(!should_auto_submit(&json!("a string somehow")));
    }
}
