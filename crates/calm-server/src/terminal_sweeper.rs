//! The terminal sweeper: one 30 s tick, two independent arms — the orphan arm reaps terminal rows whose card
//! has no active worker session; the completed-track arm ends worker sessions still running on a completed or archived track.

use std::time::Duration;

use crate::db::sqlite::terminal_delete_tx;
use crate::db::{write_in_tx_typed, write_with_event_typed};
use crate::error::Result;
use crate::event::{Event, EventScope};
use crate::ids::ActorId;
use crate::model::Terminal;
use crate::session_projection_repo::WorkerSessionState;
use crate::state::AppState;
use crate::terminal_renderer::{RendererDropOutcome, TerminalRendererRegistry};
use calm_session::control::ProcSignal;
use sqlx::Row;

/// A PTY exit ends an ephemeral session. For a resumable session it is only viewer/liveness
/// evidence: the provider death arbiter and explicit completion retain authority.
pub(crate) async fn complete_ephemeral_session_from_terminal_exit(
    repo: &dyn crate::db::RouteRepo,
    terminal_id: &str,
    terminal_status: crate::session_projection_repo::WorkerSessionState,
) -> Result<()> {
    use crate::db::sqlite::{
        session_complete_tx, session_get_tx, session_projection_active_for_terminal_tx,
    };
    use calm_types::worker::{SessionMode, WorkerSessionId};
    let terminal_id = terminal_id.to_owned();
    crate::db::write_in_tx_typed(repo, move |tx| {
        Box::pin(async move {
            let Some(active) = session_projection_active_for_terminal_tx(tx, &terminal_id).await?
            else {
                return Ok(());
            };
            let session = session_get_tx(tx, &WorkerSessionId(active.id.clone()))
                .await?
                .ok_or_else(|| {
                    crate::error::CalmError::NotFound(format!("worker session {}", active.id))
                })?;
            if session.mode == SessionMode::Ephemeral {
                session_complete_tx(tx, &active.id, terminal_status).await?;
            }
            Ok(())
        })
    })
    .await
}

/// Actor stamped on every event the sweeper produces.
const fn sweeper_actor() -> ActorId {
    ActorId::Kernel
}

/// 30 s is comfortably below the 1-minute grace window — every orphan that exists at one tick is caught the next.
const SWEEP_INTERVAL: Duration = Duration::from_secs(30);

/// Grace window between terminal creation and orphan eligibility. Absorbs the 3-step
/// terminal-card create race; err on the side of "never reap a live terminal mid-create".
const ORPHAN_GRACE_SECONDS: i64 = 60;

/// Maximum time we wait for the daemon to accept a `Kill`; if it's hung, fall through to SIGTERM
/// rather than block the sweep tick.
const GRACEFUL_KILL_TIMEOUT: Duration = Duration::from_secs(5);

/// The set the completed-track arm ends. `created_at_ms <=` keeps a session opened on an already-done
/// track out of the set; harness rows are outside structurally (never `running`, no `terminal_run_id`).
pub const COMPLETED_TRACK_LIVE_SESSIONS_SQL: &str = "SELECT ws.id, ws.provider, ws.card_id, te.id AS terminal_id, ws.thread_id \
       FROM worker_sessions ws JOIN tracks t ON t.id = ws.track_id \
       JOIN terminals te ON te.id = ws.terminal_run_id AND te.exit_code IS NULL AND te.signal_killed = 0 \
      WHERE ws.state = 'running' \
        AND ( (t.lifecycle = 'done' AND ws.created_at_ms <= t.terminal_at) \
           OR (t.archived_at IS NOT NULL AND ws.created_at_ms <= t.archived_at) ) \
        AND NOT EXISTS (SELECT 1 FROM current_tasks ct WHERE ct.track_id = t.id AND ct.worker_card_id = ws.card_id \
                          AND ct.status IN ('dispatched','running','verifying'))";

/// Spawn the sweeper task. Subscribes to no events; purely time-driven.
pub fn spawn(state: AppState) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(SWEEP_INTERVAL);
        // Skip the immediate first tick so boot doesn't race the sweep.
        tick.tick().await;
        loop {
            tick.tick().await;
            if let Err(e) = sweep(&state).await {
                tracing::warn!(error = %e, "terminal_sweeper: sweep failed");
            }
        }
    });
}

