//! Integration test for the codex hands-free spawn primitive's
//! v2-protocol interaction with the calm-session-daemon.
//!
//! What we're actually validating
//! ------------------------------
//!
//! The hands-free path has four layers (managed hooks, composer pre-
//! fill, trust silencer, auto-submit). Layers 1-3 are pure file/string
//! plumbing covered by unit tests. Layer 4 is the only one that touches
//! the live daemon protocol, and that's where v2 compatibility could
//! break. This file pins it.
//!
//! Specifically:
//!
//!   * `DaemonClient::inject_stdin` opens a fresh Unix-socket
//!     connection to a live PTY-backed daemon, frames a `ClientHello`
//!     with `capabilities.kernel_originated_input = true` +
//!     `role_hint = Observer`, then an `Input` frame with our bytes,
//!     then drops. The daemon's owner-only gate on `Input` must accept
//!     the write because of the capability (the post-PR-87 v2 contract).
//!
//!   * The browser's Owner WS connection must remain Owner — no
//!     `OwnerChanged` frame fires when the kernel injects. This is what
//!     keeps the user able to keep typing while a hands-free spawn
//!     finishes; if our injection accidentally stole Owner, the
//!     browser's next keystroke would land in nowhere.
//!
//!   * The injected bytes flow through the PTY just like browser-typed
//!     bytes — they show up in subsequent `RenderPatch` frames. (We use
//!     `/bin/sh` and inject `printf hello\n` because that's the
//!     tightest substring-match we can do without exit-code timing
//!     races.)
//!
//! What we're explicitly NOT testing here
//! --------------------------------------
//!
//! Codex itself is not on PATH in CI. The interaction with the actual
//! codex CLI (its composer + auto-submit-on-`\r`) is covered manually
//! and at the unit-test level (`shell_single_quote`, `build_config_toml`,
//! `should_auto_submit`). This test stands in for the wire-level
//! contract between `inject_stdin` and the daemon, which is the only
//! place the v2 protocol can regress invisibly.

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

const STEP_TIMEOUT: Duration = Duration::from_secs(5);

/// Mirror of `tests/ws_terminal_e2e.rs::locate_daemon_bin`. Kept local
/// rather than refactored into a shared `tests/common/` module because
/// the shared-module song-and-dance ranges from one-off-helper to
/// `cargo` quirks (`Cargo.toml` `[[test]]` declarations vs. `mod`
/// inclusion); a 10-line copy is easier to read than the alternative.
fn locate_daemon_bin() -> PathBuf {
    let mut p = std::env::current_exe().expect("current_exe");
    p.pop();
    p.pop();
    p.push("calm-session-daemon");
    assert!(
        p.exists(),
        "calm-session-daemon not found at {p:?}; run \
         `cargo build -p calm-session --bin calm-session-daemon` first, or \
         use `cargo test --workspace` which builds workspace bins"
    );
    p
}

async fn boot_full() -> (
    std::net::SocketAddr,
    axum::Router,
    Arc<DaemonClient>,
    String,
    TempDir,
) {
    let tmp = TempDir::new().expect("tempdir for daemon sockets");

    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );

    let cove = repo
        .cove_create(NewCove {
            name: "cf".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id,
            title: "cf".into(),
            sort: None,
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
        daemon.clone(),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo,
            PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data"),
            Vec::new(),
            EventBus::new(),
        )),
        Arc::new(CodexClient::new_stub()),
    );

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
        axum::serve(listener, serve_app).await.unwrap();
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    (addr, app, daemon, wave.id, tmp)
}

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

/// Drain frames until `pred` returns Some or we hit the budget. Tolerant
/// of `RenderPatch` / `RenderSnapshot` / `ChildReady` noise.
async fn wait_for_some<T>(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    label: &str,
    budget: Duration,
    mut pred: impl FnMut(&DaemonMsg) -> Option<T>,
) -> T {
    let deadline = tokio::time::Instant::now() + budget;
    let mut seen: Vec<String> = Vec::new();
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
            Ok(Some(Ok(TMessage::Close(_)))) => panic!("ws closed before {label}; saw {seen:?}"),
            Ok(Some(Ok(_other))) => continue,
            Ok(Some(Err(e))) => panic!("ws read error waiting for {label}: {e}"),
            Ok(None) => panic!("ws stream ended before {label}; saw {seen:?}"),
            Err(_) => panic!(
                "timed out waiting for {label}; saw {} other frames: {seen:?}",
                seen.len()
            ),
        };
        if let Some(out) = pred(&msg) {
            return out;
        }
        seen.push(match &msg {
            DaemonMsg::RenderPatch(p) => format!("RenderPatch(rev={})", p.render_rev),
            DaemonMsg::RenderSnapshot(s) => format!("RenderSnapshot(rev={})", s.render_rev),
            DaemonMsg::ChildReady { .. } => "ChildReady".into(),
            DaemonMsg::OwnerChanged { .. } => "OwnerChanged".into(),
            other => format!("{other:?}"),
        });
    }
}

