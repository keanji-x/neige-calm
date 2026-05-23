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

    // #177 PR2 — priority for daemon argv theme args:
    //   1. `opts.terminal_fg/bg` (caller-supplied; the codex-card POST,
    //      wave-create spec-card spawn, and dispatcher worker all pass
    //      `Some(...)` here when the host browser sent a theme).
    //   2. `term.theme_fg/bg` (persisted on the row from a prior spawn
    //      or a mid-session `TerminalThemeUpdate`).
    //   3. None — daemon stays silent on OSC 10/11, codex falls back to
    //      its built-in default (matches pre-#177 behaviour).
    //
    // The fallback is what closes the WS auto-revive race: that path
    // passes `SpawnDaemonOpts::default()` so without (2) it would
    // re-spawn without theme args. With (2), the revive spawn reads
    // the same theme the initial spawn wrote — both candidate processes
    // in a socket race carry identical argv, so it no longer matters
    // which one wins.
    let chosen_fg = opts
        .terminal_fg
        .as_deref()
        .or(term.theme_fg.as_deref())
        .map(str::to_owned);
    let chosen_bg = opts
        .terminal_bg
        .as_deref()
        .or(term.theme_bg.as_deref())
        .map(str::to_owned);

    // #177 diagnostic — log the three theme inputs + chosen output
    // right before they become daemon argv. Layered with the upstream
    // logs in `wave_create` / `create_codex_card` / `seed_and_spawn_spec_daemon`,
    // this row pinpoints the exact layer that dropped the theme:
    //   - `opts.terminal_fg = None` + `term.theme_fg = None` ⇒ caller
    //     never passed it (browser, route, or spawn helper layer).
    //   - `opts.terminal_fg = Some(..)` but `chosen = None` ⇒ a future
    //     refactor broke the priority logic in this function.
    //   - `chosen = Some(..)` but the daemon never echoes correct OSC
    //     11 ⇒ the bug is downstream in `calm-session-daemon` /
    //     `RenderPlane::with_colors`.
    tracing::info!(
        terminal_id = %term.id,
        opts_terminal_fg = ?opts.terminal_fg,
        opts_terminal_bg = ?opts.terminal_bg,
        term_theme_fg = ?term.theme_fg,
        term_theme_bg = ?term.theme_bg,
        chosen_fg = ?chosen_fg,
        chosen_bg = ?chosen_bg,
        "spawn_daemon_with_parts: theme arg derivation",
    );

    let mut cmd = tokio::process::Command::new(&daemon.session_daemon_bin);
    cmd.args(["--id", &term.id])
        .args(["--sock", &sock_str])
        .args(["--cwd", cwd]);
    if let Some(fg) = chosen_fg.as_deref() {
        cmd.args(["--terminal-fg", fg]);
    }
    if let Some(bg) = chosen_bg.as_deref() {
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

    // #177 PR2 — remember the theme we actually launched with, so a
    // later spawn for this same terminal (the WS auto-revive when the
    // daemon dies, or a code path that passes
    // `SpawnDaemonOpts::default()`) reads `term.theme_fg/bg` and
    // re-derives identical argv via the priority logic above. Only
    // write when we have at least one non-NULL component AND it
    // differs from what's already on the row — avoids spurious
    // UPDATEs when the revive path re-stamps the same values
    // it just read.
    if (chosen_fg.is_some() || chosen_bg.is_some())
        && (chosen_fg.as_deref() != term.theme_fg.as_deref()
            || chosen_bg.as_deref() != term.theme_bg.as_deref())
    {
        if let Err(e) = repo
            .terminal_set_theme(&term.id, chosen_fg.as_deref(), chosen_bg.as_deref())
            .await
        {
            // Best-effort: the daemon is already up and themed. A failed
            // persist just means the NEXT auto-revive may launch without
            // theme — which is the pre-#177-PR2 status quo, not a regression.
            tracing::warn!(
                terminal_id = %term.id,
                error = %e,
                "failed to persist terminal theme; next respawn may launch without theme"
            );
        }
    }

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

    /// The real regression guard: an initial spawn with theme opts
    /// persists onto the row, and a subsequent spawn with
    /// `SpawnDaemonOpts::default()` (the WS auto-revive shape) still
    /// produces argv with the same `--terminal-fg` / `--terminal-bg`
    /// flags because the helper reads them from the row.
    ///
    /// Pre-#177 PR2 the auto-revive shim used `spawn_daemon_for`, which
    /// dropped the theme. PR #193 had to fix both spawn paths to thread
    /// theme through `SpawnDaemonOpts`; this test pins the persistence-
    /// based fix that closes the WS race observed in production
    /// (two daemons racing the socket: codex_cards-path themed, ws::
    /// terminal-path un-themed).
    #[tokio::test]
    async fn auto_revive_respawn_reads_theme_from_row() {
        let (repo, daemon, _tmp, term_id) = boot().await;

        // First spawn: initial path — caller supplies theme via opts.
        let term0 = repo.terminal_get(&term_id).await.unwrap().expect("row 0");
        spawn_daemon_with_parts(
            daemon.as_ref(),
            repo.as_ref(),
            &term0,
            "codex",
            "/",
            &serde_json::json!({}),
            SpawnDaemonOpts {
                terminal_fg: Some("216,219,226".into()),
                terminal_bg: Some("15,20,24".into()),
            },
        )
        .await
        .expect("initial spawn must succeed");

        // Recorder writes the sock path that was used; argv file lives
        // at `<sock>.argv`. The DaemonClient.sock_path renders
        // `<data_dir>/<term_id>.sock`.
        let sock_path = daemon.sock_path(&term_id);
        let argv_first = read_argv_lines(&sock_path.to_string_lossy());
        assert!(
            argv_first.iter().any(|a| a == "--terminal-fg"),
            "initial spawn must carry --terminal-fg; got: {argv_first:?}"
        );

        // Row must now carry the persisted theme.
        let term_after = repo
            .terminal_get(&term_id)
            .await
            .unwrap()
            .expect("row after first spawn");
        assert_eq!(term_after.theme_fg.as_deref(), Some("216,219,226"));
        assert_eq!(term_after.theme_bg.as_deref(), Some("15,20,24"));

        // Simulate the "daemon died" precondition the WS auto-revive
        // path runs into: the socket file is stale / gone. Cleaning
        // it up is what `spawn_daemon_with_parts` already does at
        // entry (it `remove_file`s a stale sock), but we also nuke the
        // argv sidecar so the second spawn's file is a fresh write.
        let _ = std::fs::remove_file(&sock_path);
        let _ = std::fs::remove_file(format!("{}.argv", sock_path.to_string_lossy()));

        // Second spawn: auto-revive shape — caller passes default
        // opts. The fix-under-test is that the helper falls back to
        // `term.theme_fg/bg` (now non-NULL after the first spawn) and
        // still stamps the flags.
        let term1 = repo.terminal_get(&term_id).await.unwrap().expect("row 1");
        spawn_daemon_with_parts(
            daemon.as_ref(),
            repo.as_ref(),
            &term1,
            "codex",
            "/",
            &serde_json::json!({}),
            SpawnDaemonOpts::default(),
        )
        .await
        .expect("auto-revive spawn must succeed");

        let argv_second = read_argv_lines(&sock_path.to_string_lossy());
        let pairs: Vec<(String, String)> = argv_second
            .windows(2)
            .map(|w| (w[0].clone(), w[1].clone()))
            .collect();
        assert!(
            pairs
                .iter()
                .any(|(k, v)| k == "--terminal-fg" && v == "216,219,226"),
            "auto-revive spawn must still carry --terminal-fg 216,219,226 \
             (read from row); got: {argv_second:?}"
        );
        assert!(
            pairs
                .iter()
                .any(|(k, v)| k == "--terminal-bg" && v == "15,20,24"),
            "auto-revive spawn must still carry --terminal-bg 15,20,24 \
             (read from row); got: {argv_second:?}"
        );
    }

    /// Companion to `auto_revive_respawn_reads_theme_from_row`: when
    /// the row has NULL `theme_fg/bg` (terminal-card path, pre-#177
    /// row, scripted caller) AND the opts are default, the spawn
    /// MUST NOT stamp any theme flag. Regression guard so a future
    /// refactor doesn't hard-code a fallback default and accidentally
    /// theme every terminal.
    #[tokio::test]
    async fn auto_revive_respawn_without_persisted_theme_omits_flags() {
        let (repo, daemon, _tmp, term_id) = boot().await;
        let term0 = repo.terminal_get(&term_id).await.unwrap().expect("row 0");
        assert!(
            term0.theme_fg.is_none() && term0.theme_bg.is_none(),
            "precondition: fresh row carries no theme"
        );
        spawn_daemon_with_parts(
            daemon.as_ref(),
            repo.as_ref(),
            &term0,
            "codex",
            "/",
            &serde_json::json!({}),
            SpawnDaemonOpts::default(),
        )
        .await
        .expect("spawn must succeed without theme");

        let sock_path = daemon.sock_path(&term_id);
        let argv = read_argv_lines(&sock_path.to_string_lossy());
        assert!(
            !argv.iter().any(|a| a == "--terminal-fg"),
            "no --terminal-fg must appear when both opts + row are NULL; got: {argv:?}"
        );
        assert!(
            !argv.iter().any(|a| a == "--terminal-bg"),
            "no --terminal-bg must appear when both opts + row are NULL; got: {argv:?}"
        );

        // Row must still be NULL after the no-theme spawn — the
        // persistence step only fires when there's something to
        // persist.
        let term_after = repo.terminal_get(&term_id).await.unwrap().unwrap();
        assert!(term_after.theme_fg.is_none() && term_after.theme_bg.is_none());
    }
}
