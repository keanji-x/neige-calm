//! Issue #177 (PR1 of split / closes #256) — `ws::terminal::
//! resolve_live_sock` is probe-only: it never auto-respawns a daemon
//! on the WS hot path. When the terminal row has no live daemon
//! (either `daemon_handle = NULL` or the socket file doesn't accept
//! connections), the upgrade path emits a clean
//! `Close(1000, "child-exited")` so the browser renders the
//! "process exited" overlay (with a Restart button) instead of a
//! 1006 disconnect.
//!
//! Pre-PR1 behavior: `resolve_live_sock` auto-respawned the daemon
//! with the row's persisted env, which could:
//!   * win a socket race against the initial themed spawn (no theme
//!     args on the respawn → codex composer painted in default colors),
//!   * spawn a Spec/Worker daemon without the per-card MCP env (which
//!     codex CLI 0.132 doesn't forward to the MCP shim, so the shim
//!     fails handshake).
//!
//! Post-PR1 behavior: `resolve_live_sock` is "probe; never respawn".
//! The one legitimate auto-revive case (calm-server restart while
//! daemons survived) is handled by `revive_orphans_on_boot` at
//! startup. The "row has no daemon socket at WS attach time" cases
//! all surface as `Close(1000, "child-exited")`; the row's
//! `daemon_handle` is NEVER rewritten by the WS attach path.

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
use calm_session::{
    ClientCapabilities, ClientMsg, DaemonMsg, InitialScrollback, PROTOCOL_VERSION, PtySize,
    RenderEncoding, RenderSnapshot, Role, read_frame, write_frame,
};
use tempfile::TempDir;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, UnixListener, UnixStream};
use tokio::sync::mpsc;
use tokio::time::sleep;
use uuid::Uuid;

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
        proc_supervisor_sock: None,
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
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });
    sleep(Duration::from_millis(50)).await;

    Boot {
        addr,
        repo,
        term_id: term.id,
        _tmp: tmp,
    }
}

fn client_hello(terminal_id: &str) -> ClientMsg {
    ClientMsg::ClientHello {
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
    }
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

fn spawn_handshake_listener(
    sock: &std::path::Path,
    terminal_id: String,
) -> mpsc::UnboundedReceiver<String> {
    let listener = UnixListener::bind(sock).expect("bind handshake listener");
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let terminal_id = terminal_id.clone();
            let tx = tx.clone();
            tokio::spawn(async move {
                let Ok(ClientMsg::ClientHello {
                    terminal_id: observed_terminal_id,
                    ..
                }) = read_frame::<ClientMsg, _>(&mut stream).await
                else {
                    return;
                };
                let _ = tx.send(observed_terminal_id);
                let _ = write_frame(&mut stream, &server_hello(&terminal_id)).await;
            });
        }
    });
    rx
}

fn spawn_garbage_listener(sock: &std::path::Path) {
    let listener = UnixListener::bind(sock).expect("bind stale protocol listener");
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let _ = stream.write_all(b"not-a-calm-session-frame").await;
            });
        }
    });
}

