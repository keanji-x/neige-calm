//! Orphan-terminal cleanup sweeper — **fallback** layer. See
//! `docs/sync-engine-design.md` §10.
//!
//! ## Two-layer cleanup model (issue #197)
//!
//! Terminal rows are owned by a single card row via
//! `terminals.card_id` (UNIQUE, NOT NULL). A `calm-session-daemon`
//! process + unix socket live alongside the row. Cleanup happens in two
//! layers:
//!
//!   1. **Eager teardown in the route handler.** When a user issues
//!      `DELETE /api/cards/:id`, `DELETE /api/waves/:id`, or
//!      `DELETE /api/coves/:id`, the handler walks the affected card
//!      list, calls [`reap_terminal_artifacts`] to kill the daemon +
//!      unlink the socket + delete the terminal row, *then* deletes the
//!      card / wave / cove row. The `terminals.card_id` FK is
//!      `ON DELETE RESTRICT` (migration 0011), so a missed cleanup
//!      surfaces as a transaction-level FK error rather than a silent
//!      daemon-process leak.
//!   2. **This sweeper.** Catches the residual shape: a crashed server,
//!      a SIGKILL'd writer, or a partial-success transaction that left
//!      a terminal row whose `card_id` no longer matches any
//!      `cards.payload.terminal_id`. The orphan SQL definition is
//!      unchanged from the pre-#197 contract (see
//!      [`crate::db::RepoRead::terminals_orphaned`]) so the
//!      `eventBridge.tsx:60-70` 3-step terminal-card creation race
//!      window is still absorbed by the 60-second grace.
//!
//! ## What the sweeper is *not*
//!
//! Pre-#197, the sweeper was documented as the cleanup path for the
//! card-delete happy case: the FK cascade nuked the `terminals` row,
//! and the sweeper was supposed to "catch the leak" — but in practice
//! it had nothing to catch (the row was already gone) and the daemon
//! process kept running until the next 30 s tick at best. That model
//! was wrong; the design doc lied. Card / wave / cove delete now own
//! their own teardown synchronously, and this sweeper exists only for
//! crash-recovery / partial-write residue.
//!
//! ## Cleanup sequence per orphan
//!
//! 1. **Graceful Kill via unix socket** (`GRACEFUL_KILL_TIMEOUT`). The
//!    daemon's `Attach → Kill` path triggers a SIGHUP to its child and
//!    a clean shutdown. Best-effort: if the socket doesn't connect or
//!    `Kill` write fails, fall through.
//! 2. **SIGTERM via PID** (`SIGTERM_GRACE`). Falls back when the
//!    graceful path didn't take. Skipped if `pid` is `None` (row
//!    predates Scope C).
//! 3. **Socket file removal.** Best-effort `unlink`; missing socket is
//!    fine (the daemon may already have removed it on clean exit).
//! 4. **Row delete via `write_with_event`** emitting
//!    `Event::TerminalDeleted { id, card_id }` with `actor = "kernel"`.
//!    This step IS the audit signal — steps 1-3 are housekeeping.
//!
//! Steps 1-3 are also what the eager-teardown helper
//! [`reap_terminal_artifacts`] runs from the route handler. The
//! sweeper's row-delete step is what differentiates it: it happens
//! through `write_with_event` to emit an audit event in the
//! crash-recovery path, whereas the route-handler eager teardown
//! deletes the row inside the same transaction that's about to delete
//! the card and emits `Event::CardDeleted` (or `WaveDeleted` /
//! `CoveDeleted`) as the audit signal.

use std::path::Path;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;

use crate::db::sqlite::terminal_delete_tx;
use crate::db::write_with_event_typed;
use crate::error::Result;
use crate::event::{Event, EventScope};
use crate::ids::ActorId;
use crate::model::Terminal;
use crate::state::AppState;
use calm_session::{
    ClientCapabilities, ClientMsg, InitialScrollback, PROTOCOL_VERSION, PtySize, RenderEncoding,
    write_frame,
};
use uuid::Uuid;

/// Actor stamped on every event the sweeper produces. Distinct from
/// [`ActorId::User`] (REST) and [`ActorId::Plugin`]; matches the convention
/// used by `card_fsm` for kernel-internal projectors. PR2 of #136 typed
/// this from the legacy `"kernel"` string.
const fn sweeper_actor() -> ActorId {
    ActorId::Kernel
}

/// How often the sweep runs. 30 s is comfortably below the 1-minute grace
/// window — every orphan that exists at one tick is caught the next.
const SWEEP_INTERVAL: Duration = Duration::from_secs(30);

/// Grace window between terminal creation and orphan eligibility. Absorbs
/// the 3-step terminal-card create race (POST card → POST terminal →
/// PATCH card.payload — `web/src/app/eventBridge.tsx:60-70`). One minute
/// is overkill for the ~10 ms race window in practice; we err on the side
/// of "never reap a live terminal mid-create".
const ORPHAN_GRACE_SECONDS: i64 = 60;

