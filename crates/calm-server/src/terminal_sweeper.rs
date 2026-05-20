//! Orphan terminal cleanup sweeper. See `docs/sync-engine-design.md` §10.
//!
//! A `terminals` row leaks (its row, its `calm-session-daemon` process, and
//! its unix socket) when a terminal card is deleted today: `routes/cards.rs`
//! `card_delete` removes the card row, but the daemon process keeps running.
//! The FK cascade on `terminals.card_id` clears the row in production —
//! still, the daemon process and socket file are left behind. This sweeper
//! catches the leak by walking for terminals that no card points at via
//! `cards.payload.terminal_id` and reaping them through the same
//! `write_with_event` pipeline every other write uses. The cleanup lands in
//! the audit log as an `Event::TerminalDeleted` with `actor = "kernel"`.
//!
//! ## Lifecycle
//!
//! `spawn(state)` is called once at server boot from `AppState::new`,
//! modeled after `card_fsm::spawn`. It runs a `tokio::time::interval`
//! every `SWEEP_INTERVAL` and calls `sweep()` per tick. Errors from
//! `sweep()` are logged but do not bring the task down — we'd rather
//! recover next tick than crash the kernel.
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
//! ## Why not a user-initiated DELETE endpoint?
//!
//! Out of scope per the Scope C spec. If one lands later, it goes
//! through `write_with_event` identically and emits the same event;
//! the sweeper continues to catch leaked rows the explicit path missed.

use std::path::Path;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;

use crate::db::sqlite::terminal_delete_tx;
use crate::db::write_with_event_typed;
use crate::error::Result;
use crate::event::Event;
use crate::model::Terminal;
use crate::state::AppState;
use calm_session::{ClientMsg, write_frame};

/// Actor stamped on every event the sweeper produces. Distinct from
/// `"user"` (REST) and `"plugin:<id>"`; matches the convention used by
/// `card_fsm` for kernel-internal projectors.
const SWEEPER_ACTOR: &str = "kernel";

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

/// Reap a single orphan. Idempotent against missing artifacts: a
/// pre-deceased daemon, an already-unlinked socket, or a stale `pid`
/// pointing at a recycled OS process all collapse to "row delete still
/// succeeds, audit event still emits".
async fn cleanup_terminal(state: &AppState, term: &Terminal) -> Result<()> {
    // 1. Graceful Kill via unix socket. Bounded by GRACEFUL_KILL_TIMEOUT.
    if let Some(sock) = term.daemon_handle.as_deref() {
        match tokio::time::timeout(
            GRACEFUL_KILL_TIMEOUT,
            graceful_kill_via_socket(Path::new(sock)),
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

    // 4. Audit-log + row delete in one transaction. This step is the
    //    headline guarantee: regardless of how steps 1-3 went, the row
    //    leaves the kernel cleanly and any subscriber sees the
    //    `terminal.deleted` event.
    let terminal_id = term.id.clone();
    let card_id = term.card_id.clone();
    let (_unit, _event_id) = write_with_event_typed(
        state.repo.as_ref(),
        SWEEPER_ACTOR,
        None,
        &state.events,
        move |tx| {
            Box::pin(async move {
                // The FK cascade on `terminals.card_id` may have already
                // removed the row when the card was deleted. Treat
                // NotFound as "nothing to do, but still emit the audit
                // event" — but the audit event itself only makes sense
                // when there *was* something to clean up. So we tolerate
                // missing-row here by translating NotFound to Ok(()).
                match terminal_delete_tx(tx, &terminal_id).await {
                    Ok(()) => {}
                    Err(crate::error::CalmError::NotFound(_)) => {
                        tracing::debug!(
                            terminal_id = %terminal_id,
                            "terminal row already gone (FK cascade or prior sweep)"
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

/// Open the daemon's unix socket, send the required `Attach` then `Kill`
/// frame, drop the connection. Bounded by the caller via `tokio::time::timeout`.
async fn graceful_kill_via_socket(sock: &Path) -> std::io::Result<()> {
    let stream = UnixStream::connect(sock).await?;
    let (_rd, mut wr) = stream.into_split();
    // Daemon protocol requires Attach as the first frame; placeholder
    // viewport since we're not going to read anything back.
    write_frame(&mut wr, &ClientMsg::Attach { cols: 80, rows: 24 })
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
