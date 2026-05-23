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

/// Spawn a `calm-session-daemon` for the given terminal row, wait for
/// its unix socket to accept connections, and persist the socket path
/// as the row's `daemon_handle`.
///
/// Used by `routes::terminal_cards::create_terminal_card` (atomic-create),
/// `routes::codex_cards::create_codex_card`, `spec_card::seed_and_spawn_spec_daemon`
/// (wave-create), `dispatcher::spawn_codex_worker`, and
/// `lib::revive_orphans_on_boot` (kernel-restart-only auto-revive in
/// commit 3). The previous `SpawnDaemonOpts` knob + `spawn_daemon_for_with_opts`
/// shim were removed in the #177 root-cause refactor — theme is read
/// directly from `term.theme_fg/bg` (NOT NULL via migration 0013), so
/// every caller produces identical daemon argv by construction.
pub(crate) async fn spawn_daemon_for(
    s: &AppState,
    term: &Terminal,
    program: &str,
    cwd: &str,
    env: &serde_json::Value,
) -> Result<()> {
    spawn_daemon_with_parts(s.daemon.as_ref(), s.repo.as_ref(), term, program, cwd, env).await
}

/// Lower-level seam over [`spawn_daemon_for`] that takes the constituent
/// `DaemonClient` + `&dyn RouteRepo` instead of the full `AppState`.
/// Used by the dispatcher (which doesn't own an `AppState` — it's a
/// kernel-internal worker that ships before `AppState` exists in the
/// boot order). Identical semantics to `spawn_daemon_for`; the latter
/// is now a one-line forwarder.
pub(crate) async fn spawn_daemon_with_parts(
    daemon: &DaemonClient,
    repo: &dyn RouteRepo,
    term: &Terminal,
    program: &str,
    cwd: &str,
    env: &serde_json::Value,
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

    // #177 root-cause refactor — theme is a row-creation invariant
    // (NOT NULL since migration 0013). Read directly from the row;
    // every spawn path (initial codex-card, wave-create spec card,
    // dispatcher worker, boot-time orphan revive) renders identical
    // argv by construction. No opts override, no priority chain, no
    // "stay silent if absent" branch — those were the seams that
    // allowed un-themed daemons to win socket races.
    let mut cmd = tokio::process::Command::new(&daemon.session_daemon_bin);
    cmd.args(["--id", &term.id])
        .args(["--sock", &sock_str])
        .args(["--cwd", cwd])
        .args(["--terminal-fg", &term.theme_fg])
        .args(["--terminal-bg", &term.theme_bg]);
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

#[cfg(test)]
mod tests {
    //! #177 PR2 — exercises the spawn-time theme priority + persistence.
    //!
    //! Why a unit test in this module: `spawn_daemon_with_parts` is
    //! `pub(crate)` so external integration tests can't reach it
    //! directly, and the function's two callers in tree (the initial
    //! codex-card / wave-create spawn and the WS auto-revive shim) sit
    //! across enough route plumbing that re-deriving the same set-up
    //! through an HTTP handler just to assert "the argv carries theme
    //! after a respawn that passes default opts" would dwarf the
    //! assertion itself. The argv-recorder fixture covers the bytes
    //! that leave the spawn; here we drive the helper directly so the
    //! coverage is precise and the test wall-clock is sub-second.
    //!
    //! Recorder protocol (see
    //! `crates/calm-server/tests/fixtures/argv-recorder-daemon/main.rs`):
    //!   * Kernel passes `--sock <path>` like the real daemon.
    //!   * Recorder writes `<path>.argv` (one line per argv element)
    //!     then binds the unix socket so the kernel's readiness poll
    //!     succeeds and `spawn_daemon_with_parts` returns Ok.

    use super::*;
    use crate::db::prelude::*;
    use crate::db::sqlite::SqlxRepo;
    use crate::model::{Card, CardRole, NewCard, NewCove, NewTerminal, NewWave};
    use crate::state::DaemonClient;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    /// `CARGO_BIN_EXE_argv-recorder-daemon` is only populated for the
    /// integration-test crate (target = test, kind = test). Unit tests
    /// inside `src/` don't get that env var — fall back to the same
    /// relative location Cargo emits the test bin to (`target/debug/`
    /// or `target/release/`). Resolves via the workspace's
    /// `CARGO_TARGET_DIR` / `OUT_DIR` chain.
    fn locate_recorder_bin() -> PathBuf {
        // First try the cleanest path: env from a sibling integration
        // test invocation. If unset (the common case under unit-test
        // runs), reconstruct from current_exe(): unit-test binaries
        // live at `target/<profile>/deps/<name>-<hash>`, recorder
        // binary at `target/<profile>/argv-recorder-daemon`.
        if let Ok(p) = std::env::var("CARGO_BIN_EXE_argv-recorder-daemon") {
            return PathBuf::from(p);
        }
        let me = std::env::current_exe().expect("current_exe");
        // .../target/<profile>/deps/<name>-<hash> → .../target/<profile>/argv-recorder-daemon
        let target_profile = me
            .parent()
            .and_then(|p| p.parent())
            .expect("test bin parent");
        let candidate = target_profile.join("argv-recorder-daemon");
        if candidate.exists() {
            return candidate;
        }
        panic!(
            "argv-recorder-daemon binary not found at {candidate:?}; \
             build the workspace first (`cargo build --tests --workspace`)"
        );
    }

    /// Read the recorder's sidecar file written next to `<sock>.argv`.
    /// Recorder writes argv before binding, but we still poll briefly
    /// because some filesystems flush writes lazily under load.
    fn read_argv_lines(sock: &str) -> Vec<String> {
        let argv_path = format!("{sock}.argv");
        let start = Instant::now();
        loop {
            if let Ok(text) = std::fs::read_to_string(&argv_path) {
                if !text.is_empty() {
                    return text.lines().map(String::from).collect();
                }
            }
            if start.elapsed() > Duration::from_secs(3) {
                panic!("argv file {argv_path:?} never appeared / stayed empty");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Set up the minimum surface — repo + DaemonClient (pointing at the
    /// recorder fixture) + a card → terminal row pair — needed to drive
    /// `spawn_daemon_with_parts` directly.
    async fn boot() -> (Arc<dyn Repo>, Arc<DaemonClient>, tempfile::TempDir, String) {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let repo: Arc<dyn Repo> = Arc::new(
            SqlxRepo::open("sqlite::memory:")
                .await
                .expect("open in-memory sqlite"),
        );
        // Seed a cove → wave → card so the FK chain on `terminals.card_id`
        // is satisfied.
        let cove = repo
            .cove_create(NewCove {
                name: "respawn-theme-test".into(),
                color: "#000".into(),
                sort: None,
            })
            .await
            .unwrap();
        let wave = repo
            .wave_create(NewWave {
                cove_id: cove.id.clone(),
                title: "respawn-theme-test".into(),
                sort: None,
                theme: crate::routes::theme::RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        let _card: Card = repo
            .card_create(NewCard {
                wave_id: wave.id.clone(),
                kind: "codex".into(),
                sort: None,
                payload: serde_json::json!({}),
            })
            .await
            .unwrap();
        let card_id_str = _card.id.to_string();
        // Now mint a terminal row parented to that card.
        let term = repo
            .terminal_create(NewTerminal {
                card_id: _card.id,
                program: "codex".into(),
                cwd: "/".into(),
                env: serde_json::json!({}),
                theme: crate::routes::theme::RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        let _ = card_id_str; // silence unused for now
        let daemon = Arc::new(DaemonClient {
            data_dir: tmp.path().to_path_buf(),
            session_daemon_bin: locate_recorder_bin(),
        });
        let _ = CardRole::Plain; // import-side-effect placeholder
        (repo, daemon, tmp, term.id)
    }

    /// #177 root-cause refactor — every spawn (initial, dispatcher,
    /// WS revive in commit 3) reads theme from the terminal row, which
    /// is NOT NULL by migration 0013. This test pins that single read
    /// path: a row created with one theme produces daemon argv with
    /// matching `--terminal-fg` / `--terminal-bg` flags. The pre-
    /// refactor tests (`auto_revive_respawn_reads_theme_from_row`,
    /// `auto_revive_respawn_without_persisted_theme_omits_flags`) were
    /// deleted along with the `SpawnDaemonOpts` opts-override path
    /// they exercised.
    #[tokio::test]
    async fn spawn_reads_theme_from_row() {
        let (repo, daemon, _tmp, term_id) = boot().await;

        let term = repo.terminal_get(&term_id).await.unwrap().expect("row");
        // The boot helper minted the row with `default_dark()` — assert
        // the canonical RGB lands on argv.
        assert_eq!(term.theme_fg, "216,219,226");
        assert_eq!(term.theme_bg, "15,20,24");

        spawn_daemon_with_parts(
            daemon.as_ref(),
            repo.as_ref(),
            &term,
            "codex",
            "/",
            &serde_json::json!({}),
        )
        .await
        .expect("spawn must succeed");

        let sock_path = daemon.sock_path(&term_id);
        let argv = read_argv_lines(&sock_path.to_string_lossy());
        let pairs: Vec<(String, String)> = argv
            .windows(2)
            .map(|w| (w[0].clone(), w[1].clone()))
            .collect();
        assert!(
            pairs
                .iter()
                .any(|(k, v)| k == "--terminal-fg" && v == "216,219,226"),
            "spawn must carry --terminal-fg from row; got: {argv:?}"
        );
        assert!(
            pairs
                .iter()
                .any(|(k, v)| k == "--terminal-bg" && v == "15,20,24"),
            "spawn must carry --terminal-bg from row; got: {argv:?}"
        );
    }
}
