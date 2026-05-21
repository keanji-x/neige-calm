//! Wire-level e2e tests for the v2 terminal protocol in the WS↔daemon
//! bridge (`pump`). Mounts the public [`calm_server::ws::terminal::pump`]
//! function under a minimal axum router and drives it with one end of a
//! `tokio::io::duplex` pair playing the role of the daemon socket. No
//! `calm-session-daemon` subprocess is forked — every byte of the
//! JSON↔bincode bridge is exercised in-process.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::ws::WebSocketUpgrade;
use axum::routing::get;
use calm_server::ws::terminal::pump;
use calm_session::{
    ClientCapabilities, ClientMsg, DaemonMsg, InitialScrollback, PROTOCOL_VERSION, PtySize,
    RenderEncoding, RenderPatch, RenderSnapshot, read_frame, write_frame,
};
use futures_util::{SinkExt, StreamExt};
use tokio::io::DuplexStream;
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message as TMessage;
use uuid::Uuid;

const TID: &str = "ws-terminal-v2-test";

/// Big enough that the heartbeat arm never fires inside the test's
/// wall-clock window — same idiom as `pump_tests`.
fn long_window() -> (Duration, Duration) {
    (Duration::from_secs(10), Duration::from_secs(60))
}

/// Spin up a fresh axum app mounting [`pump`] on `/pump` with the given
/// duplex daemon side. Returns the bound socket address.
async fn boot(daemon_side: DuplexStream, terminal_id: &str) -> SocketAddr {
    let slot = Arc::new(Mutex::new(Some(daemon_side)));
    let tid = terminal_id.to_string();
    let (ping, pong) = long_window();
    let app = Router::new().route(
        "/pump",
        get(move |upgrade: WebSocketUpgrade| {
            let slot = slot.clone();
            let tid = tid.clone();
            async move {
                let daemon = slot.lock().await.take().expect("pump route called twice");
                // `pump` returns a `PumpOutcome` (post-#59) but
                // `on_upgrade` wants a `Future<Output = ()>`. This test
                // only cares about the v2 wire round-trip; the outcome
                // variant is exercised separately in `pump_tests`.
                upgrade.on_upgrade(move |socket| async move {
                    let _ = pump(socket, daemon, tid, ping, pong).await;
                })
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    addr
}

fn hello() -> ClientMsg {
    ClientMsg::ClientHello {
        protocol_version: PROTOCOL_VERSION,
        terminal_id: TID.to_string(),
        client_id: Uuid::new_v4(),
        desired_size: PtySize {
            cols: 132,
            rows: 50,
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

fn empty_snapshot() -> RenderSnapshot {
    RenderSnapshot {
        render_rev: 0,
        pty_seq: 0,
        cols: 132,
        rows: 50,
        encoding: RenderEncoding::Vt,
        data: Vec::new(),
        scrollback: None,
    }
}

/// Full happy-path round trip: WS client sends ClientHello → daemon side
/// reads it as a v2 bincode frame, replies with ServerHello → WS client
/// receives it as JSON → client sends Input → daemon reads it → daemon
/// pushes a RenderPatch → client receives it.
#[tokio::test]
async fn v2_round_trip_via_pump() {
    let (mut daemon_side, server_side) = tokio::io::duplex(16 * 1024);
    let addr = boot(server_side, TID).await;

    let url = format!("ws://{}/pump", addr);
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    // 1) WS client → daemon: ClientHello.
    ws.send(TMessage::Text(serde_json::to_string(&hello()).unwrap()))
        .await
        .unwrap();
    let got_hello: ClientMsg = tokio::time::timeout(
        Duration::from_secs(2),
        read_frame::<ClientMsg, _>(&mut daemon_side),
    )
    .await
    .expect("daemon-side hello read timed out")
    .expect("daemon-side hello decode failed");
    match got_hello {
        ClientMsg::ClientHello {
            protocol_version, ..
        } => {
            assert_eq!(protocol_version, PROTOCOL_VERSION);
        }
        other => panic!("expected ClientHello, got {other:?}"),
    }

    // 2) daemon → WS: ServerHello.
    let server_hello = DaemonMsg::ServerHello {
        protocol_version: PROTOCOL_VERSION,
        terminal_id: TID.to_string(),
        session_id: Uuid::new_v4(),
        client_role: calm_session::Role::Owner,
        owner_client_id: Some(Uuid::new_v4()),
        pty_size: PtySize {
            cols: 132,
            rows: 50,
            pixel_width: None,
            pixel_height: None,
        },
        pty_seq_head: 0,
        pty_seq_tail: 0,
        render_rev: 0,
        snapshot: empty_snapshot(),
        history_gap: None,
    };
    write_frame(&mut daemon_side, &server_hello).await.unwrap();
    let msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .expect("ws recv timed out")
        .expect("ws closed unexpectedly")
        .expect("ws error");
    let parsed: DaemonMsg = match msg {
        TMessage::Text(t) => serde_json::from_str(&t.to_string()).expect("server hello json"),
        other => panic!("expected text, got {other:?}"),
    };
    assert!(matches!(parsed, DaemonMsg::ServerHello { .. }));

    // 3) WS client → daemon: Input.
    let input = ClientMsg::Input(b"keystrokes".to_vec());
    ws.send(TMessage::Text(serde_json::to_string(&input).unwrap()))
        .await
        .unwrap();
    let got_input: ClientMsg = tokio::time::timeout(
        Duration::from_secs(2),
        read_frame::<ClientMsg, _>(&mut daemon_side),
    )
    .await
    .expect("daemon-side input read timed out")
    .expect("daemon-side input decode failed");
    match got_input {
        ClientMsg::Input(b) => assert_eq!(b, b"keystrokes"),
        other => panic!("expected Input, got {other:?}"),
    }

    // 4) Owner-driven ResizeCommit round-trip + ResizeApplied broadcast.
    let resize = ClientMsg::ResizeCommit {
        epoch: 1,
        cols: 100,
        rows: 30,
    };
    ws.send(TMessage::Text(serde_json::to_string(&resize).unwrap()))
        .await
        .unwrap();
    let got_resize: ClientMsg = tokio::time::timeout(
        Duration::from_secs(2),
        read_frame::<ClientMsg, _>(&mut daemon_side),
    )
    .await
    .expect("daemon-side resize read timed out")
    .expect("daemon-side resize decode failed");
    assert!(matches!(
        got_resize,
        ClientMsg::ResizeCommit {
            epoch: 1,
            cols: 100,
            rows: 30
        }
    ));

    // Stale-epoch ResizeCommit goes over the wire (the daemon is the one
    // that decides to drop it — the pump just shuttles bytes). Confirm
    // the wire layer doesn't care.
    let stale = ClientMsg::ResizeCommit {
        epoch: 0,
        cols: 1,
        rows: 1,
    };
    ws.send(TMessage::Text(serde_json::to_string(&stale).unwrap()))
        .await
        .unwrap();
    let got_stale: ClientMsg = tokio::time::timeout(
        Duration::from_secs(2),
        read_frame::<ClientMsg, _>(&mut daemon_side),
    )
    .await
    .expect("daemon-side stale read timed out")
    .expect("daemon-side stale decode failed");
    assert!(matches!(
        got_stale,
        ClientMsg::ResizeCommit { epoch: 0, .. }
    ));

    // 5) daemon → WS: RenderPatch broadcast lands as Text.
    let patch = DaemonMsg::RenderPatch(RenderPatch {
        render_rev: 1,
        prev_render_rev: 0,
        pty_seq: 1,
        encoding: RenderEncoding::Vt,
        data: b"hello world".to_vec(),
    });
    write_frame(&mut daemon_side, &patch).await.unwrap();
    let msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .expect("ws patch recv timed out")
        .expect("ws closed unexpectedly")
        .expect("ws error");
    let parsed: DaemonMsg = match msg {
        TMessage::Text(t) => serde_json::from_str(&t.to_string()).expect("patch json"),
        other => panic!("expected text, got {other:?}"),
    };
    match parsed {
        DaemonMsg::RenderPatch(p) => assert_eq!(p.data, b"hello world"),
        other => panic!("expected RenderPatch, got {other:?}"),
    }

    // 6) Owner Kill round-trip — daemon side must see it.
    ws.send(TMessage::Text(
        serde_json::to_string(&ClientMsg::Kill).unwrap(),
    ))
    .await
    .unwrap();
    let got_kill: ClientMsg = tokio::time::timeout(
        Duration::from_secs(2),
        read_frame::<ClientMsg, _>(&mut daemon_side),
    )
    .await
    .expect("daemon-side kill read timed out")
    .expect("daemon-side kill decode failed");
    assert!(matches!(got_kill, ClientMsg::Kill));
}

/// When the daemon emits `TerminalExited`, the pump forwards it as JSON
/// and then closes the WS — the stream drains to None.
#[tokio::test]
async fn pump_terminates_on_terminal_exited() {
    let (mut daemon_side, server_side) = tokio::io::duplex(8192);
    let addr = boot(server_side, TID).await;

    let url = format!("ws://{}/pump", addr);
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    write_frame(
        &mut daemon_side,
        &DaemonMsg::TerminalExited {
            code: Some(0),
            pty_seq: 42,
            render_rev: 42,
        },
    )
    .await
    .unwrap();
    drop(daemon_side);

    // 1) Text frame with TerminalExited.
    let msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .expect("ws recv timed out")
        .expect("ws closed before TerminalExited")
        .expect("ws error");
    let text = match msg {
        TMessage::Text(t) => t.to_string(),
        other => panic!("expected text, got {other:?}"),
    };
    let parsed: DaemonMsg = serde_json::from_str(&text).unwrap();
    assert!(matches!(
        parsed,
        DaemonMsg::TerminalExited {
            code: Some(0),
            pty_seq: 42,
            render_rev: 42
        }
    ));

    // 2) Close frame.
    let close = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .expect("ws recv (close) timed out")
        .expect("ws closed without Close frame")
        .expect("ws error");
    assert!(matches!(close, TMessage::Close(_)));

    // 3) Stream drains — pump returned.
    let end = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .expect("stream did not end after Close");
    assert!(
        end.is_none() || matches!(end, Some(Err(_))),
        "expected stream end after Close, got {end:?}",
    );
}