/// Regression guard for the probe-only refactor: a terminal row with
/// `daemon_handle = NULL` (spawn-site eager-write never landed —
/// `cmd.spawn()` itself failed, or some other rare path) must NOT
/// trigger a daemon spawn on WS upgrade. The upgrade succeeds (101)
/// and the server immediately emits `Close(1000, "child-exited")`
/// so the browser renders the "process exited" overlay; the row's
/// `daemon_handle` stays NULL afterwards (no auto-respawn).
#[tokio::test]
async fn ws_upgrade_without_live_daemon_emits_child_exited_close_and_does_not_spawn() {
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::Message as TMessage;

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

    // The browser-side attach. Upgrade succeeds (101) and the very
    // first frame on the WS must be Close(1000, "child-exited") so
    // the JS client surfaces the clean-exit overlay — no 1006.
    let url = format!("ws://{}/api/terminals/{}", boot.addr, boot.term_id);
    let (mut ws, _resp) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("upgrade must reach 101 even when daemon_handle is None");
    let first = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .expect("ws recv (close) timed out")
        .expect("ws closed without sending Close")
        .expect("ws error");
    match first {
        TMessage::Close(Some(cf)) => {
            assert_eq!(u16::from(cf.code), 1000, "expected 1000 normal close");
            assert_eq!(
                cf.reason.as_ref(),
                "child-exited",
                "expected `child-exited` reason text — pins the upgrade-time race fix",
            );
        }
        other => panic!("expected Close(1000, child-exited), got {other:?}"),
    }

    // Crucially: no daemon was spawned. The row's handle stays NULL,
    // and no socket file appears in the daemon data dir. Pre-PR1
    // this is where auto-respawn would have rewritten the handle.
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
/// but points at a stale socket (sock file doesn't accept connect).
/// This is the canonical "fast-exit child" path now that the spawn
/// site persists `daemon_handle` eagerly — the readiness poll could
/// have failed (daemon exited and unlinked before its socket bound),
/// or a long-lived daemon could have exited between rows ago. Either
/// way, the upgrade must surface `Close(1000, "child-exited")` and
/// MUST NOT respawn (the row's handle stays byte-for-byte the same).
#[tokio::test]
async fn ws_upgrade_with_stale_daemon_handle_emits_child_exited_close_and_does_not_respawn() {
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::Message as TMessage;

    let boot = boot_with_terminal_row().await;

    // Plant a stale handle: a path that doesn't accept connections.
    let stale_sock = boot._tmp.path().join("stale-not-bound.sock");
    let stale_sock_str = stale_sock.to_string_lossy().to_string();
    boot.repo
        .terminal_set_handle(&boot.term_id, Some(&stale_sock_str))
        .await
        .unwrap();

    let url = format!("ws://{}/api/terminals/{}", boot.addr, boot.term_id);
    let (mut ws, _resp) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("upgrade must reach 101 — server should emit Close, not 500");
    let first = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .expect("ws recv (close) timed out")
        .expect("ws closed without sending Close")
        .expect("ws error");
    match first {
        TMessage::Close(Some(cf)) => {
            assert_eq!(u16::from(cf.code), 1000, "expected 1000 normal close");
            assert_eq!(
                cf.reason.as_ref(),
                "child-exited",
                "expected `child-exited` reason text",
            );
        }
        other => panic!("expected Close(1000, child-exited), got {other:?}"),
    }

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

/// #337 — a daemon handle is live only if the socket completes the
/// calm-session handshake. This mock daemon answers the resolver's probe
/// with `ServerHello`, then answers the real pump attach the same way; the
/// browser sees the forwarded `ServerHello` instead of the child-exited
/// close.
#[tokio::test]
async fn ws_upgrade_with_handshake_live_daemon_enters_alive_path() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message as TMessage;

    let boot = boot_with_terminal_row().await;

    let live_sock = boot._tmp.path().join("live-protocol.sock");
    let mut observed_hellos = spawn_handshake_listener(&live_sock, boot.term_id.clone());
    let live_sock_str = live_sock.to_string_lossy().to_string();
    boot.repo
        .terminal_set_handle(&boot.term_id, Some(&live_sock_str))
        .await
        .unwrap();

    let url = format!("ws://{}/api/terminals/{}", boot.addr, boot.term_id);
    let (mut ws, _resp) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("upgrade must reach 101 for a live daemon");
    ws.send(TMessage::Text(
        serde_json::to_string(&client_hello(&boot.term_id)).expect("serialize ClientHello"),
    ))
    .await
    .expect("send ClientHello");

    let first = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .expect("ws recv (ServerHello) timed out")
        .expect("ws closed before ServerHello")
        .expect("ws error");
    match first {
        TMessage::Text(t) => {
            let parsed: DaemonMsg =
                serde_json::from_str(&t).unwrap_or_else(|e| panic!("bad JSON {t}: {e}"));
            assert!(
                matches!(parsed, DaemonMsg::ServerHello { .. }),
                "expected forwarded ServerHello, got {parsed:?}"
            );
        }
        other => panic!("expected Text(ServerHello), got {other:?}"),
    }

    let expected_hyphenated = Uuid::parse_str(&boot.term_id)
        .expect("terminal id should be UUID parseable")
        .to_string();
    assert!(
        expected_hyphenated.contains('-'),
        "regression guard must assert the hyphenated terminal id form"
    );
    let observed_probe_id = observed_hellos
        .recv()
        .await
        .expect("mock daemon should observe resolver probe ClientHello");
    assert_eq!(
        observed_probe_id, expected_hyphenated,
        "resolver probe must send the daemon-facing hyphenated terminal_id"
    );

    let post = boot
        .repo
        .terminal_get(&boot.term_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        post.daemon_handle.as_deref(),
        Some(live_sock_str.as_str()),
        "live protocol probe must not rewrite daemon handle"
    );
}

/// #337 — an accepting socket that does not speak calm-session is stale.
/// The old bare-connect resolver would have taken the `Alive` path; the
/// handshake probe returns the existing child-exited close and still reads
/// the `.exit` sidecar so the browser gets the real code.
#[tokio::test]
async fn ws_upgrade_with_stale_protocol_socket_emits_child_exited_close_with_exit_code() {
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::Message as TMessage;

    let boot = boot_with_terminal_row().await;

    let stale_sock = boot._tmp.path().join("stale-protocol.sock");
    spawn_garbage_listener(&stale_sock);
    let stale_sock_str = stale_sock.to_string_lossy().to_string();
    boot.repo
        .terminal_set_handle(&boot.term_id, Some(&stale_sock_str))
        .await
        .unwrap();
    std::fs::write(
        format!("{stale_sock_str}.exit"),
        r#"{"code":7,"signal_killed":false}"#,
    )
    .expect("write sidecar");

    assert!(
        UnixStream::connect(&stale_sock).await.is_ok(),
        "precondition: old bare-connect probe would have misclassified this stale socket as live",
    );

    let url = format!("ws://{}/api/terminals/{}", boot.addr, boot.term_id);
    let (mut ws, _resp) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("upgrade must reach 101 — server should emit Close, not 500");

    let first = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .expect("ws recv (TerminalExited) timed out")
        .expect("ws closed before TerminalExited")
        .expect("ws error");
    match first {
        TMessage::Text(t) => {
            let parsed: serde_json::Value = serde_json::from_str(&t)
                .unwrap_or_else(|e| panic!("parsing JSON failed for {t}: {e}"));
            let exited = parsed
                .get("TerminalExited")
                .unwrap_or_else(|| panic!("expected `TerminalExited` envelope, got {parsed}"));
            assert_eq!(
                exited.get("code").and_then(|v| v.as_i64()),
                Some(7),
                "expected TerminalExited.code == 7, got {parsed}"
            );
        }
        other => panic!("expected Text(TerminalExited), got {other:?}"),
    }

    let close = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .expect("ws recv (close) timed out")
        .expect("ws closed without sending Close")
        .expect("ws error");
    match close {
        TMessage::Close(Some(cf)) => {
            assert_eq!(u16::from(cf.code), 1000);
            assert_eq!(cf.reason.as_ref(), "child-exited");
        }
        other => panic!("expected Close(1000, child-exited), got {other:?}"),
    }

    let post = boot
        .repo
        .terminal_get(&boot.term_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(post.exit_code, Some(7));
    assert_eq!(
        post.daemon_handle.as_deref(),
        Some(stale_sock_str.as_str()),
        "WS probe-only path must not respawn or rewrite the stale handle"
    );
}

/// #306 — companion to the stale-handle path: when the daemon DID get
/// a chance to write the `<sock>.exit` sidecar before exiting (the
/// common case for a clean child exit — the daemon's `spawn_child_waiter`
/// writes the file before broadcasting its TerminalExited frame), the
/// kernel must read the sidecar at WS-upgrade time and persist
/// `exit_code` + `signal_killed` on the terminal row. The frontend's
/// terminal-card builtin then reads those columns off the REST DTO and
/// seeds the header badge before the WS even attaches.
#[tokio::test]
async fn ws_upgrade_reads_exit_sidecar_and_persists_exit_code() {
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::Message as TMessage;

    let boot = boot_with_terminal_row().await;

    // Plant a stale handle + a sidecar file at `<handle>.exit`. The
    // handle file itself doesn't exist (and that's what
    // `resolve_live_sock`'s probe fails on); the sidecar is at the
    // canonical `<sock>.exit` path and carries `{"code": 0,
    // "signal_killed": false}` — the shape the daemon's
    // `spawn_child_waiter` writes on a `printf done`-style clean
    // exit.
    let stale_sock = boot._tmp.path().join("stale-with-sidecar.sock");
    let stale_sock_str = stale_sock.to_string_lossy().to_string();
    boot.repo
        .terminal_set_handle(&boot.term_id, Some(&stale_sock_str))
        .await
        .unwrap();
    std::fs::write(
        format!("{stale_sock_str}.exit"),
        r#"{"code":0,"signal_killed":false}"#,
    )
    .expect("write sidecar");

    // Precondition: row carries no exit info yet.
    let pre = boot
        .repo
        .terminal_get(&boot.term_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(pre.exit_code, None);
    assert!(!pre.signal_killed);

    let url = format!("ws://{}/api/terminals/{}", boot.addr, boot.term_id);
    let (mut ws, _resp) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("upgrade must reach 101");

    // First frame: JSON `TerminalExited { code: Some(0), .. }` so the
    // browser's live exit-badge state picks up the real code without
    // waiting on the REST seed (which races the WS upgrade and may
    // read the row before `persist_exit_sidecar` writes). The frame
    // shape matches the in-pump path so the client's
    // `'TerminalExited' in msg` branch handles it.
    let first = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .expect("ws recv (text) timed out")
        .expect("ws closed before sending TerminalExited")
        .expect("ws error");
    match first {
        TMessage::Text(t) => {
            let parsed: serde_json::Value = serde_json::from_str(&t)
                .unwrap_or_else(|e| panic!("parsing JSON failed for {t}: {e}"));
            let exited = parsed
                .get("TerminalExited")
                .unwrap_or_else(|| panic!("expected `TerminalExited` envelope, got {parsed}"));
            assert_eq!(
                exited.get("code").and_then(|v| v.as_i64()),
                Some(0),
                "expected TerminalExited.code == 0, got {parsed}"
            );
        }
        other => panic!("expected Text(TerminalExited), got {other:?}"),
    }

    // Second frame: the existing `Close(1000, "child-exited")`. The
    // pump path and the upgrade-time path now share the
    // JSON-then-Close shape.
    let close = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .expect("ws recv (close) timed out")
        .expect("ws closed without sending Close")
        .expect("ws error");
    match close {
        TMessage::Close(Some(cf)) => {
            assert_eq!(u16::from(cf.code), 1000);
            assert_eq!(cf.reason.as_ref(), "child-exited");
        }
        other => panic!("expected Close(1000, child-exited), got {other:?}"),
    }

    // Post: the row now reflects the sidecar's payload. This is the
    // load-bearing fix for #306: a refreshed page (or any subsequent
    // REST poll of the terminal row) returns `exit_code = Some(0),
    // signal_killed = false` so the frontend can render the badge
    // immediately, without waiting for the WS attach or the JSON
    // `TerminalExited` frame.
    let post = boot
        .repo
        .terminal_get(&boot.term_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        post.exit_code,
        Some(0),
        "WS upgrade must persist exit_code from `.exit` sidecar"
    );
    assert!(!post.signal_killed);
}

/// #306 — SIGKILL'd daemon case: stale handle, NO sidecar on disk
/// (the daemon never reached its `child.wait()` write site). The
/// kernel surfaces `Close(1000, "child-exited")` (the v1 conflation —
/// v2 will distinguish DaemonLost) but MUST NOT write garbage onto
/// the row: `exit_code` stays NULL, `signal_killed` stays false.
#[tokio::test]
async fn ws_upgrade_with_stale_handle_and_no_sidecar_leaves_exit_code_null() {
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::Message as TMessage;

    let boot = boot_with_terminal_row().await;

    let stale_sock = boot._tmp.path().join("stale-no-sidecar.sock");
    let stale_sock_str = stale_sock.to_string_lossy().to_string();
    boot.repo
        .terminal_set_handle(&boot.term_id, Some(&stale_sock_str))
        .await
        .unwrap();
    // Belt-and-braces: assert there's no leftover sidecar from a
    // prior test run (tempdir-per-boot makes this trivially true,
    // but explicit > implicit on regression guards).
    assert!(!std::path::Path::new(&format!("{stale_sock_str}.exit")).exists());

    let url = format!("ws://{}/api/terminals/{}", boot.addr, boot.term_id);
    let (mut ws, _resp) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("upgrade must reach 101");
    let _ = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .expect("ws recv (close) timed out")
        .expect("ws closed without sending Close")
        .expect("ws error");
    // Drop the WS; the row read below is what we actually care about.
    let _ = ws;
    let _ = TMessage::Text("".into());

    let post = boot
        .repo
        .terminal_get(&boot.term_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        post.exit_code, None,
        "absent sidecar must leave exit_code NULL (DaemonLost shape)"
    );
    assert!(
        !post.signal_killed,
        "absent sidecar must leave signal_killed false"
    );
}