/// One sweep pass: the orphan arm, then the completed-track arm; integration tests drive it without the interval task.
pub async fn sweep(state: &AppState) -> Result<()> {
    let orphans = state.repo.terminals_orphaned(ORPHAN_GRACE_SECONDS).await?;
    if !orphans.is_empty() {
        tracing::info!(count = orphans.len(), "terminal_sweeper: reaping orphans");
    }
    for term in orphans {
        if let Err(e) = cleanup_terminal(state, &term).await {
            tracing::warn!(
                terminal_id = %term.id,
                error = %e,
                "terminal_sweeper: cleanup failed (row will be retried next tick)"
            );
        }
    }

    let completed = completed_track_live_sessions(state).await?;
    if !completed.is_empty() {
        tracing::info!(
            count = completed.len(),
            "terminal_sweeper: ending sessions left running on completed tracks"
        );
    }
    for session in completed {
        if let Err(e) = end_completed_track_session(state, &session).await {
            tracing::warn!(
                worker_session_id = %session.id,
                terminal_id = %session.terminal_id,
                error = %e,
                "terminal_sweeper: ending a completed-track session failed (before the \
                 exited write: retried next tick; after it: the orphan arm converges)"
            );
        }
    }
    Ok(())
}

/// One row of [`COMPLETED_TRACK_LIVE_SESSIONS_SQL`]. `pub` (fields too) so
/// the test suite can hand [`end_completed_track_session`] a candidate that
/// went stale between the set read and the action.
pub struct CompletedTrackSession {
    pub id: String,
    pub provider: String,
    pub card_id: String,
    pub terminal_id: String,
    /// Captured with the candidate: the interrupt is addressed by it, never by a lookup through the
    /// session row, which is already `exited` by then and no longer resolves as active.
    pub thread_id: Option<String>,
}

async fn completed_track_live_sessions(state: &AppState) -> Result<Vec<CompletedTrackSession>> {
    let Some(pool) = state.sqlite_pool() else {
        return Ok(Vec::new());
    };
    let rows = sqlx::query(COMPLETED_TRACK_LIVE_SESSIONS_SQL)
        .fetch_all(&pool)
        .await?;
    Ok(rows
        .iter()
        .map(|r| CompletedTrackSession {
            id: r.get("id"),
            provider: r.get("provider"),
            card_id: r.get("card_id"),
            terminal_id: r.get("terminal_id"),
            thread_id: r.get("thread_id"),
        })
        .collect())
}

/// End one session of the completed-track set. The claim runs first on an IMMEDIATE transaction: a reopen
/// or dispatch since the candidate was read takes the row out of the set, and nothing is written or signalled.
pub async fn end_completed_track_session(
    state: &AppState,
    session: &CompletedTrackSession,
) -> Result<()> {
    end_completed_track_session_impl(state, session, || async {}).await
}

/// Fixtures-only seam: `before_write` runs after the guard, immediately before the claiming
/// write — the window a reopen can land in.
#[cfg(feature = "fixtures")]
#[doc(hidden)]
pub async fn end_completed_track_session_before_write_for_test<F, Fut>(
    state: &AppState,
    session: &CompletedTrackSession,
    before_write: F,
) -> Result<()>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    end_completed_track_session_impl(state, session, before_write).await
}

