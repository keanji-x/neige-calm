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
use std::process::Stdio;
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
use calm_session::{
    ClientMsg, DaemonMsg, PROTOCOL_VERSION, PtySize, RenderEncoding, RenderSnapshot, Role,
    read_frame, write_frame,
};
use tempfile::TempDir;
use tokio::net::{UnixListener, UnixStream};
use tokio::process::{Child, Command};
use uuid::Uuid;

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

fn locate_wrong_protocol_daemon_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_wrong-protocol-daemon"))
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
            cwd: String::new(),
            attach_folder: false,
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

fn server_hello(terminal_id: &str) -> DaemonMsg {
    let terminal_id = Uuid::parse_str(terminal_id)
        .map(|uuid| uuid.to_string())
        .unwrap_or_else(|_| terminal_id.to_string());
    DaemonMsg::ServerHello {
        protocol_version: PROTOCOL_VERSION,
        terminal_id,
        session_id: Uuid::new_v4(),
        client_role: Role::Owner,
        owner_client_id: Some(Uuid::new_v4()),
        pty_size: PtySize {
            cols: 80,
            rows: 24,
            pixel_width: None,
            pixel_height: None,
        },
        pty_seq_head: 0,
        pty_seq_tail: 0,
        render_rev: 0,
        snapshot: RenderSnapshot {
            render_rev: 0,
            pty_seq: 0,
            cols: 80,
            rows: 24,
            encoding: RenderEncoding::Vt,
            data: Vec::new(),
            scrollback: None,
        },
        history_gap: None,
        is_child_ready: false,
    }
}

fn spawn_handshake_listener(sock: &std::path::Path, terminal_id: String) {
    let listener = UnixListener::bind(sock).expect("bind handshake listener");
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let terminal_id = terminal_id.clone();
            tokio::spawn(async move {
                let Ok(ClientMsg::ClientHello { .. }) =
                    read_frame::<ClientMsg, _>(&mut stream).await
                else {
                    return;
                };
                let _ = write_frame(&mut stream, &server_hello(&terminal_id)).await;
            });
        }
    });
}

async fn spawn_garbage_daemon_process(sock: &std::path::Path) -> Child {
    let mut child = Command::new(locate_wrong_protocol_daemon_bin())
        .arg("--sock")
        .arg(sock)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn garbage protocol daemon");

    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(2) {
        if UnixStream::connect(sock).await.is_ok() {
            return child;
        }
        if let Some(status) = child.try_wait().expect("poll garbage daemon") {
            panic!("garbage protocol daemon exited before binding socket: {status}");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let _ = child.kill().await;
    let _ = child.wait().await;
    panic!("garbage protocol daemon did not bind socket {sock:?}");
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
    let mut sentinel = Command::new("sleep")
        .arg("60")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn sentinel process");
    let sentinel_pid = sentinel.id().expect("sentinel pid");
    fx.repo
        .terminal_set_pid(&term_id, Some(sentinel_pid))
        .await
        .unwrap();

    // Confirm the precondition: socket really doesn't accept.
    assert!(
        UnixStream::connect(&stale_sock).await.is_err(),
        "precondition: stale sock must not accept connections"
    );

    // Sweep.
    calm_server::revive_orphans_on_boot(&fx.state).await;
    assert!(
        sentinel.try_wait().expect("poll sentinel").is_none(),
        "Unreachable probe path must not signal the persisted pid"
    );
    let _ = sentinel.kill().await;
    let _ = sentinel.wait().await;

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

/// Regression guard for #337: a socket can accept `connect(2)` and still
/// fail the calm-session protocol. The old boot sweep would have counted
/// this row alive; the handshake probe must classify it stale and respawn.
#[tokio::test]
async fn revive_orphans_on_boot_respawns_stale_protocol_socket() {
    let fx = fixture().await;
    let term_id = seed_terminal_row(fx.repo.as_ref()).await;

    let stale_sock = fx._tmp.path().join("stale-protocol.sock");
    let mut stale_child = spawn_garbage_daemon_process(&stale_sock).await;
    let stale_pid = stale_child.id().expect("garbage daemon pid");
    let stale_sock_str = stale_sock.to_string_lossy().to_string();
    fx.repo
        .terminal_set_handle(&term_id, Some(&stale_sock_str))
        .await
        .unwrap();
    fx.repo
        .terminal_set_pid(&term_id, Some(stale_pid))
        .await
        .unwrap();

    assert!(
        UnixStream::connect(&stale_sock).await.is_ok(),
        "precondition: old bare-connect probe would have misclassified this stale socket as live",
    );

    calm_server::revive_orphans_on_boot(&fx.state).await;

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
        "handshake failure must replace the stale protocol socket"
    );
    assert!(
        UnixStream::connect(&new_handle).await.is_ok(),
        "post-sweep socket {new_handle:?} should accept connections"
    );
    tokio::time::sleep(Duration::from_millis(350)).await;
    assert!(
        std::path::Path::new(&new_handle).exists(),
        "old stale daemon's delayed unlink must not remove fresh socket {new_handle:?}"
    );
    assert!(
        UnixStream::connect(&new_handle).await.is_ok(),
        "fresh socket {new_handle:?} should remain connectable after old daemon shutdown cleanup"
    );

    let old_status = tokio::time::timeout(Duration::from_secs(2), stale_child.wait())
        .await
        .expect("old stale daemon should be reaped")
        .expect("wait old stale daemon");
    assert!(
        old_status.success(),
        "handshake-failed accepting daemon pid {stale_pid} should handle SIGTERM and exit cleanly before respawn; got {old_status:?}"
    );
}

/// A row whose `daemon_handle` completes the calm-session handshake must
/// NOT be respawned — the sweep should be a no-op for live daemons. We
/// point the row at a socket file we bind ourselves and answer
/// `ClientHello` with `ServerHello`, then assert the row's handle is
/// byte-for-byte unchanged after the sweep.
#[tokio::test]
async fn revive_orphans_on_boot_skips_live_daemons() {
    let fx = fixture().await;
    let term_id = seed_terminal_row(fx.repo.as_ref()).await;

    // Bind a live unix socket on a path of our choosing; this stands
    // in for a still-alive daemon.
    let live_sock = fx._tmp.path().join("live.sock");
    spawn_handshake_listener(&live_sock, term_id.clone());
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