/// Spawn a `/bin/sh` terminal, attach as Owner via WS, inject bytes
/// via the kernel's privileged path, observe them echo, and assert the
/// browser Owner connection stays Owner throughout.
#[tokio::test]
async fn inject_stdin_writes_via_kernel_capability_without_stealing_owner() {
    let (addr, app, daemon, wave_id, _tmp) = boot_full().await;

    // ---- 1. Card + terminal via the real REST path. -------------------
    let (status, card) = rest_post(
        app.clone(),
        format!("/api/waves/{wave_id}/cards"),
        json!({ "kind": "terminal", "payload": null, "sort": 1.0 }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create card: body={card:?}");
    let card_id = card["id"].as_str().unwrap().to_string();

    let (status, term) = rest_post(
        app.clone(),
        format!("/api/cards/{card_id}/terminal"),
        json!({ "program": "/bin/sh", "cwd": "", "env": {} }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "create terminal: body={term:?}"
    );
    let raw_terminal_id = term["id"].as_str().unwrap().to_string();
    // `model::new_id` returns the *simple* (no-dashes) UUID form via the
    // API. Production callers (`codex_auto_submit`) read this exact form
    // off `card.payload.terminal_id` and hand it to BOTH `sock_path`
    // and `inject_stdin`; the latter normalizes internally to the
    // hyphenated form the daemon's handshake match expects. That double-
    // duty is the contract this test pins.
    let hyphenated_terminal_id = Uuid::parse_str(&raw_terminal_id)
        .expect("terminal id is a uuid")
        .to_string();

    // ---- 2. Browser-style WS attach as Owner. -------------------------
    let ws_url = format!("ws://{addr}/api/terminals/{raw_terminal_id}");
    let (mut ws, _resp) =
        tokio::time::timeout(STEP_TIMEOUT, tokio_tungstenite::connect_async(&ws_url))
            .await
            .expect("ws connect timed out")
            .expect("ws connect failed");

    let browser_client_id = Uuid::new_v4();
    let hello = ClientMsg::ClientHello {
        protocol_version: PROTOCOL_VERSION,
        terminal_id: raw_terminal_id.clone(),
        client_id: browser_client_id,
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
            // The WS bridge unconditionally strips this to `false` (see
            // ws/terminal.rs §SECURITY); a hostile browser CANNOT spoof
            // kernel-originated input. The kernel-side `inject_stdin`
            // path, by contrast, sets it to `true` because it speaks
            // directly to the daemon over the kernel-private Unix socket
            // — that's exactly what this test verifies works.
            kernel_originated_input: false,
        },
    };
    ws.send(TMessage::Text(serde_json::to_string(&hello).unwrap()))
        .await
        .unwrap();

    // ServerHello — confirms Owner role for the browser client.
    let server_hello = wait_for_some(&mut ws, "ServerHello", STEP_TIMEOUT, |msg| match msg {
        DaemonMsg::ServerHello {
            client_role,
            terminal_id,
            owner_client_id,
            ..
        } => Some((*client_role, terminal_id.clone(), *owner_client_id)),
        _ => None,
    })
    .await;
    assert!(
        matches!(server_hello.0, Role::Owner),
        "first attach should be Owner, got {:?}",
        server_hello.0
    );
    assert_eq!(
        server_hello.1, hyphenated_terminal_id,
        "daemon stamps hyphenated terminal_id in ServerHello"
    );
    assert_eq!(
        server_hello.2,
        Some(browser_client_id),
        "daemon ServerHello should report the browser client as Owner"
    );

    // ---- 3. Inject from the kernel-private path. ----------------------
    // `printf hello\n` is the smallest thing that lands a deterministic
    // substring in the PTY output without depending on a prompt redraw
    // or exit timing. Newline (not `\r`) because the test isn't trying
    // to mimic codex's submit behavior — `\r` would also work but the
    // shell's response to it is more variable across shells.
    //
    // We deliberately use `raw_terminal_id` (the simple no-dashes form
    // returned by the API and the exact bytes a production caller would
    // read from `card.payload.terminal_id`) for BOTH the sock-path
    // lookup and the inject_stdin id argument. The first is correct
    // because `spawn_daemon_for` created the socket file at
    // `daemon.sock_path(&term.id)`; the second exercises
    // `inject_stdin`'s internal normalization to hyphenated form for
    // the daemon handshake. If the normalization were ever removed,
    // this test would regress to a `BadHandshake`-driven
    // connection-closed error.
    // Wait a beat for the /bin/sh child to finish initializing under the
    // PTY. The daemon's handshake returns the moment it accepts the
    // socket connection, but the shell prompt / stdin readiness happens
    // separately on another channel. 100ms is empirically enough on dev
    // machines without padding the happy-path test budget.
    tokio::time::sleep(Duration::from_millis(100)).await;

    let sock_path = daemon.sock_path(&raw_terminal_id);
    daemon
        .inject_stdin(&sock_path, &raw_terminal_id, b"printf hello\n")
        .await
        .expect("inject_stdin failed");
    // Give the daemon a moment to process the Input frame, forward
    // through the PTY, and broadcast the resulting RenderPatch to all
    // attached clients (the kernel connection has closed by now, but the
    // browser is still listening). Without this, fast machines can race
    // the close-vs-process and we'd false-alarm before the
    // PTY round-trip lands.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // ---- 4. Drain frames until we see "hello" in the rendered output. -
    // RenderPatch carries the raw VT chunk. Multiple chunks may interleave
    // with ChildReady / RenderSnapshot — collect across all of them.
    let mut concat: Vec<u8> = Vec::new();
    let deadline = tokio::time::Instant::now() + STEP_TIMEOUT;
    let mut owner_changed_seen = false;
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline - tokio::time::Instant::now();
        let msg = match tokio::time::timeout(remaining, ws.next()).await {
            Ok(Some(Ok(TMessage::Text(t)))) => {
                serde_json::from_str::<DaemonMsg>(&t).expect("decode")
            }
            Ok(Some(Ok(_))) => continue,
            Ok(Some(Err(e))) => panic!("ws read error: {e}"),
            Ok(None) => break,
            Err(_) => break,
        };
        match &msg {
            DaemonMsg::RenderPatch(p) => concat.extend_from_slice(&p.data),
            DaemonMsg::RenderSnapshot(s) => {
                // A snapshot is the daemon's authoritative view at one
                // point in time — append for the substring search rather
                // than `clear()`-ing, so a snapshot triggered by our
                // kernel-client's hello ResizePty doesn't drop earlier
                // RenderPatches that already carried the injected bytes.
                concat.extend_from_slice(&s.data);
            }
            DaemonMsg::OwnerChanged { .. } => {
                owner_changed_seen = true;
            }
            _ => {}
        }
        if concat.windows(b"hello".len()).any(|w| w == b"hello") {
            break;
        }
    }

    assert!(
        concat.windows(b"hello".len()).any(|w| w == b"hello"),
        "expected 'hello' to appear in PTY output after inject_stdin; \
         got {} bytes: {:?}",
        concat.len(),
        String::from_utf8_lossy(&concat),
    );

    // ---- 5. The browser is still Owner. -------------------------------
    //
    // The whole point of `role_hint = Observer` + the
    // `kernel_originated_input` capability is that the kernel can write
    // without stealing Owner. If we saw an `OwnerChanged` frame in the
    // drain above, that contract is broken.
    assert!(
        !owner_changed_seen,
        "kernel inject_stdin must NOT trigger OwnerChanged \
         (would demote the browser's Owner connection)"
    );

    // Belt-and-suspenders: send an `Input` from the browser to prove it
    // still has write authority. A NotOwner ProtocolError here would
    // mean the kernel inject stole Owner silently.
    ws.send(TMessage::Text(
        serde_json::to_string(&ClientMsg::Input(b"\n".to_vec())).unwrap(),
    ))
    .await
    .unwrap();
    let post_check_deadline = tokio::time::Instant::now() + Duration::from_millis(500);
    while tokio::time::Instant::now() < post_check_deadline {
        let remaining = post_check_deadline - tokio::time::Instant::now();
        match tokio::time::timeout(remaining, ws.next()).await {
            Ok(Some(Ok(TMessage::Text(t)))) => {
                let msg: DaemonMsg = serde_json::from_str(&t).unwrap();
                if let DaemonMsg::ProtocolError { code, message, .. } = msg {
                    panic!(
                        "browser Input was rejected with ProtocolError {code:?} {message:?} \
                         — kernel inject_stdin must have stolen Owner"
                    );
                }
            }
            Ok(Some(_)) => continue,
            _ => break,
        }
    }
}
