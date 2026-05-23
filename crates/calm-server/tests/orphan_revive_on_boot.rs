//! Issue #177 (PR1 of split / closes #256) — `revive_orphans_on_boot`
//! re-spawns the `calm-session-daemon` for every terminal row whose
//! persisted socket is unreachable. This replaces the WS-handler-side
//! auto-revive (which raced themed spawns with un-themed ones); the
//! boot-time sweep is the ONLY kernel-internal auto-revive path now.
//!
//! Test taxonomy:
//!   * `revive_orphans_on_boot_respawns_unreachable_daemon` — a row
//!     with `daemon_handle = Some(<stale sock>)` whose socket file
//!     doesn't accept connections triggers a respawn; post-sweep the
//!     row carries a fresh socket and the file exists on disk.
//!   * `revive_orphans_on_boot_skips_live_daemons` — a row whose
//!     daemon is actually responsive is left alone (no respawn, no
//!     handle rewrite).
//!   * `revive_orphans_on_boot_skips_rows_without_handle` — rows that
//!     never spawned (`daemon_handle = NULL`) are ignored; the sweep
//!     only touches rows that *thought* they had a live daemon.
//!
//! All tests use the real `calm-session-daemon` binary, mirroring
//! `wave_create_sync_daemon.rs` / `codex_card_endpoint.rs`.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::model::{CardRole, NewCard, NewCove, NewTerminal, NewWave};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_server::wave_cove_cache::WaveCoveCache;
use tempfile::TempDir;
use tokio::net::UnixStream;

/// Locate the workspace-built `calm-session-daemon` binary (same
/// trick the codex-card endpoint tests use).
fn locate_daemon_bin() -> PathBuf {
    let mut p = std::env::current_exe().expect("current_exe");
    p.pop();
    p.pop();
    p.push("calm-session-daemon");
    assert!(
        p.exists(),
        "calm-session-daemon not found at {p:?}; run \
         `cargo build -p calm-session --bin calm-session-daemon` first",
    );
    p
}

struct Fixture {
    state: AppState,
    repo: Arc<dyn Repo>,
    _tmp: TempDir,
}

async fn fixture() -> Fixture {
    let tmp = TempDir::new().expect("tempdir for daemon sockets");
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let daemon = Arc::new(DaemonClient {
        data_dir: tmp.path().to_path_buf(),
        session_daemon_bin: locate_daemon_bin(),
    });
    let events = EventBus::new();
    let card_role_cache = CardRoleCache::new();
    let wave_cove_cache = WaveCoveCache::new();
    repo.seed_wave_cove_cache(&wave_cove_cache).await.unwrap();
    let state = AppState::from_parts(
        repo.clone(),
        events,
        daemon,
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data-orphan-revive"),
            Vec::new(),
            EventBus::new(),
            card_role_cache.clone(),
            wave_cove_cache.clone(),
        )),
        Arc::new(CodexClient::new_stub()),
        Some(card_role_cache),
        Some(wave_cove_cache),
    );
    Fixture {
        state,
        repo,
        _tmp: tmp,
    }
}