/// Maximum time we wait for the daemon to accept a `ClientMsg::Kill` and
/// drop its socket. Short — if the daemon is healthy this completes in
/// single-digit ms; if it's hung, we fall through to SIGTERM rather than
/// block the sweep tick.
const GRACEFUL_KILL_TIMEOUT: Duration = Duration::from_secs(5);

/// Spawn the sweeper task. Subscribes to no events; purely time-driven.
pub fn spawn(state: AppState) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(SWEEP_INTERVAL);
        // Skip the immediate first tick — there's no point sweeping a
        // freshly-booted kernel with no terminals yet, and the test
        // harness is happier when boot doesn't race the sweep.
        tick.tick().await;
        loop {
            tick.tick().await;
            if let Err(e) = sweep(&state).await {
                tracing::warn!(error = %e, "terminal_sweeper: sweep failed");
            }
        }
    });
}

/// One sweep pass. Public-in-crate so integration tests can drive it
/// without standing up the interval task.
pub async fn sweep(state: &AppState) -> Result<()> {
    let orphans = state.repo.terminals_orphaned(ORPHAN_GRACE_SECONDS).await?;
    if orphans.is_empty() {
        return Ok(());
    }
    tracing::info!(count = orphans.len(), "terminal_sweeper: reaping orphans");
    for term in orphans {
        if let Err(e) = cleanup_terminal(state, &term).await {
            tracing::warn!(
                terminal_id = %term.id,
                error = %e,
                "terminal_sweeper: cleanup failed (row will be retried next tick)"
            );
        }
    }
    Ok(())
}

/// Reap a single orphan (sweeper path). Idempotent against missing
/// artifacts: a pre-deceased daemon, an already-unlinked socket, or a
/// stale `pid` pointing at a recycled OS process all collapse to "row
/// delete still succeeds, audit event still emits".
async fn cleanup_terminal(state: &AppState, term: &Terminal) -> Result<()> {
    // Steps 1-3: daemon + socket housekeeping, shared with the eager-
    // teardown route handlers via `reap_terminal_artifacts`.
    reap_terminal_artifacts(term).await;

    // Step 4: audit-log + row delete in one transaction. This step is
    // the headline guarantee: regardless of how steps 1-3 went, the row
    // leaves the kernel cleanly and any subscriber sees the
    // `terminal.deleted` event.
    //
    // Scope (PR2 of #136): try to resolve the card → wave → cove
    // chain so per-card subscribers see the reap. If the card has
    // already been deleted (the common case — the sweeper exists
    // precisely because card-delete may have left an orphan
    // terminal), fall back to `EventScope::System`. We don't refuse
    // the reap for a missing ancestor.
    let terminal_id = term.id.clone();
    let card_id = term.card_id.clone();
    let scope = match state.repo.card_get(card_id.as_str()).await? {
        Some(c) => match state.repo.wave_get(c.wave_id.as_str()).await? {
            Some(w) => EventScope::Card {
                card: c.id,
                wave: w.id,
                cove: w.cove_id,
            },
            None => EventScope::System,
        },
        None => EventScope::System,
    };
    let (_unit, _event_id) = write_with_event_typed(
        state.repo.as_ref(),
        sweeper_actor(),
        scope,
        None,
        &state.events,
        &state.card_role_cache,
        move |tx| {
            Box::pin(async move {
                // The eager-teardown handlers (and a prior sweep tick)
                // may already have removed the row. Treat NotFound as
                // "nothing to do, but still emit the audit event" — but
                // the audit event itself only makes sense when there
                // *was* something to clean up. We tolerate missing-row
                // here by translating NotFound to Ok(()).
                match terminal_delete_tx(tx, &terminal_id).await {
                    Ok(()) => {}
                    Err(crate::error::CalmError::NotFound(_)) => {
                        tracing::debug!(
                            terminal_id = %terminal_id,
                            "terminal row already gone (eager teardown or prior sweep)"
                        );
                    }
                    Err(e) => return Err(e),
                }
                Ok((
                    (),
                    Event::TerminalDeleted {
                        id: terminal_id,
                        card_id,
                    },
                ))
            })
        },
    )
    .await?;
    Ok(())
}

