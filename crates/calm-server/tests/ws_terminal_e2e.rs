//! Full-chain e2e for terminal protocol v2.
//!
//! Boots a real axum server (in-memory SqlxRepo) → spawns the real
//! `calm-session-daemon` binary backing `/bin/sh` → drives a tokio-tungstenite
//! client through the v2 happy path:
//!   ClientHello → ServerHello → Input → RenderPatch → ResizeCommit
//!   → ResizeApplied → Kill → TerminalExited.
//!
//! Existing tests cover the WS↔daemon bridge with a `DuplexStream` mock
//! (`tests/ws_terminal_v2.rs`) and the daemon-with-real-PTY but bypassing the
//! WS bridge (`calm-session/tests/protocol_error_routing.rs`). This file is
//! the only one that exercises every link in the chain in-process.
//!
//! Prerequisite: workspace bins must be built before this test runs. `cargo
//! test --workspace` handles this; `cargo test -p calm-server` alone may not.
//! `locate_daemon_bin` panics with a build hint if the binary is missing.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::model::{NewCove, NewWave};
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

/// Per-step budget. Generous because `spawn_daemon_for` itself polls the
/// daemon socket for up to ~3s (75 × 40ms) before returning, and a cold
/// PTY init under load can push close to that.
const STEP_TIMEOUT: Duration = Duration::from_secs(5);

/// Extended budget for the kill → exit sequence. After `ClientMsg::Kill`
/// the daemon sends SIGHUP and then SIGKILL after 2s; in between it
/// continues to broadcast `RenderPatch` frames carrying the shell's
/// SIGHUP-driven output. Under workspace-level CPU contention these
/// patches can monopolize the WS for several seconds before
/// `TerminalExited` makes it through. Bound is still well under the
/// issue's <10s total budget.
const EXIT_TIMEOUT: Duration = Duration::from_secs(8);

/// Locate the `calm-session-daemon` binary built by the workspace. Test
/// binaries live at `target/<profile>/deps/<test_name>`, so two `pop`s
/// land us in `target/<profile>/` where the daemon binary sits.
fn locate_daemon_bin() -> PathBuf {
    let mut p = std::env::current_exe().expect("current_exe");
    p.pop(); // strip test name (e.g. ws_terminal_e2e-<hash>)
    p.pop(); // strip "deps/"
    p.push("calm-session-daemon");
    assert!(
        p.exists(),
        "calm-session-daemon not found at {p:?}; run \
         `cargo build -p calm-session --bin calm-session-daemon` first, or \
         use `cargo test --workspace` which builds workspace bins"
    );
    p
}

