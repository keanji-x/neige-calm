//! `codex_auto_submit` — kernel-side subscriber that submits the
//! composer-pre-filled prompt for a freshly-spawned codex card.
//!
//! ## What it does
//!
//! When `routes::codex_cards` spawns a codex daemon with a non-empty
//! `prompt`, the codex CLI mounts its TUI with the composer text pre-set
//! to that prompt (the prompt is wired through codex's positional
//! `[PROMPT]` arg). The subscriber here watches the event bus for the
//! `hook.codex.session_start` envelope — which the codex bridge POSTs
//! once codex's session is fully constructed — looks up the owning card,
//! and if its `payload.prompt` is present and non-empty, opens a
//! kernel-private unix-socket connection to the daemon and injects a
//! single `\r` via [`DaemonClient::inject_stdin`]. That triggers codex's
//! "send composer contents" path, replacing the previous "user must hit
//! Enter to start" friction.
//!
//! ## Why session_start, not card.added
//!
//! `card.added` fires inside the create handler, before the daemon has
//! even spawned. `hook.codex.session_start` fires after codex has bound
//! its session, opened its socket, and reached a state where stdin is
//! drained into the composer. Earlier signals (e.g. just-the-PTY-spawn)
//! aren't enough — the composer can still be priming.
//!
//! Even though `inject_stdin` *itself* waits on `ChildReady` /
//! `is_child_ready`, we still gate on `session_start` because that's
//! when codex has actually populated the composer with the prompt arg;
//! a `\r` before that would land on an empty composer.
//!
//! ## Dedup
//!
//! Codex can re-fire `session_start` on TUI reconnect (e.g. user closes
//! the browser tab then re-opens it). We keep a per-card `HashSet` of
//! already-submitted card ids so a re-fire is a no-op. The set lives
//! for the lifetime of the kernel — bounded by the number of codex
//! cards ever created in this process, which is OK at the deployment
//! scale we target (single-user dev workstation, low hundreds at most).
//!
//! ## Failure handling
//!
//! Everything degrades to a `warn!` log on failure — a missing card, a
//! payload without `prompt`, a daemon socket that refuses the
//! connection, a timeout on the InputAck round-trip — none of these
//! should crash the subscriber or take the rest of the kernel down.
//! The user sees the codex TUI with the composer pre-filled and can
//! hit Enter manually.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;

use crate::db::RepoRead;
use crate::event::{Event, EventBus};
use crate::state::DaemonClient;
use crate::terminal_renderer::TerminalRendererRegistry;

/// Backstop budget for the whole inject_stdin round-trip (connect → hello →
/// child-ready → input → ack). PR #110 used a 5s budget for the post-write
/// grace sleep alone; we keep the same overall bound on the deterministic
/// protocol-await path so a stuck PTY can't wedge the subscriber. The
/// timeout is not the success signal; `DaemonClient::inject_stdin` still
/// requires the protocol's `ServerHello` / `ChildReady` / `InputAck` stages.
const INJECT_STDIN_TIMEOUT: Duration = Duration::from_secs(5);

/// Spawn the auto-submit subscriber. Reads the event bus on the same
/// `tokio::Runtime` as the rest of the kernel; per-event work is
/// fire-and-forget tokio tasks so a slow daemon socket can't backpressure
/// the bus reader.
pub fn spawn(repo: Arc<dyn RepoRead>, daemon: Arc<DaemonClient>, bus: EventBus) {
    spawn_with_terminal_renderer(repo, daemon, TerminalRendererRegistry::new(), bus);
}

pub fn spawn_with_terminal_renderer(
    repo: Arc<dyn RepoRead>,
    daemon: Arc<DaemonClient>,
    terminal_renderer: Arc<TerminalRendererRegistry>,
    bus: EventBus,
) {
    let mut rx = bus.subscribe();
    let inner = Arc::new(Inner {
        repo,
        daemon,
        terminal_renderer,
        submitted: Mutex::new(HashSet::new()),
    });
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(env) => {
                    if let Event::CodexHook {
                        ref card_id,
                        ref kind,
                        ..
                    } = env.event
                        && kind == "hook.codex.session_start"
                    {
                        // Fire-and-forget — the bus reader keeps draining
                        // while the inject round-trip is in flight.
                        let inner = inner.clone();
                        let card_id = card_id.clone();
                        tokio::spawn(async move {
                            inner.maybe_submit(card_id.as_ref()).await;
                        });
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(skipped = n, "codex_auto_submit subscriber lagged");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

struct Inner {
    repo: Arc<dyn RepoRead>,
    daemon: Arc<DaemonClient>,
    terminal_renderer: Arc<TerminalRendererRegistry>,
    /// Card ids we've already submitted on. Guards against codex's
    /// session_start re-firing on TUI reconnect.
    submitted: Mutex<HashSet<String>>,
}

impl Inner {
    async fn maybe_submit(self: Arc<Self>, card_id: &str) {
        // Per-card dedup — once we've shipped `\r` for a card, never again.
        {
            let mut g = self.submitted.lock().await;
            if !g.insert(card_id.to_string()) {
                return;
            }
        }

        // Look up the card; bail quietly if it vanished or isn't codex.
        let card = match self.repo.card_get(card_id).await {
            Ok(Some(c)) => c,
            Ok(None) => {
                tracing::debug!(card_id, "auto_submit: card not found, skipping");
                return;
            }
            Err(e) => {
                tracing::warn!(card_id, error = %e, "auto_submit: card_get failed");
                return;
            }
        };

        // PR3a (#293) — push-path skip. When the card payload carries a
        // non-empty `codex_thread_id`, the spec card is running in
        // app-server *push* mode: the kernel already booted the
        // `codex app-server`, ran `turn/start` with the goal, and the
        // browser TUI is a `codex resume <tid> --remote …` that *rejoins*
        // that thread. Turn #1 is already in flight; there is no
        // composer-pre-filled prompt to "Enter", and injecting a `\r`
        // into a resumed TUI would be a spurious empty submission. Bail
        // (the card is already dedup-marked above, so we never retry).
        if let Some(tid) = card.payload.get("codex_thread_id").and_then(|v| v.as_str())
            && !tid.trim().is_empty()
        {
            tracing::debug!(
                card_id,
                codex_thread_id = tid,
                "auto_submit: card has codex_thread_id (push mode); skip \\r injection"
            );
            return;
        }

        // Pull `prompt` + `terminal_id` off the payload. Empty / missing
        // `prompt` is the "user spawned codex without auto-submit" path —
        // do nothing, keep TUI behavior identical to today.
        let prompt = card
            .payload
            .get("prompt")
            .and_then(|v| v.as_str())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty());
        if prompt.is_none() {
            tracing::debug!(
                card_id,
                "auto_submit: payload has no non-empty prompt; skip"
            );
            return;
        }
        let terminal_id = match card.payload.get("terminal_id").and_then(|v| v.as_str()) {
            Some(t) if !t.is_empty() => t.to_string(),
            _ => {
                tracing::warn!(
                    card_id,
                    "auto_submit: codex card missing payload.terminal_id; skip"
                );
                return;
            }
        };

        if let Err(e) = self
            .daemon
            .inject_stdin_renderer(
                self.terminal_renderer.as_ref(),
                &terminal_id,
                b"\r",
                INJECT_STDIN_TIMEOUT,
            )
            .await
        {
            tracing::warn!(
                card_id,
                terminal_id,
                error = %e,
                "auto_submit: inject_stdin failed; user can hit Enter manually"
            );
        } else {
            tracing::info!(
                card_id,
                terminal_id,
                "auto_submit: composer Enter delivered"
            );
        }
    }
}