async fn end_completed_track_session_impl<F, Fut>(
    state: &AppState,
    session: &CompletedTrackSession,
    before_write: F,
) -> Result<()>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let _operation_guard = state.operation_runtime.lock_for_track_delete().await;
    crate::operation::terminal_disposal::require_safe(
        state.repo.as_ref(),
        crate::operation::terminal_disposal::Scope::Terminal(session.terminal_id.clone()),
        state.daemon.proc_supervisor_sock.as_deref(),
    )
    .await?;

    before_write().await;

    let session_id = session.id.clone();
    let claimed = write_in_tx_typed(state.repo.as_ref(), move |tx| {
        Box::pin(async move {
            // A reopen (`track_update_tx`) either committed before this BEGIN — the row is not returned and
            // nothing is written — or waits behind this IMMEDIATE transaction.
            if !in_completed_track_set(tx, &session_id).await? {
                return Ok(false);
            }
            crate::db::sqlite::session_complete_tx(tx, &session_id, WorkerSessionState::Exited)
                .await?;
            Ok(true)
        })
    })
    .await?;
    if !claimed {
        tracing::debug!(
            worker_session_id = %session.id,
            terminal_id = %session.terminal_id,
            "terminal_sweeper: completed-track candidate left the set before the write; skipped"
        );
        return Ok(());
    }
    tracing::info!(
        worker_session_id = %session.id,
        terminal_id = %session.terminal_id,
        provider = %session.provider,
        "terminal_sweeper: session left running on a completed track written exited"
    );

    if session.provider == "codex"
        && let Some(thread_id) = session.thread_id.as_deref()
        && let Err(e) = state
            .shared_codex_appserver
            .interrupt_active_turn(thread_id)
            .await
    {
        tracing::warn!(
            target: "shared_codex_daemon::orphan_turn",
            worker_session_id = %session.id,
            card_id = %session.card_id,
            thread_id = %thread_id,
            error = %e,
            "failed to interrupt active shared codex turn while ending a completed-track session"
        );
    }

    let Some(term) = state.repo.terminal_get(&session.terminal_id).await? else {
        return Ok(());
    };
    match crate::ws::terminal::resolve_live_renderer_from_terminal(state, term.clone()).await? {
        crate::ws::terminal::LiveRenderer::Alive(_) => {
            reap_terminal_artifacts_with_renderer(Some(state.terminal_renderer.as_ref()), &term)
                .await;
        }
        crate::ws::terminal::LiveRenderer::ChildExited { exit_code } => {
            tracing::info!(
                terminal_id = %term.id,
                ?exit_code,
                "terminal_sweeper: no renderer obtained for a completed-track session; \
                 left to the orphan arm / boot reconcile"
            );
        }
    }
    Ok(())
}

/// The set narrowed to one session, built from the same text so the set and the claim cannot drift.
/// Runs on the claiming transaction's own connection, never on the pool.
async fn in_completed_track_set(
    conn: &mut sqlx::SqliteConnection,
    session_id: &str,
) -> Result<bool> {
    let sql = format!("{COMPLETED_TRACK_LIVE_SESSIONS_SQL} AND ws.id = ?1");
    let row = sqlx::query(&sql)
        .bind(session_id)
        .fetch_optional(conn)
        .await?;
    Ok(row.is_some())
}

