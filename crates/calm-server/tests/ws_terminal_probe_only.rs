//! Issue #177 (PR1 of split / closes #256) — `ws::terminal::
//! resolve_live_sock` is probe-only. When the terminal row has no
//! live daemon (either `daemon_handle = NULL` or the socket file
//! doesn't accept connections), the WS upgrade path must return an
//! error and NOT silently respawn the daemon. The browser's existing
//! "Reconnect" UI surfaces the failure.
//!
//! Pre-PR1 behavior: `resolve_live_sock` auto-respawned the daemon
//! with the row's persisted env, which could:
//!   * win a socket race against the initial themed spawn (no theme
//!     args on the respawn → codex composer painted in default colors),
//!   * spawn a Spec/Worker daemon without the per-card MCP env (which
//!     codex CLI 0.132 doesn't forward to the MCP shim, so the shim
//!     fails handshake).
//!
//! Post-PR1 behavior: `resolve_live_sock` is "probe and 500". The one
//! legitimate auto-revive case (calm-server restart while daemons
//! survived) is handled by `revive_orphans_on_boot` at startup; the
//! WS hot path stays cold.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::model::{NewCard, NewCove, NewTerminal, NewWave};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_server::ws;
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::time::sleep;

/// Locate the workspace-built `calm-session-daemon` binary — used here
/// only as a sentinel path; the probe-only assertion runs entirely
/// before any spawn would happen.
fn locate_daemon_bin() -> PathBuf {
    let mut p = std::env::current_exe().expect("current_exe");
    p.pop();
    p.pop();
    p.push("calm-session-daemon");
    p
}

struct Boot {
    addr: std::net::SocketAddr,
    repo: Arc<dyn Repo>,
    term_id: String,
    _tmp: TempDir,
}

async fn boot_with_terminal_row() -> Boot {
    let tmp = TempDir::new().expect("tempdir");
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let cove = repo
        .cove_create(NewCove {
            name: "probe-only-test".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id,
            title: "probe-only-test".into(),
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

    let daemon = Arc::new(DaemonClient {
        data_dir: tmp.path().to_path_buf(),
        session_daemon_bin: locate_daemon_bin(),
    });
    let state = AppState::from_parts(
        repo.clone(),
        EventBus::new(),
        daemon,
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data-probe-only"),
            Vec::new(),
            EventBus::new(),
            calm_server::card_role_cache::CardRoleCache::new(),
            calm_server::wave_cove_cache::WaveCoveCache::new(),
        )),
        Arc::new(CodexClient::new_stub()),
        None,
        None,
    );

    let app = axum::Router::new().merge(ws::router()).with_state(state);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    sleep(Duration::from_millis(50)).await;

    Boot {
        addr,
        repo,
        term_id: term.id,
        _tmp: tmp,
    }
}

/// Regression guard for the probe-only refactor: a terminal row with
/// `daemon_handle = NULL` (never spawned) must NOT trigger a daemon
/// spawn on WS upgrade. The handshake must fail at the upgrade step
/// (no 101) and the row's `daemon_handle` stays NULL after.
#[tokio::test]
async fn ws_upgrade_without_live_daemon_returns_error_and_does_not_spawn() {
    let boot = boot_with_terminal_row().await;

    let pre = boot
        .repo
        .terminal_get(&boot.term_id)
        .await
        .unwrap()
        .unwrap();
    assert!(
        pre.daemon_handle.is_none(),
        "precondition: row has no daemon handle yet",
    );

    // Try to upgrade — this is what the browser sends to attach.
    let url = format!("ws://{}/ws/terminal/{}", boot.addr, boot.term_id);
    let connect = tokio_tungstenite::connect_async(&url).await;

    // Upgrade must fail. Pre-PR1 this would succeed because
    // `resolve_live_sock` would respawn a daemon and the WS would
    // attach to the freshly-spawned socket.
    match connect {
        Ok(_) => panic!(
            "ws upgrade succeeded for a row with no live daemon; \
             probe-only `resolve_live_sock` regressed — auto-respawn returned",
        ),
        Err(e) => {
            // Either the server returns 500 (status-code error from
            // tungstenite) or the TCP connection lands but the handshake
            // doesn't reach 101 — both are acceptable signals that we
            // didn't open a WS attached to a freshly-spawned daemon.
            eprintln!("expected ws upgrade failure: {e}");
        }
    }

    // Crucially: no daemon was spawned. The row's handle stays NULL,
    // and no socket file appears in the daemon data dir.
    let post = boot
        .repo
        .terminal_get(&boot.term_id)
        .await
        .unwrap()
        .unwrap();
    assert!(
        post.daemon_handle.is_none(),
        "ws upgrade must NOT auto-spawn a daemon; got {:?}",
        post.daemon_handle,
    );
}

/// Companion to the test above: a row whose `daemon_handle` exists
/// but points at a stale socket (sock file doesn't accept connect)
/// also returns an error and does NOT respawn. Pre-PR1 this was the
/// race path — un-themed respawn could win the socket against an
/// initial themed spawn.
#[tokio::test]
async fn ws_upgrade_with_stale_daemon_handle_returns_error_and_does_not_respawn() {
    let boot = boot_with_terminal_row().await;

    // Plant a stale handle: a path that doesn't accept connections.
    let stale_sock = boot._tmp.path().join("stale-not-bound.sock");
    let stale_sock_str = stale_sock.to_string_lossy().to_string();
    boot.repo
        .terminal_set_handle(&boot.term_id, Some(&stale_sock_str))
        .await
        .unwrap();

    let url = format!("ws://{}/ws/terminal/{}", boot.addr, boot.term_id);
    let connect = tokio_tungstenite::connect_async(&url).await;
    assert!(
        connect.is_err(),
        "ws upgrade succeeded against a stale daemon handle; \
         probe-only resolve regressed",
    );

    // Post-attempt: the handle is byte-for-byte the same (no respawn
    // wrote a fresh path). Confirms the probe-only contract is intact:
    // the bad handle stays bad, the operator (or `revive_orphans_on_boot`
    // on next restart) is the one who clears it.
    let post = boot
        .repo
        .terminal_get(&boot.term_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        post.daemon_handle.as_deref(),
        Some(stale_sock_str.as_str()),
        "ws upgrade must NOT rewrite the daemon handle (no auto-respawn)",
    );
}