/// Daemon + socket housekeeping for a single terminal row, shared between
/// the sweeper and the eager-teardown route handlers (issue #197).
///
/// Idempotent: missing socket, dead pid, and absent `daemon_handle` /
/// `pid` all collapse to a clean return. The caller is responsible for
/// the *row delete* step (eager teardown: inside the surrounding
/// `card_delete_tx` / `wave_delete_tx` transaction; sweeper: inside its
/// own `write_with_event` audit transaction).
///
/// This is the synchronous bottom-half of the cleanup contract: steps
/// 1-3 in the module doc above. Bounded by `GRACEFUL_KILL_TIMEOUT` for
/// the graceful path; SIGTERM is non-blocking. Safe to call inline from
/// an HTTP handler — the worst-case latency is `GRACEFUL_KILL_TIMEOUT`
/// (5 s) when the daemon is hung; the common case is single-digit ms.
pub async fn reap_terminal_artifacts(term: &Terminal) {
    // 1. Graceful Kill via unix socket. Bounded by GRACEFUL_KILL_TIMEOUT.
    if let Some(sock) = term.daemon_handle.as_deref() {
        match tokio::time::timeout(
            GRACEFUL_KILL_TIMEOUT,
            graceful_kill_via_socket(Path::new(sock), &term.id),
        )
        .await
        {
            Ok(Ok(())) => {
                tracing::debug!(terminal_id = %term.id, sock = %sock, "graceful Kill delivered");
            }
            Ok(Err(e)) => {
                tracing::debug!(
                    terminal_id = %term.id,
                    sock = %sock,
                    error = %e,
                    "graceful Kill failed; falling through to SIGTERM"
                );
            }
            Err(_) => {
                tracing::debug!(
                    terminal_id = %term.id,
                    sock = %sock,
                    "graceful Kill timed out; falling through to SIGTERM"
                );
            }
        }
    }

    // 2. SIGTERM fallback. Skipped when no pid persisted (legacy rows or
    //    spawn-time write_pid failure).
    if let Some(pid) = term.pid
        && let Err(e) = send_sigterm(pid)
    {
        // Common case once the graceful path took: ESRCH (process
        // already gone). Log at debug so we don't spam in normal
        // operation.
        tracing::debug!(
            terminal_id = %term.id,
            pid,
            error = %e,
            "SIGTERM failed (likely already exited)"
        );
    }

    // 3. Remove the socket file. Best-effort; the daemon may have
    //    cleaned it up itself on graceful exit.
    if let Some(sock) = term.daemon_handle.as_deref() {
        let _ = std::fs::remove_file(sock);
    }
}

/// Open the daemon's unix socket, send the required v2 `ClientHello`
/// (so the daemon's handshake accepts us and routes the connection
/// through `TerminalSessionState`), then a `Kill` frame, drop the
/// connection. Bounded by the caller via `tokio::time::timeout`.
///
/// `ClientHello.role_hint` is left `None` so the sweeper is implicitly
/// promoted to Owner when no live client holds ownership — which is the
/// common case at reap time (the original client is long gone). If a live
/// client *does* still hold ownership, our `Kill` is rejected as
/// `NotOwner` and we fall through to the SIGTERM step. Either path lands
/// at "daemon process gone" within the bounded timeout.
async fn graceful_kill_via_socket(sock: &Path, terminal_id: &str) -> std::io::Result<()> {
    let stream = UnixStream::connect(sock).await?;
    let (_rd, mut wr) = stream.into_split();
    write_frame(
        &mut wr,
        &ClientMsg::ClientHello {
            protocol_version: PROTOCOL_VERSION,
            terminal_id: terminal_id.to_string(),
            client_id: Uuid::new_v4(),
            desired_size: PtySize {
                cols: 80,
                rows: 24,
                pixel_width: None,
                pixel_height: None,
            },
            cell_size: None,
            initial_scrollback: InitialScrollback::None,
            resume_from: None,
            role_hint: None,
            capabilities: ClientCapabilities {
                render_encodings: vec![RenderEncoding::Vt],
                supports_scrollback: false,
                supports_sixel: false,
                supports_images: false,
                kernel_originated_input: false,
            },
        },
    )
    .await
    .map_err(std::io::Error::other)?;
    write_frame(&mut wr, &ClientMsg::Kill)
        .await
        .map_err(std::io::Error::other)?;
    // Flush + close — `into_split` write half drops here, closing the
    // socket. The daemon picks up EOF and exits naturally after the Kill
    // has signaled its child.
    let _ = wr.shutdown().await;
    Ok(())
}

#[cfg(unix)]
fn send_sigterm(pid: i64) -> std::io::Result<()> {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;
    // Stored as i64 in sqlite for INTEGER affinity; on unix `pid_t` is
    // i32, so a cast is safe within the legal pid range (>0, <2^22 on
    // Linux). Sentinel values like 0/-1 would target the calling process
    // group or all processes — guard against persistence corruption.
    let raw: i32 = i32::try_from(pid)
        .map_err(|_| std::io::Error::other(format!("pid {pid} out of range for i32")))?;
    if raw <= 0 {
        return Err(std::io::Error::other(format!(
            "refusing to signal non-positive pid {raw}"
        )));
    }
    kill(Pid::from_raw(raw), Signal::SIGTERM)
        .map_err(|e| std::io::Error::other(format!("kill(SIGTERM, {raw}) failed: {e}")))
}

#[cfg(not(unix))]
fn send_sigterm(_pid: i64) -> std::io::Result<()> {
    // No-op on non-unix; the graceful socket path is our only lever.
    Ok(())
}