/// Boot: in-memory repo + cove + wave seeded; AppState wired with a real
/// `DaemonClient` pointed at a fresh `TempDir` (so sockets from concurrent
/// tests don't race in /tmp); both REST and WS routers merged and bound to
/// a fresh `127.0.0.1:0` listener.
///
/// Returns:
///   - bound socket address (for ws upgrade)
///   - cloned router (for in-process REST oneshot calls)
///   - the wave id we seeded
///   - the `TempDir` (kept alive for the duration of the test — drop unlinks
///     the daemon socket directory)
async fn boot_full() -> (std::net::SocketAddr, axum::Router, String, TempDir) {
    let tmp = TempDir::new().expect("tempdir for daemon sockets");

    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );

    // Seed a cove + wave so the test can POST a card into the wave. Goes
    // through `raw_repo()` (gated behind the `fixtures` feature, auto-
    // enabled in dev-deps) just like `payload_validation.rs` does.
    let cove = repo
        .cove_create(NewCove {
            name: "e2e".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id,
            title: "e2e".into(),
            sort: None,
            cwd: String::new(),
            attach_folder: false,
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
            repo,
            PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data"),
            Vec::new(),
            EventBus::new(),
            calm_server::card_role_cache::CardRoleCache::new(),
            calm_server::wave_cove_cache::WaveCoveCache::new(),
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
    // Tiny breathing room — same idiom as tests/ws_events.rs.
    tokio::time::sleep(Duration::from_millis(50)).await;

    (addr, app, wave.id.to_string(), tmp)
}

/// In-process REST POST against the merged router (no TCP hop, no JSON
/// parsing race with the listener task). Returns the response with body
/// drained into a JSON value.
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

/// Read frames until one matches `pred`. Bounded by `STEP_TIMEOUT` total —
/// individual frames may arrive faster, but the cumulative wait won't blow
/// past the budget. Used to skip past unrelated `ChildReady` / `RenderPatch`
/// noise on the way to a target frame (e.g. `ResizeApplied`,
/// `TerminalExited`).
///
/// `label` is included in the timeout / unexpected-close panic message so
/// a CI failure pinpoints which step ran out of budget (the bare line
/// number alone doesn't disambiguate steps 8 vs 10).
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
    let (addr, app, wave_id, _tmp) = boot_full().await;

    // ---- 1. POST atomic terminal-card -----------------------------------
    //
    // One round-trip creates the card row, the linked terminal row, AND
    // spawns the daemon (the handler returns 201 only after the daemon is
    // accepting connections). The pre-#13 wire was a 3-step recipe
    // (POST card → POST /terminal → PATCH payload); the atomic endpoint
    // collapsed it to one call. See `routes::terminal_cards`.
    //
    // Force `/bin/sh` (not the user's `$SHELL`). The default-program path
    // would otherwise pick up zsh/fish/whatever from the host env, and
    // interactive shells can take seconds to respond to SIGHUP — the
    // kill→exit step is the test's main flakiness driver, and pinning to
    // `sh -c sh` cuts the worst-case from 8s+ down to sub-second
    // consistently. The wire-level v2 contract doesn't depend on the
    // shell choice.
    let (status, card) = rest_post(
        app.clone(),
        format!("/api/waves/{wave_id}/terminal-cards"),
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

    // ---- 2. WS upgrade --------------------------------------------------
    let ws_url = format!("ws://{addr}/api/terminals/{raw_terminal_id}");
    let (mut ws, _resp) =
        tokio::time::timeout(STEP_TIMEOUT, tokio_tungstenite::connect_async(&ws_url))
            .await
            .expect("ws connect timed out")
            .expect("ws connect failed");

    // ---- 3. ClientHello -------------------------------------------------
    // `terminal_id` comes from `POST /api/cards/{id}/terminal` as the
    // *simple* UUID form (no dashes): `model::new_id()` uses
    // `Uuid::simple()` and the response leaks that string verbatim. The
    // daemon, on the other hand, validates against
    // `cli.id.to_string()` which is hyphenated (`Uuid` `Display`). The
    // WS bridge in `crates/calm-server/src/ws/terminal.rs` normalizes
    // `ClientHello.terminal_id` to hyphenated form before forwarding to
    // the daemon, so we deliberately pass the raw response value here:
    // this test then exercises the full chain (browser → API response →
    // ClientHello → WS bridge normalization → daemon handshake) and
    // would regress to `BadHandshake` if the normalization were ever
    // removed.
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
            // WS bridge unconditionally strips this to `false` before
            // forwarding (see ws/terminal.rs §SECURITY). Setting it here
            // exercises the strip; the daemon receives `false` regardless.
            kernel_originated_input: false,
        },
    };
    ws.send(TMessage::Text(serde_json::to_string(&hello).unwrap()))
        .await
        .unwrap();

    // ---- 4. ServerHello -------------------------------------------------
    // The daemon stamps `terminal_id` into the ServerHello via
    // `cli.id.to_string()` (`Uuid` `Display`, always hyphenated), so the
    // ServerHello we receive is hyphenated — even though `hello_terminal_id`
    // (the *simple* form returned by the API) is what we sent. Compare
    // against the hyphenated form to assert the WS-bridge normalization
    // ran successfully end-to-end.
    let expected_terminal_id = Uuid::parse_str(&raw_terminal_id)
        .expect("terminal id is a uuid")
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
    // snapshot.data is the model's serialized viewport — for a freshly-
    // spawned /bin/sh against a 80x24 PTY this is non-empty (the daemon
    // ANSI-clears the screen + positions the cursor before serializing).
    assert!(
        snapshot_len > 0,
        "ServerHello snapshot.data should be non-empty"
    );

    // ---- 5. Input "echo hello\r" ----------------------------------------
    // `input_seq: 0` mirrors the browser path: no ack requested. This
    // test asserts on `RenderPatch` echo, not on `InputAck` arrival.
    ws.send(TMessage::Text(
        serde_json::to_string(&ClientMsg::Input {
            data: b"echo hello\r".to_vec(),
            input_seq: 0,
        })
        .unwrap(),
    ))
    .await
    .unwrap();

    // ---- 6. Collect RenderPatches until concat contains "hello" ---------
    // We can't rely on a single patch carrying the substring — the shell
    // may emit echo + the prompt redraw across two or more PTY chunks, and
    // each chunk becomes its own RenderPatch. Tolerant of `ChildReady`
    // (one-shot) and `RenderSnapshot` (resize-driven) interleaved in.
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
                // ChildReady / RenderSnapshot / etc. just don't contribute
                // to the echo concat — keep reading.
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

    // ---- 7. ResizeCommit ------------------------------------------------
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

    // ---- 8. ResizeApplied -----------------------------------------------
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

    // ---- 9. Kill --------------------------------------------------------
    ws.send(TMessage::Text(
        serde_json::to_string(&ClientMsg::Kill).unwrap(),
    ))
    .await
    .unwrap();

    // ---- 10. TerminalExited ---------------------------------------------
    let exited = wait_for(&mut ws, "TerminalExited", EXIT_TIMEOUT, |m| {
        matches!(m, DaemonMsg::TerminalExited { .. })
    })
    .await;
    // Don't assert the exit code — graceful (Kill → SIGHUP → shell exits
    // with whatever it decides) vs forced (SIGKILL fallback) yields
    // different codes on different libc / kernel combinations. Existence
    // of the frame is the contract.
    match exited {
        DaemonMsg::TerminalExited { .. } => {}
        _ => unreachable!(),
    }

    // Graceful WS close; ignore errors (the server may have already sent
    // its own Close after `TerminalExited`).
    let _ = ws.close(None).await;
}
