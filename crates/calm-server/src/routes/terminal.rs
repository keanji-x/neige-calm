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

/// Spawn a `calm-session-daemon` for the given terminal row, wait for its
/// deterministic ready signal, and persist the socket path as the row's
/// `daemon_handle`. Used by `routes::terminal_cards::create_terminal_card`
/// (the atomic-create endpoint), the codex route's PTY spawn, and (when a
/// previously-spawned daemon has died) by the WS handler's auto-revive path.
pub(crate) async fn spawn_daemon_for(
    s: &AppState,
    term: &Terminal,
    program: &str,
    cwd: &str,
    env: &serde_json::Value,
) -> Result<()> {
    spawn_daemon_with_parts(s.daemon.as_ref(), s.repo.as_ref(), term, program, cwd, env).await
}

/// PR6 (#136) — lower-level seam over `spawn_daemon_for` that takes the
/// constituent `DaemonClient` + `&dyn RouteRepo` instead of the full
/// `AppState`. Used by the dispatcher (which doesn't own an `AppState` —
/// it's a kernel-internal worker that ships before AppState exists in
/// the boot order). Identical semantics to `spawn_daemon_for`; the
/// latter is now a one-line forwarder.
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

    // #177 PR2 — `term.theme_fg/_bg` are the single source of truth
    // for daemon OSC 10/11 reply colors (write-once at row create,
    // NOT NULL post-migration 0017). Thread them onto every daemon
    // spawn so the model can answer codex's startup probe with the
    // host theme's RGB before the first PTY chunk lands. The daemon
    // (PR2 commit 3) bails out fast if either flag is missing — the
    // NOT NULL row invariant should make that unreachable but the
    // belt-and-braces check protects against future kernel-side
    // regressions that forget to thread through this helper.
    let mut args = Vec::with_capacity(15);
    args.extend(["--id".to_string(), term.id.clone()]);
    args.extend(["--sock".to_string(), sock_str.clone()]);
    args.extend(["--terminal-fg".to_string(), term.theme_fg.clone()]);
    args.extend(["--terminal-bg".to_string(), term.theme_bg.clone()]);
    args.extend(["--cwd".to_string(), cwd.to_string()]);
    args.extend(["--ready-fd".to_string(), "0".to_string()]);
    args.push("--".to_string());
    args.extend(["/bin/sh".to_string(), "-c".to_string(), program.to_string()]);

    let mut envs = vec![
        ("TERM".to_string(), "xterm-256color".to_string()),
        ("COLORTERM".to_string(), "truecolor".to_string()),
    ];
    if let Some(map) = env.as_object() {
        for (k, v) in map {
            if let Some(val) = v.as_str() {
                envs.push((k.clone(), val.to_string()));
            }
        }
    }

    // pid + handle must be persisted BEFORE readiness completes:
    //   * the dispatcher's `rollback_orphan_worker` fast-exit-preserve
    //     discriminator reads `daemon_handle` to find `<sock>.exit`;
    //   * the partial-spawn reap path reads `pid` to SIGTERM a hung daemon.
    // Both run if readiness errors. We achieve this by reacting to the
    // supervisor's `Spawned` frame (which precedes readiness) in the
    // on_spawned callback below.
    let pid = crate::proc_supervisor::ensure_proc(
        daemon.proc_supervisor_sock.as_deref(),
        calm_session::control::EnsureProcRequest {
            proc_id: term.id.clone(),
            program: daemon.session_daemon_bin.display().to_string(),
            args,
            envs,
            cwd: cwd.to_string(),
            ready_timeout_ms: crate::proc_supervisor::DEFAULT_READY_TIMEOUT.as_millis() as u64,
        },
        |pid| {
            let term_id = term.id.clone();
            let sock_str = sock_str.clone();
            async move {
                tracing::info!(pid = ?pid, terminal_id = %term_id, "spawned calm-session-daemon");
                // Best-effort: a failed pid write degrades the sweeper's
                // SIGTERM lever but must not abort the spawn.
                if let Err(e) = repo.terminal_set_pid(&term_id, Some(pid)).await {
                    tracing::warn!(
                        terminal_id = %term_id,
                        pid = ?pid,
                        error = %e,
                        "failed to persist terminal pid; sweeper will fall back to socket-Kill only"
                    );
                }
                // Handle MUST persist before readiness completes: a one-shot
                // child (e.g. `printf done`) can exit + the daemon can
                // unlink its socket before any attach observes it, and the
                // dispatcher's discriminator reads this field to find the
                // `.exit` sidecar.
                repo.terminal_set_handle(&term_id, Some(&sock_str)).await
            }
        },
    )
    .await?;
    let _ = pid;
    Ok(())
}

