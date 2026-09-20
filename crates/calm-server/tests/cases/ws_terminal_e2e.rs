//! Full-chain e2e for terminal protocol v2: real axum server, real renderer backing `/bin/sh`, tokio-tungstenite client.
//! Workspace bins must be built first; `locate_daemon_bin` panics with a build hint if the binary is missing.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::model::{NewArea, NewTrack};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_server::ws;
use calm_session::{
    ClientCapabilities, ClientMsg, DaemonMsg, InitialScrollback, PROTOCOL_VERSION, PtySize,
    RenderEncoding, Role,
};
use futures_util::{SinkExt, StreamExt};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message as TMessage;
use tower::ServiceExt;
use uuid::Uuid;

/// Per-step budget; `spawn_terminal_for` itself polls the daemon socket for up to ~3s.
const STEP_TIMEOUT: Duration = Duration::from_secs(5);

/// Kill → exit budget: after `Kill` the daemon sends SIGHUP then SIGKILL after 2s, and SIGHUP-driven patches can monopolize the WS meanwhile.
const EXIT_TIMEOUT: Duration = Duration::from_secs(8);
/// Boot: in-memory repo + seeded area/track, real `DaemonClient` on a fresh `TempDir` so concurrent tests' sockets don't race in /tmp.
async fn boot_full() -> (std::net::SocketAddr, axum::Router, String, TempDir) {
    let tmp = TempDir::new().expect("tempdir for daemon sockets");

    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );

    let area = repo
        .area_create(NewArea {
            name: "e2e".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id,
            title: "e2e".into(),
            sort: None,
            // The terminal card's cwd defaults to the track's workspace; an empty workspace path is refused.
            cwd: "/neige-fixture-workspace".into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();

    let daemon = Arc::new(DaemonClient {
        data_dir: tmp.path().to_path_buf(),
        proc_supervisor_sock: None,
    });
    let state = AppState::from_parts(
        repo.clone(),
        EventBus::new(),
        daemon,
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo,
            PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data"),
            Vec::new(),
            EventBus::new(),
            calm_server::state::WriteContext::new(
                calm_server::card_role_cache::CardRoleCache::new(),
                calm_server::track_area_cache::TrackAreaCache::new(),
            ),
        )),
        Arc::new(CodexClient::new_stub()),
        None,
        None,
    );

    // REST routes need `actor_middleware` so handlers can extract `Actor`
    // from request extensions; mirror `main.rs`.
    let rest = routes::router().layer(axum::middleware::from_fn(
        calm_server::actor::actor_middleware,
    ));
    let app = axum::Router::new()
        .merge(rest)
        .merge(ws::router())
        .with_state(state);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let serve_app = app.clone();
    tokio::spawn(async move {
        axum::serve(
            listener,
            serve_app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    (addr, app, track.id.to_string(), tmp)
}

/// In-process REST POST against the merged router; returns the status and JSON body.
async fn rest_post(app: axum::Router, uri: String, body: Value) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

async fn recv_daemon_frame(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> DaemonMsg {
    let msg = tokio::time::timeout(STEP_TIMEOUT, ws.next())
        .await
        .expect("timed out waiting for daemon frame")
        .expect("ws stream closed early")
        .expect("ws read error");
    match msg {
        TMessage::Text(t) => serde_json::from_str(&t).expect("decode DaemonMsg from ws text frame"),
        other => panic!("expected text frame, got {other:?}"),
    }
}

/// Read frames until one matches `pred`, bounded by `timeout` in total; `label` names the step in the panic message.
async fn wait_for(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    label: &str,
    budget: Duration,
    pred: impl Fn(&DaemonMsg) -> bool,
) -> DaemonMsg {
    let deadline = tokio::time::Instant::now() + budget;
    let mut seen = Vec::<String>::new();
    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            panic!(
                "timed out waiting for {label}; saw {} other frames: {seen:?}",
                seen.len()
            );
        }
        let remaining = deadline - now;
        let msg = match tokio::time::timeout(remaining, ws.next()).await {
            Ok(Some(Ok(TMessage::Text(t)))) => {
                serde_json::from_str::<DaemonMsg>(&t).expect("decode DaemonMsg from ws text frame")
            }
            Ok(Some(Ok(TMessage::Close(_)))) => {
                panic!("ws closed before {label}; saw {seen:?}")
            }
            Ok(Some(Ok(_other))) => continue, // ignore Ping/Pong/Binary
            Ok(Some(Err(e))) => panic!("ws read error waiting for {label}: {e}"),
            Ok(None) => panic!("ws stream ended before {label}; saw {seen:?}"),
            Err(_) => panic!(
                "timed out waiting for {label}; saw {} other frames: {seen:?}",
                seen.len()
            ),
        };
        if pred(&msg) {
            return msg;
        }
        // Compact a frame label for diagnostics — the full Debug spew on a
        // `RenderSnapshot` is huge.
        seen.push(match &msg {
            DaemonMsg::RenderPatch(p) => format!("RenderPatch(rev={})", p.render_rev),
            DaemonMsg::RenderSnapshot(s) => format!("RenderSnapshot(rev={})", s.render_rev),
            DaemonMsg::ChildReady { .. } => "ChildReady".into(),
            DaemonMsg::ResizeApplied {
                epoch, cols, rows, ..
            } => {
                format!("ResizeApplied(epoch={epoch}, {cols}x{rows})")
            }
            DaemonMsg::TerminalExited { code, .. } => format!("TerminalExited(code={code:?})"),
            DaemonMsg::ProtocolError { code, message, .. } => {
                format!("ProtocolError({code:?}, {message:?})")
            }
            other => format!("{other:?}"),
        });
    }
}

#[tokio::test]
async fn v2_full_chain_happy_path() {
    let (addr, app, track_id, _tmp) = boot_full().await;

    // 1. POST atomic terminal-card. Force `/bin/sh`: interactive shells from `$SHELL` can take seconds to respond to SIGHUP, the main flakiness driver.
    let (status, card) = rest_post(
        app.clone(),
        format!("/api/tracks/{track_id}/terminal-cards"),
        json!({ "program": "/bin/sh", "cwd": "", "env": {}, "sort": 1.0, "theme": {"fg": [216,219,226], "bg": [15,20,24]} }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "create terminal-card: body={card:?}"
    );
    let raw_terminal_id = card["payload"]["terminal_id"]
        .as_str()
        .expect("card.payload.terminal_id is a string")
        .to_string();

    // 2. WS upgrade
    let ws_url = format!("ws://{addr}/api/terminals/{raw_terminal_id}");
    let (mut ws, _resp) =
        tokio::time::timeout(STEP_TIMEOUT, tokio_tungstenite::connect_async(&ws_url))
            .await
            .expect("ws connect timed out")
            .expect("ws connect failed");

    // 3. ClientHello. The API returns the simple UUID form; the WS bridge normalizes it before the daemon handshake, so pass the raw value deliberately.
    let hello_terminal_id = raw_terminal_id.clone();
    let hello = ClientMsg::ClientHello {
        protocol_version: PROTOCOL_VERSION,
        terminal_id: hello_terminal_id.clone(),
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
        role_hint: Some(Role::Owner),
        capabilities: ClientCapabilities {
            render_encodings: vec![RenderEncoding::Vt],
            supports_scrollback: true,
            supports_sixel: false,
            supports_images: false,
            // The WS bridge unconditionally strips this to `false` before forwarding.
            kernel_originated_input: false,
        },
    };
    ws.send(TMessage::Text(serde_json::to_string(&hello).unwrap()))
        .await
        .unwrap();

    // 4. ServerHello round-trips the simple form: the renderer stores entries by `term.id` and `sanitize_client_msg` normalizes inbound ids to simple.
    let expected_terminal_id = Uuid::parse_str(&raw_terminal_id)
        .expect("terminal id is a uuid")
        .simple()
        .to_string();
    let server_hello = recv_daemon_frame(&mut ws).await;
    let (client_role, snapshot_len) = match server_hello {
        DaemonMsg::ServerHello {
            client_role,
            snapshot,
            protocol_version,
            terminal_id,
            ..
        } => {
            assert_eq!(protocol_version, PROTOCOL_VERSION);
            assert_eq!(terminal_id, expected_terminal_id);
            (client_role, snapshot.data.len())
        }
        DaemonMsg::ProtocolError {
            code,
            message,
            expected_version,
        } => panic!(
            "expected ServerHello, got ProtocolError {{ code: {code:?}, \
             message: {message:?}, expected_version: {expected_version:?} }}"
        ),
        other => panic!("expected ServerHello, got {other:?}"),
    };
    assert!(
        matches!(client_role, Role::Owner),
        "first attach should be Owner"
    );
    // Non-empty for a fresh /bin/sh on an 80x24 PTY: the daemon ANSI-clears the screen before serializing.
    assert!(
        snapshot_len > 0,
        "ServerHello snapshot.data should be non-empty"
    );

    // 5. Input. `input_seq: 0` mirrors the browser path: no ack requested.
    ws.send(TMessage::Text(
        serde_json::to_string(&ClientMsg::Input {
            data: b"echo hello\r".to_vec(),
            input_seq: 0,
        })
        .unwrap(),
    ))
    .await
    .unwrap();

    // 6. Collect RenderPatches until the concatenation contains "hello": echo and prompt redraw may span several PTY chunks.
    let mut concat = Vec::<u8>::new();
    let deadline = tokio::time::Instant::now() + STEP_TIMEOUT;
    while tokio::time::Instant::now() < deadline
        && concat
            .windows(b"hello".len())
            .all(|w| w != b"hello".as_ref())
    {
        let remaining = deadline - tokio::time::Instant::now();
        let frame = tokio::time::timeout(remaining, ws.next()).await;
        match frame {
            Ok(Some(Ok(TMessage::Text(t)))) => {
                let msg: DaemonMsg =
                    serde_json::from_str(&t).expect("decode DaemonMsg from ws text frame");
                if let DaemonMsg::RenderPatch(p) = msg {
                    concat.extend_from_slice(&p.data);
                }
            }
            Ok(Some(Ok(TMessage::Close(_)))) => panic!("ws closed before echo arrived"),
            Ok(Some(Ok(_other))) => continue,
            Ok(Some(Err(e))) => panic!("ws read error during echo collect: {e}"),
            Ok(None) => panic!("ws stream ended during echo collect"),
            Err(_) => panic!(
                "timed out collecting echo output; got {} bytes so far: {:?}",
                concat.len(),
                String::from_utf8_lossy(&concat)
            ),
        }
    }
    assert!(
        concat.windows(b"hello".len()).any(|w| w == b"hello"),
        "expected echoed PTY output to contain 'hello'; got {} bytes: {:?}",
        concat.len(),
        String::from_utf8_lossy(&concat)
    );

    // 7. ResizeCommit
    ws.send(TMessage::Text(
        serde_json::to_string(&ClientMsg::ResizeCommit {
            epoch: 1,
            cols: 120,
            rows: 40,
        })
        .unwrap(),
    ))
    .await
    .unwrap();

    // 8. ResizeApplied
    let resize_applied = wait_for(&mut ws, "ResizeApplied", STEP_TIMEOUT, |m| {
        matches!(m, DaemonMsg::ResizeApplied { .. })
    })
    .await;
    match resize_applied {
        DaemonMsg::ResizeApplied {
            epoch, cols, rows, ..
        } => {
            assert_eq!(epoch, 1);
            assert_eq!(cols, 120);
            assert_eq!(rows, 40);
        }
        _ => unreachable!(),
    }

    // 9. Kill
    ws.send(TMessage::Text(
        serde_json::to_string(&ClientMsg::Kill).unwrap(),
    ))
    .await
    .unwrap();

    // 10. TerminalExited
    let exited = wait_for(&mut ws, "TerminalExited", EXIT_TIMEOUT, |m| {
        matches!(m, DaemonMsg::TerminalExited { .. })
    })
    .await;
    // The exit code differs between graceful (SIGHUP) and forced (SIGKILL) exits across libc/kernel combinations; the frame's existence is the contract.
    match exited {
        DaemonMsg::TerminalExited { .. } => {}
        _ => unreachable!(),
    }

    // Graceful WS close; ignore errors (the server may have already sent
    // its own Close after `TerminalExited`).
    let _ = ws.close(None).await;
}