/// Reap a single orphan using the existing cleanup behavior only after its
/// prepared task launch is resolved. Missing artifacts do not discharge an
/// unknown launch request that may still reach the supervisor.
async fn cleanup_terminal(state: &AppState, term: &Terminal) -> Result<()> {
    let _operation_guard = state.operation_runtime.lock_for_track_delete().await;
    crate::operation::terminal_disposal::require_safe(
        state.repo.as_ref(),
        crate::operation::terminal_disposal::Scope::Terminal(term.id.clone()),
        state.daemon.proc_supervisor_sock.as_deref(),
    )
    .await?;
    reap_terminal_artifacts(state, term).await;

    // Audit-log + row delete in one transaction. If the card has already been deleted (the common
    // case), fall back to `EventScope::System`; a missing ancestor never refuses the reap.
    let terminal_id = term.id.clone();
    let card_id = term.card_id.clone();
    let scope = match state.repo.card_get(card_id.as_str()).await? {
        Some(c) => match state.repo.track_get(c.track_id.as_str()).await? {
            Some(w) => EventScope::Card {
                card: c.id,
                track: w.id,
                area: w.area_id,
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
        state.write(),
        move |tx| {
            Box::pin(async move {
                crate::operation::terminal_disposal::require_safe_tx(
                    tx,
                    &crate::operation::terminal_disposal::Scope::Terminal(terminal_id.clone()),
                )
                .await?;
                // The eager-teardown handlers (and a prior sweep tick) may already have removed the row:
                // NotFound is translated to Ok(()).
                match terminal_delete_tx(tx, &terminal_id)
                    .await
                    .map_err(crate::error::CalmError::from)
                {
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

/// Daemon + socket housekeeping for a single terminal row, shared between the sweeper and the
/// eager-teardown route handlers. Idempotent; the caller owns the row-delete step. Worst-case
/// latency is `GRACEFUL_KILL_TIMEOUT` when the daemon is hung.
pub async fn reap_terminal_artifacts(state: &AppState, term: &Terminal) {
    reap_terminal_artifacts_with_renderer(Some(state.terminal_renderer.as_ref()), term).await;
}

pub async fn reap_terminal_artifacts_with_renderer(
    renderer: Option<&TerminalRendererRegistry>,
    term: &Terminal,
) {
    // 1. Graceful shutdown through the in-process renderer, on a fresh supervisor UDS connection
    // so it bypasses any queued PTY writes stuck behind backpressure.
    if let Some((renderer, entry)) = renderer.and_then(|r| r.get(&term.id).map(|e| (r, e))) {
        match tokio::time::timeout(
            GRACEFUL_KILL_TIMEOUT,
            entry.shutdown_signal(ProcSignal::Term),
        )
        .await
        {
            Ok(()) => {
                tracing::debug!(terminal_id = %term.id, "renderer shutdown signal delivered");
            }
            Err(_) => {
                tracing::debug!(
                    terminal_id = %term.id,
                    "renderer shutdown signal timed out; falling through to pid fallback"
                );
            }
        }
        renderer.drop_entry(&term.id).await;
    } else {
        tracing::warn!(
            terminal_id = %term.id,
            "no live renderer entry while reaping terminal; using pid fallback if available"
        );
    }
    // The generated Planner hook settings file (server-owned path derived from the card id;
    // never a path read from the row's env).
    if let Some(renderer) = renderer {
        renderer.remove_hook_settings(term.card_id.as_str());
    }

    // 2. SIGTERM fallback. Skipped when no pid persisted.
    if let Some(pid) = term.pid
        && let Err(e) = send_sigterm(pid)
    {
        // Common case once the graceful path took: ESRCH (process already gone).
        tracing::debug!(
            terminal_id = %term.id,
            pid,
            error = %e,
            "SIGTERM failed (likely already exited)"
        );
    }
}

/// Preserve unresolved task launch ownership before the existing terminal
/// deletion checks. A missing PID or negative probe cannot discharge a pending
/// EnsureProc. Observed leader exit does not prove all descendants stopped.
pub async fn quiesce_terminal_artifacts_for_deletion(
    renderer: Option<&TerminalRendererRegistry>,
    supervisor_sock: Option<&std::path::Path>,
    term: &Terminal,
) -> crate::error::Result<()> {
    // A renderer entry gives a supervisor-owned `proc_id` to TERM/KILL. Without the entry, the
    // legacy row has no `(pid,start_time,boot_id)` ownership proof, so deletion must observe only
    // and fail closed instead of signaling a possibly recycled pid.
    let renderer_outcome = match renderer {
        Some(registry) => {
            registry.require_disposal_safe(&term.id).await?;
            registry.drop_entry_for_deletion(&term.id).await
        }
        None => RendererDropOutcome::Missing,
    };
    if renderer_outcome == RendererDropOutcome::ExitPersisted {
        return Ok(());
    }
    if let Some(pid) = term.pid {
        match wait_for_pid_exit_passively(pid, Duration::from_secs(2)).await {
            PassivePidExit::Exited | PassivePidExit::InvalidPid => {}
            verdict @ (PassivePidExit::StillAlive | PassivePidExit::Unsupported) => {
                return Err(crate::error::CalmError::Internal(format!(
                    "terminal {} did not quiesce before deletion ({verdict:?}; \
                     renderer_outcome={renderer_outcome:?}); refusing an unverified pid signal",
                    term.id,
                )));
            }
        }
    } else if let Some(supervisor_sock) = supervisor_sock {
        match tokio::time::timeout(
            Duration::from_secs(2),
            crate::probe_supervisor_for_terminal_at(Some(supervisor_sock), &term.id),
        )
        .await
        {
            Ok(Ok(false)) => {}
            Ok(Ok(true)) => {
                return Err(crate::error::CalmError::Internal(format!(
                    "terminal {} is still running in the process supervisor; refusing to move its workspace",
                    term.id
                )));
            }
            Ok(Err(error)) => {
                return Err(crate::error::CalmError::Internal(format!(
                    "terminal {} has no persisted pid and supervisor absence could not be proven: {error}",
                    term.id
                )));
            }
            Err(_) => {
                return Err(crate::error::CalmError::Internal(format!(
                    "terminal {} supervisor absence probe timed out; refusing to move its workspace",
                    term.id
                )));
            }
        }
    } else if renderer_outcome == RendererDropOutcome::Unverified {
        return Err(crate::error::CalmError::Internal(format!(
            "terminal {} renderer shutdown was not verified and no persisted pid is available; refusing to move its workspace",
            term.id
        )));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PassivePidExit {
    Exited,
    InvalidPid,
    StillAlive,
    Unsupported,
}

#[cfg(unix)]
async fn wait_for_pid_exit_passively(pid: i64, timeout: Duration) -> PassivePidExit {
    use nix::errno::Errno;
    use nix::sys::signal::kill;
    use nix::unistd::Pid;
    use tokio::time::Instant;

    let Ok(raw) = valid_raw_pid(pid) else {
        return PassivePidExit::InvalidPid;
    };
    let pid = Pid::from_raw(raw);
    let deadline = Instant::now() + timeout;
    loop {
        match kill(pid, None) {
            Ok(()) if pid_has_finished_shutdown(raw) => return PassivePidExit::Exited,
            Ok(()) => {}
            Err(Errno::ESRCH) => return PassivePidExit::Exited,
            Err(_) => return PassivePidExit::Unsupported,
        }
        let now = Instant::now();
        if now >= deadline {
            return PassivePidExit::StillAlive;
        }
        tokio::time::sleep(std::cmp::min(Duration::from_millis(50), deadline - now)).await;
    }
}

#[cfg(not(unix))]
async fn wait_for_pid_exit_passively(_pid: i64, _timeout: Duration) -> PassivePidExit {
    PassivePidExit::Unsupported
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitForPidExit {
    Exited,
    InvalidPid,
    StillAliveAfterSigkill,
    Unsupported,
}

/// Wait until a previously-signaled daemon has run its shutdown cleanup: a stale daemon may
/// still unlink the socket path during its own shutdown.
pub async fn wait_for_pid_exit(pid: i64, timeout: Duration) -> WaitForPidExit {
    wait_for_pid_exit_with_poll(pid, timeout, Duration::from_millis(50)).await
}

/// SIGTERM a known pid for a partial spawn that wrote `pid` to the terminal row but never
/// reached the renderer entry write; `reap_terminal_artifacts` keys off the entry and would be
/// a no-op. Best-effort: a failed `kill` (usually ESRCH) is logged and swallowed.
pub fn reap_terminal_pid_only(terminal_id: &str, pid: i64) {
    if let Err(e) = send_sigterm(pid) {
        tracing::debug!(
            terminal_id = %terminal_id,
            pid,
            error = %e,
            "reap_terminal_pid_only: SIGTERM failed (likely already exited or recycled)"
        );
    } else {
        tracing::info!(
            terminal_id = %terminal_id,
            pid,
            "reap_terminal_pid_only: SIGTERM delivered to pid-only partial-spawn daemon"
        );
    }
}

#[cfg(unix)]
async fn wait_for_pid_exit_with_poll(
    pid: i64,
    timeout: Duration,
    poll_interval: Duration,
) -> WaitForPidExit {
    use nix::errno::Errno;
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;
    use tokio::time::Instant;

    let Ok(raw) = valid_raw_pid(pid) else {
        return WaitForPidExit::InvalidPid;
    };
    let pid = Pid::from_raw(raw);
    let deadline = Instant::now() + timeout;

    loop {
        match kill(pid, None) {
            Ok(()) => {
                if pid_has_finished_shutdown(raw) {
                    return WaitForPidExit::Exited;
                }
            }
            Err(Errno::ESRCH) => return WaitForPidExit::Exited,
            Err(_) => {}
        }

        let now = Instant::now();
        if now >= deadline {
            break;
        }
        tokio::time::sleep(std::cmp::min(poll_interval, deadline - now)).await;
    }

    let _ = kill(pid, Signal::SIGKILL);
    let sigkill_deadline = Instant::now() + Duration::from_millis(500);
    loop {
        match kill(pid, None) {
            Ok(()) => {
                if pid_has_finished_shutdown(raw) {
                    return WaitForPidExit::Exited;
                }
            }
            Err(Errno::ESRCH) => return WaitForPidExit::Exited,
            Err(_) => {}
        }

        let now = Instant::now();
        if now >= sigkill_deadline {
            return WaitForPidExit::StillAliveAfterSigkill;
        }
        tokio::time::sleep(std::cmp::min(poll_interval, sigkill_deadline - now)).await;
    }
}

#[cfg(not(unix))]
async fn wait_for_pid_exit_with_poll(
    _pid: i64,
    _timeout: Duration,
    _poll_interval: Duration,
) -> WaitForPidExit {
    WaitForPidExit::Unsupported
}

#[cfg(unix)]
fn valid_raw_pid(pid: i64) -> std::io::Result<i32> {
    let raw: i32 = i32::try_from(pid)
        .map_err(|_| std::io::Error::other(format!("pid {pid} out of range for i32")))?;
    if raw <= 0 {
        return Err(std::io::Error::other(format!(
            "refusing to signal non-positive pid {raw}"
        )));
    }
    Ok(raw)
}

#[cfg(all(unix, target_os = "linux"))]
fn pid_has_finished_shutdown(pid: i32) -> bool {
    proc_stat_state(pid).is_some_and(|state| state == 'Z')
}

#[cfg(all(unix, not(target_os = "linux")))]
fn pid_has_finished_shutdown(_pid: i32) -> bool {
    false
}

#[cfg(all(unix, target_os = "linux"))]
fn proc_stat_state(pid: i32) -> Option<char> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_comm = stat.rsplit_once(") ")?.1;
    after_comm.chars().next()
}

#[cfg(unix)]
fn send_sigterm(pid: i64) -> std::io::Result<()> {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;
    // Sentinel values like 0/-1 would target the calling process group or all processes —
    // guard against persistence corruption.
    let raw = valid_raw_pid(pid)?;
    kill(Pid::from_raw(raw), Signal::SIGTERM)
        .map_err(|e| std::io::Error::other(format!("kill(SIGTERM, {raw}) failed: {e}")))
}

#[cfg(not(unix))]
fn send_sigterm(_pid: i64) -> std::io::Result<()> {
    // No-op on non-unix; the graceful socket path is our only lever.
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    #[tokio::test]
    async fn wait_for_pid_exit_returns_promptly_for_dead_pid() {
        let mut child = Command::new("true")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn true");
        let pid = child.id() as i64;
        child.wait().expect("reap true");

        let start = tokio::time::Instant::now();
        let outcome =
            wait_for_pid_exit_with_poll(pid, Duration::from_secs(3), Duration::from_millis(10))
                .await;

        assert_eq!(outcome, WaitForPidExit::Exited);
        assert!(
            start.elapsed() < Duration::from_millis(200),
            "dead pid wait should return promptly, took {:?}",
            start.elapsed()
        );
    }

    #[tokio::test]
    async fn wait_for_pid_exit_is_bounded_for_lingering_pid() {
        let mut child = Command::new("sleep")
            .arg("5")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sleep");
        let pid = child.id() as i64;

        let start = tokio::time::Instant::now();
        let outcome =
            wait_for_pid_exit_with_poll(pid, Duration::from_millis(100), Duration::from_millis(10))
                .await;
        let elapsed = start.elapsed();

        let _ = child.kill();
        let _ = child.wait();

        assert!(
            matches!(
                outcome,
                WaitForPidExit::Exited | WaitForPidExit::StillAliveAfterSigkill
            ),
            "unexpected wait outcome: {outcome:?}"
        );
        assert!(
            elapsed < Duration::from_secs(1),
            "lingering pid wait should stay bounded, took {elapsed:?}"
        );
    }
}