#[cfg(test)]
mod tests {
    //! #177 PR2 — exercise the spawn-time theme plumbing.
    //!
    //! `spawn_daemon_with_parts` is `pub(crate)` so external integration
    //! tests can't reach it directly; we drive it from a unit-test
    //! module here. The `argv-recorder-daemon` fixture (see
    //! `crates/calm-server/tests/fixtures/argv-recorder-daemon/main.rs`)
    //! stands in for the real `calm-session-daemon`: writes the full
    //! argv it received to `<sock>.argv`, binds the socket, then writes
    //! `ready\n` to the inherited ready fd. Asserts the bytes that leave the
    //! spawn site without paying the cost of a real daemon.
    //!
    //! Recorder protocol:
    //!   * Kernel passes `--sock <path>` like the real daemon.
    //!   * Recorder writes `<path>.argv` (one line per argv element),
    //!     binds the unix socket, then writes `ready\n` to `--ready-fd`
    //!     so `spawn_daemon_with_parts` returns Ok.

    use super::*;
    use crate::db::prelude::*;
    use crate::db::sqlite::SqlxRepo;
    use crate::model::{Card, NewCard, NewCove, NewTerminal, NewWave};
    use crate::state::DaemonClient;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    /// `CARGO_BIN_EXE_argv-recorder-daemon` is only populated for the
    /// integration-test crate (target = test, kind = test). Unit tests
    /// inside `src/` don't get that env var — fall back to the same
    /// relative location Cargo emits the test bin to (`target/debug/`
    /// or `target/release/`). Resolves via `current_exe()`'s parent
    /// chain: the unit-test binary lives at
    /// `target/<profile>/deps/<name>-<hash>`; the recorder binary
    /// lives at `target/<profile>/argv-recorder-daemon`.
    fn locate_recorder_bin() -> PathBuf {
        locate_fixture_bin("argv-recorder-daemon")
    }

    fn locate_ready_exit_bin() -> PathBuf {
        locate_fixture_bin("ready-exit-daemon")
    }

    fn locate_fixture_bin(name: &str) -> PathBuf {
        let env_key = format!("CARGO_BIN_EXE_{name}");
        if let Ok(p) = std::env::var(env_key) {
            return PathBuf::from(p);
        }
        let me = std::env::current_exe().expect("current_exe");
        let target_profile = me
            .parent()
            .and_then(|p| p.parent())
            .expect("test bin parent");
        let candidate = target_profile.join(name);
        if candidate.exists() {
            return candidate;
        }
        panic!(
            "{name} binary not found at {candidate:?}; \
             build the workspace first (`cargo build --tests -p calm-server`)"
        );
    }

