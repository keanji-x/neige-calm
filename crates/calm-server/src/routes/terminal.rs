//! `/api/cards/:id/terminal` — read-side helpers for terminal cards.
//!
//! The companion write path used to live here (`POST /api/cards/:id/terminal`,
//! the second leg of the 3-step terminal-card recipe) but #13's atomic
//! endpoint replaced it. The single remaining route is the GET that
//! `useTodayTerminal` uses to validate a cached `card_id` from
//! `localStorage` before attempting a WS attach.
//!
//! `spawn_daemon_for` stays public because two other call sites still need
//! it: the new atomic-create handler in `routes::terminal_cards`, the codex
//! route's PTY spawn (`routes::codex`), and the WS attach path's
//! auto-revive (`ws::terminal`).

use crate::db::RouteRepo;
use crate::error::{CalmError, ErrorBody, Result};
use crate::model::Terminal;
use crate::state::{AppState, DaemonClient};
use axum::{
    Json, Router,
    extract::{Path, State},
    routing::get,
};
use std::process::Stdio;
use std::time::Duration;
use tokio::net::UnixStream;

pub fn router() -> Router<AppState> {
    Router::new().route("/api/cards/{card_id}/terminal", get(get_terminal_for_card))
}

/// Look up the Terminal row a card owns. Returns 404 if the card has no
/// terminal (yet). The UI uses this to validate a card_id cached in
/// localStorage before attempting a WS attach to its terminal.
#[utoipa::path(
    get,
    path = "/api/cards/{card_id}/terminal",
    tag = "terminals",
    params(("card_id" = String, Path, description = "Card id (must be a terminal card)")),
    responses(
        (status = 200, description = "Terminal row for this card", body = Terminal),
        (status = 404, description = "Card has no terminal yet", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn get_terminal_for_card(
    State(s): State<AppState>,
    Path(card_id): Path<String>,
) -> Result<Json<Terminal>> {
    let term = s
        .repo
        .terminal_get_by_card(&card_id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("terminal for card {card_id}")))?;
    Ok(Json(term))
}

/// Daemon-spawn options the caller may stamp on top of the defaults.
/// All fields are `Option` so existing call sites can `..Default::default()`
/// without churn.
///
/// `terminal_fg` / `terminal_bg` (#177): when set, the daemon advertises
/// these RGB values on OSC 10/11 queries so codex's startup probe gets
/// an answer matching the host browser's theme. The codex card route
/// passes them through from `NewCodexCardBody.theme`.
#[derive(Debug, Default, Clone)]
pub(crate) struct SpawnDaemonOpts {
    pub terminal_fg: Option<String>,
    pub terminal_bg: Option<String>,
}

/// Spawn a `calm-session-daemon` for the given terminal row, wait for its
/// unix socket to accept connections, and persist the socket path as the
/// row's `daemon_handle`. Used by `routes::terminal_cards::create_terminal_card`
/// (the atomic-create endpoint), the codex route's PTY spawn, and (when a
/// previously-spawned daemon has died) by the WS handler's auto-revive path.
pub(crate) async fn spawn_daemon_for(
    s: &AppState,
    term: &Terminal,
    program: &str,
    cwd: &str,
    env: &serde_json::Value,
) -> Result<()> {
    spawn_daemon_for_with_opts(s, term, program, cwd, env, SpawnDaemonOpts::default()).await
}

/// Same as [`spawn_daemon_for`] but accepts extra knobs (theme color
/// args, ...). Existing terminal-card callers go through the simpler
/// wrapper; codex cards (#177) use this to stamp `--terminal-fg` /
/// `--terminal-bg` onto the daemon argv.
pub(crate) async fn spawn_daemon_for_with_opts(
    s: &AppState,
    term: &Terminal,
    program: &str,
    cwd: &str,
    env: &serde_json::Value,
    opts: SpawnDaemonOpts,
) -> Result<()> {
    spawn_daemon_with_parts(
        s.daemon.as_ref(),
        s.repo.as_ref(),
        term,
        program,
        cwd,
        env,
        opts,
    )
    .await
}

/// PR6 (#136) — lower-level seam over `spawn_daemon_for` that takes the
/// constituent `DaemonClient` + `&dyn RouteRepo` instead of the full
/// `AppState`. Used by the dispatcher (which doesn't own an `AppState` —
/// it's a kernel-internal worker that ships before AppState exists in
/// the boot order). Identical semantics to `spawn_daemon_for`; the
/// latter is now a one-line forwarder.
///
/// The trailing `opts` (#177) lets callers stamp extra daemon argv
/// (e.g. `--terminal-fg` / `--terminal-bg`) without forcing every
/// caller to construct one — `spawn_daemon_for` passes
/// `SpawnDaemonOpts::default()` and the dispatcher does the same.
pub(crate) async fn spawn_daemon_with_parts(
    daemon: &DaemonClient,
    repo: &dyn RouteRepo,
    term: &Terminal,
    program: &str,
    cwd: &str,
    env: &serde_json::Value,
    opts: SpawnDaemonOpts,
) -> Result<()> {
    let sock = daemon.sock_path(&term.id);
    if let Some(parent) = sock.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| CalmError::Internal(format!("mkdir sock parent: {e}")))?;
    }
    // Stale leftover socket file from a previous daemon — must remove or
    // bind() refuses.
    if sock.exists() {
        let _ = std::fs::remove_file(&sock);
    }
    let sock_str = sock.to_string_lossy().to_string();

    let mut cmd = tokio::process::Command::new(&daemon.session_daemon_bin);
    cmd.args(["--id", &term.id])
        .args(["--sock", &sock_str])
        .args(["--cwd", cwd]);
    if let Some(fg) = opts.terminal_fg.as_deref() {
        cmd.args(["--terminal-fg", fg]);
    }
    if let Some(bg) = opts.terminal_bg.as_deref() {
        cmd.args(["--terminal-bg", bg]);
    }
    cmd.arg("--").args(["/bin/sh", "-c", program]);
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    if let Some(map) = env.as_object() {
        for (k, v) in map {
            if let Some(val) = v.as_str() {
                cmd.env(k, val);
            }
        }
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(false);

    let mut child = cmd
        .spawn()
        .map_err(|e| CalmError::Internal(format!("spawn calm-session-daemon: {e}")))?;
    let pid = child.id();
    tracing::info!(pid = ?pid, terminal_id = %term.id, "spawned calm-session-daemon");
    // Persist the pid so the orphan-terminal sweeper has a SIGTERM fallback
    // target when its graceful `ClientMsg::Kill` path doesn't take. Best-
    // effort: a failed write here is a degraded-cleanup signal but must
    // not abort the spawn (the daemon is running fine — we just lose the
    // SIGTERM lever for that row until the next respawn).
    if let Err(e) = repo.terminal_set_pid(&term.id, pid).await {
        tracing::warn!(
            terminal_id = %term.id,
            pid = ?pid,
            error = %e,
            "failed to persist terminal pid; sweeper will fall back to socket-Kill only"
        );
    }
    tokio::spawn(async move {
        let _ = child.wait().await;
    });

    // Poll until the daemon accepts connections (or give up after ~3s).
    let mut ready = false;
    for _ in 0..75 {
        if UnixStream::connect(&sock).await.is_ok() {
            ready = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    if !ready {
        return Err(CalmError::Internal(format!(
            "daemon for terminal {} did not become ready",
            term.id
        )));
    }
    repo.terminal_set_handle(&term.id, Some(&sock_str)).await?;
    Ok(())
}