/// Mint a `cove → wave → card → terminal` chain via the repo (no
/// route layer, no daemon spawn). Returns the terminal id so the
/// caller can manipulate its `daemon_handle` before invoking the
/// sweep.
async fn seed_terminal_row(repo: &dyn Repo) -> String {
    let cove = repo
        .cove_create(NewCove {
            name: "orphan-revive-test".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id,
            title: "orphan-revive-test".into(),
            sort: None,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let card = repo
        .card_create(NewCard {
            wave_id: wave.id,
            kind: "terminal".into(),
            sort: None,
            payload: serde_json::json!({}),
        })
        .await
        .unwrap();
    let _ = CardRole::Plain; // role-cache import side-effect
    let term = repo
        .terminal_create(NewTerminal {
            card_id: card.id,
            program: "/bin/sh".into(),
            cwd: "/tmp".into(),
            env: serde_json::json!({}),
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    term.id
}

/// Regression guard: a terminal row carrying a stale `daemon_handle`
/// (path that doesn't accept connections — common when calm-server
/// restarts and the old daemon's socket file is lingering on disk
/// but its process is gone) gets re-spawned by the boot sweep. After
/// the sweep, `daemon_handle` points at a fresh path whose socket file
/// is reachable.
#[tokio::test]
async fn revive_orphans_on_boot_respawns_unreachable_daemon() {
    let fx = fixture().await;
    let term_id = seed_terminal_row(fx.repo.as_ref()).await;

    // Plant a stale handle: a path inside the daemon data dir that
    // doesn't exist. The sweep's connect-probe must fail and trigger
    // the respawn branch.
    let stale_sock = fx._tmp.path().join("does-not-exist.sock");
    let stale_sock_str = stale_sock.to_string_lossy().to_string();
    fx.repo
        .terminal_set_handle(&term_id, Some(&stale_sock_str))
        .await
        .unwrap();

    // Confirm the precondition: socket really doesn't accept.
    assert!(
        UnixStream::connect(&stale_sock).await.is_err(),
        "precondition: stale sock must not accept connections"
    );

    // Sweep.
    calm_server::revive_orphans_on_boot(&fx.state).await;

    // Post-sweep: row has a non-stale handle and the new socket
    // accepts. Poll briefly because the sweep spawns the daemon
    // synchronously but the daemon's socket-bind is post-exec; the
    // helper's wait-for-socket loop runs inside the sweep, so by
    // the time the sweep returns the handle should already be live.
    let post = fx
        .repo
        .terminal_get(&term_id)
        .await
        .unwrap()
        .expect("row after sweep");
    let new_handle = post
        .daemon_handle
        .expect("daemon_handle must be set after sweep");
    assert_ne!(
        new_handle, stale_sock_str,
        "sweep must replace the stale handle, not preserve it"
    );
    let start = Instant::now();
    let mut ok = false;
    while start.elapsed() < Duration::from_secs(2) {
        if UnixStream::connect(&new_handle).await.is_ok() {
            ok = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        ok,
        "post-sweep socket {new_handle:?} did not accept connections"
    );
}

/// A row whose `daemon_handle` is reachable must NOT be respawned —
/// the sweep should be a no-op for live daemons. We point the row at
/// a socket file we bind ourselves (so the connect-probe succeeds)
/// and assert the row's handle is byte-for-byte unchanged after the
/// sweep.
#[tokio::test]
async fn revive_orphans_on_boot_skips_live_daemons() {
    let fx = fixture().await;
    let term_id = seed_terminal_row(fx.repo.as_ref()).await;

    // Bind a live unix socket on a path of our choosing; this stands
    // in for a still-alive daemon.
    let live_sock = fx._tmp.path().join("live.sock");
    let _listener = tokio::net::UnixListener::bind(&live_sock).expect("bind decoy live sock");
    let live_sock_str = live_sock.to_string_lossy().to_string();
    fx.repo
        .terminal_set_handle(&term_id, Some(&live_sock_str))
        .await
        .unwrap();

    // Confirm probe sees it live.
    assert!(
        UnixStream::connect(&live_sock).await.is_ok(),
        "precondition: live sock must accept connections"
    );

    // Sweep.
    calm_server::revive_orphans_on_boot(&fx.state).await;

    // Post-sweep: handle is identical (no respawn).
    let post = fx
        .repo
        .terminal_get(&term_id)
        .await
        .unwrap()
        .expect("row after sweep");
    assert_eq!(
        post.daemon_handle.as_deref(),
        Some(live_sock_str.as_str()),
        "sweep must NOT touch a row whose daemon socket is reachable",
    );
}

/// A row with `daemon_handle = NULL` (never spawned, or already
/// cleared) must be skipped — the sweep's input filter is "rows that
/// *think* they have a daemon", and an unspawned row doesn't qualify.
/// Regression guard against over-eager respawning of freshly-created
/// rows that haven't yet had their initial spawn complete.
#[tokio::test]
async fn revive_orphans_on_boot_skips_rows_without_handle() {
    let fx = fixture().await;
    let term_id = seed_terminal_row(fx.repo.as_ref()).await;

    // No daemon_handle set — default state after `terminal_create`.
    let pre = fx.repo.terminal_get(&term_id).await.unwrap().unwrap();
    assert!(
        pre.daemon_handle.is_none(),
        "precondition: fresh row has no daemon_handle",
    );

    // Sweep.
    calm_server::revive_orphans_on_boot(&fx.state).await;

    // Post-sweep: still no handle.
    let post = fx.repo.terminal_get(&term_id).await.unwrap().unwrap();
    assert!(
        post.daemon_handle.is_none(),
        "sweep must NOT spawn for rows that never had a daemon; got {:?}",
        post.daemon_handle,
    );
}