    /// Read the recorder's sidecar file written next to `<sock>.argv`.
    /// Recorder writes argv before binding, but we still poll briefly
    /// because some filesystems flush writes lazily under load.
    fn read_argv_lines(sock: &str) -> Vec<String> {
        let argv_path = format!("{sock}.argv");
        let start = Instant::now();
        loop {
            if let Ok(text) = std::fs::read_to_string(&argv_path)
                && !text.is_empty()
            {
                return text.lines().map(String::from).collect();
            }
            if start.elapsed() > Duration::from_secs(3) {
                panic!("argv file {argv_path:?} never appeared / stayed empty");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Mint the minimum DB surface needed to drive
    /// `spawn_daemon_with_parts`: in-memory sqlite + DaemonClient
    /// pointing at the recorder fixture + a card → terminal row pair
    /// with theme.
    async fn boot() -> (Arc<dyn Repo>, Arc<DaemonClient>, tempfile::TempDir, String) {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let repo: Arc<dyn Repo> = Arc::new(
            SqlxRepo::open("sqlite::memory:")
                .await
                .expect("open in-memory sqlite"),
        );
        let cove = repo
            .cove_create(NewCove {
                name: "spawn-argv-test".into(),
                color: "#000".into(),
                sort: None,
            })
            .await
            .unwrap();
        let wave = repo
            .wave_create(NewWave {
                cove_id: cove.id.clone(),
                title: "spawn-argv-test".into(),
                sort: None,
                cwd: String::new(),
                attach_folder: false,
                theme: crate::routes::theme::RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        let card: Card = repo
            .card_create(NewCard {
                wave_id: wave.id.clone(),
                kind: "codex".into(),
                sort: None,
                payload: serde_json::json!({}),
            })
            .await
            .unwrap();
        let term = repo
            .terminal_create(NewTerminal {
                card_id: card.id,
                program: "codex".into(),
                cwd: "/".into(),
                env: serde_json::json!({}),
                theme: crate::routes::theme::RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        let daemon = Arc::new(DaemonClient {
            data_dir: tmp.path().to_path_buf(),
            session_daemon_bin: locate_recorder_bin(),
            proc_supervisor_sock: None,
        });
        (repo, daemon, tmp, term.id)
    }

    /// Canonical test: a row created with `default_dark` produces
    /// daemon argv carrying matching `--terminal-fg` / `--terminal-bg`
    /// flags. Pinning the single read path means the dispatcher /
    /// atomic-create / WS auto-revive spawn sites all inherit the
    /// same coverage — they all go through `spawn_daemon_with_parts`.
    #[tokio::test]
    async fn spawn_threads_theme_from_row_onto_daemon_argv() {
        let (repo, daemon, _tmp, term_id) = boot().await;

        let term = repo.terminal_get(&term_id).await.unwrap().expect("row");
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
        // Pair up consecutive elements so the assertion sees
        // `(flag, value)` — argv-recorder writes one element per line,
        // which matches how `tokio::process::Command` spaces
        // `.args(["--flag", "value"])`.
        let pairs: Vec<(String, String)> = argv
            .windows(2)
            .map(|w| (w[0].clone(), w[1].clone()))
            .collect();
        assert!(
            pairs
                .iter()
                .any(|(k, v)| k == "--terminal-fg" && v == "216,219,226"),
            "spawn must carry --terminal-fg from row; got argv: {argv:?}"
        );
        assert!(
            pairs
                .iter()
                .any(|(k, v)| k == "--terminal-bg" && v == "15,20,24"),
            "spawn must carry --terminal-bg from row; got argv: {argv:?}"
        );
    }

    #[tokio::test]
    async fn spawn_treats_ready_then_fast_exit_as_ready() {
        let (repo, daemon, _tmp, term_id) = boot().await;
        let daemon = DaemonClient {
            data_dir: daemon.data_dir.clone(),
            session_daemon_bin: locate_ready_exit_bin(),
            proc_supervisor_sock: None,
        };
        let term = repo.terminal_get(&term_id).await.unwrap().expect("row");

        spawn_daemon_with_parts(
            &daemon,
            repo.as_ref(),
            &term,
            "true",
            "/",
            &serde_json::json!({}),
        )
        .await
        .expect("ready signal must win over immediate child exit");
    }

    /// Fast-exit race regression: when the spawned binary exits
    /// immediately without binding its socket (the user-visible
    /// `printf 'done\n'` reproducer in #267's followup), the readiness
    /// race inside `spawn_daemon_with_parts` returns Err — but the row's
    /// `daemon_handle` MUST already be set so that a subsequent WS
    /// attach can resolve to `LiveSock::ChildExited` and emit
    /// `Close(1000, "child-exited")` instead of falling into the
    /// "no daemon_handle" surfacing a 1006 to the browser.
    ///
    /// Drives the helper with `/bin/true` as `session_daemon_bin`:
    /// `cmd.spawn()` succeeds, the binary ignores all args + exits 0
    /// without binding the socket or writing ready, so the child-exit
    /// arm returns the "did not become ready" error. The
    /// invariant under test is that **the handle is persisted before
    /// the readiness check** — verifying it directly from the row
    /// after the failure returns.
    #[tokio::test]
    async fn spawn_persists_handle_even_when_daemon_exits_before_ready() {
        use crate::db::prelude::*;

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let repo: Arc<dyn Repo> = Arc::new(
            SqlxRepo::open("sqlite::memory:")
                .await
                .expect("open in-memory sqlite"),
        );
        let cove = repo
            .cove_create(NewCove {
                name: "fast-exit".into(),
                color: "#000".into(),
                sort: None,
            })
            .await
            .unwrap();
        let wave = repo
            .wave_create(NewWave {
                cove_id: cove.id.clone(),
                title: "fast-exit".into(),
                sort: None,
                cwd: String::new(),
                attach_folder: false,
                theme: crate::routes::theme::RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        let card: Card = repo
            .card_create(NewCard {
                wave_id: wave.id.clone(),
                kind: "terminal".into(),
                sort: None,
                payload: serde_json::json!({}),
            })
            .await
            .unwrap();
        let term = repo
            .terminal_create(NewTerminal {
                card_id: card.id,
                program: "true".into(),
                cwd: "/".into(),
                env: serde_json::json!({}),
                theme: crate::routes::theme::RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        // `/bin/true` (or `/usr/bin/true`) — accepts any args, exits 0,
        // never binds or writes ready. Drives the child-exit path.
        let true_bin = if std::path::Path::new("/bin/true").exists() {
            PathBuf::from("/bin/true")
        } else {
            PathBuf::from("/usr/bin/true")
        };
        let daemon = Arc::new(DaemonClient {
            data_dir: tmp.path().to_path_buf(),
            session_daemon_bin: true_bin,
            proc_supervisor_sock: None,
        });

        // Drive the spawn — must return Err (child exit before ready).
        let res = spawn_daemon_with_parts(
            daemon.as_ref(),
            repo.as_ref(),
            &term,
            "true",
            "/",
            &serde_json::json!({}),
        )
        .await;
        assert!(
            res.is_err(),
            "expected spawn to fail readiness, but got {res:?}",
        );

        // Phase 1 contract: handle is persisted BEFORE readiness via the
        // proc-supervisor's `Spawned` frame — the on_spawned callback in
        // `spawn_daemon_with_parts` writes pid + daemon_handle before the
        // readiness reply arrives. This preserves the pre-#388 ordering
        // the dispatcher's fast-exit-preserve discriminator and the
        // partial-spawn reap path both depend on: rollback reads
        // `daemon_handle` to locate `<sock>.exit` and reads `pid` to
        // SIGTERM hung daemons.
        let row = repo.terminal_get(&term.id).await.unwrap().expect("row");
        assert!(
            row.daemon_handle.is_some(),
            "spawn must persist daemon_handle even when readiness fails so the \
             dispatcher's rollback discriminator can find `<sock>.exit`. \
             row.daemon_handle: {:?}",
            row.daemon_handle,
        );
    }

    /// Sanity: `--id`, `--sock`, the theme flags, `--cwd`, and
    /// `--ready-fd` all
    /// appear on the same argv. Catches a `.args(...)` call that
    /// accidentally lands in a `cmd.env(...)` block.
    #[tokio::test]
    async fn spawn_argv_includes_required_kernel_flags() {
        let (repo, daemon, _tmp, term_id) = boot().await;
        let term = repo.terminal_get(&term_id).await.unwrap().expect("row");

        spawn_daemon_with_parts(
            daemon.as_ref(),
            repo.as_ref(),
            &term,
            "codex",
            "/tmp",
            &serde_json::json!({}),
        )
        .await
        .expect("spawn must succeed");

        let sock_path = daemon.sock_path(&term_id);
        let argv = read_argv_lines(&sock_path.to_string_lossy());
        for expected in &[
            "--id",
            "--sock",
            "--terminal-fg",
            "--terminal-bg",
            "--cwd",
            "--ready-fd",
        ] {
            assert!(
                argv.iter().any(|a| a == expected),
                "argv must carry {expected}; got argv: {argv:?}",
            );
        }
    }
}
