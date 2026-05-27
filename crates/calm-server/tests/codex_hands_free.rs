//! Integration test for the codex hands-free spawn path.
//!
//! This is the supersede-#110 test: instead of taping a `tokio::time::sleep`
//! against the daemon's startup race the way PR #110 did, it exercises the
//! deterministic InputAck-paired primitive that landed in #115 and the
//! `is_child_ready` snapshot that landed in #128.
//!
//! Two scenarios:
//!
//!   1. `inject_stdin_writes_bytes_and_awaits_input_ack` — happy path for
//!      the new `DaemonClient::inject_stdin` primitive. Boots a real
//!      `calm-session-daemon` running `/bin/cat` (so any byte we feed it
//!      echoes back), uses an observer WS to see the echo, then calls
//!      `inject_stdin(b"hello\r")`. Asserts the call returns `Ok` (i.e.
//!      InputAck arrived) AND the observer sees the bytes show up in a
//!      RenderPatch frame.
//!
//!   2. `auto_submit_subscriber_ignores_card_without_prompt` — covers the
//!      negative gate. Stand up the subscriber against an in-memory repo
//!      with a codex-kind card whose payload has no `prompt`, emit a
//!      synthetic `hook.codex.session_start`, and verify the subscriber
//!      doesn't attempt to contact the daemon (it would log a warn on a
//!      missing socket otherwise; we just confirm it returns quickly and
//!      doesn't panic). The positive subscriber path is covered by
//!      scenario 1's primitive — the subscriber is a thin shim over
//!      `inject_stdin` and exercising both ends in one test would require
//!      a real codex binary (out of scope for an integration test).
//!
//! Prerequisite: `calm-session-daemon` binary must be built. `cargo test
//! --workspace` builds it; `cargo test -p calm-server` alone may not.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::{Event, EventBus};
use calm_server::ids::{ActorId, CardId};
use calm_server::model::{NewCard, NewCove, NewWave};
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

/// Boots a router pointed at a real daemon binary + a fresh in-memory
/// SqlxRepo + a `127.0.0.1:0` listener serving the merged app.
async fn boot_full() -> (
    std::net::SocketAddr,
    axum::Router,
    String,
    Arc<DaemonClient>,
    TempDir,
) {
    let tmp = TempDir::new().expect("tempdir");
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let cove = repo
        .cove_create(NewCove {
            name: "hf".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id,
            title: "hf".into(),
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
            calm_server::card_role_cache::CardRoleCache::new(),
            calm_server::wave_cove_cache::WaveCoveCache::new(),
        )),
        Arc::new(CodexClient::new_stub()),
        None,
        None,
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
        axum::serve(
            listener,
            serve_app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    (addr, app, wave.id.to_string(), daemon, tmp)
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
        TMessage::Text(t) => serde_json::from_str(&t).expect("decode DaemonMsg"),
        other => panic!("expected text frame, got {other:?}"),
    }
}

#[tokio::test]
async fn inject_stdin_writes_bytes_and_awaits_input_ack() {
    let (addr, app, wave_id, daemon, _tmp) = boot_full().await;

    // Spawn a real PTY child that:
    //   1. emits an initial banner ("READY\n") so the daemon's
    //      `detect_ready` quiescent-window will fire and broadcast
    //      `ChildReady`; without an initial chunk it never fires (see
    //      `RenderPlane::detect_ready` in calm-session), which would
    //      hang the inject path,
    //   2. then loops `cat`, echoing everything we inject back over
    //      the render plane.
    //
    // Using `printf` (not `echo`) keeps the byte sequence to exactly
    // 6 bytes (`READY\n`) — predictable for the echo-assertion below.
    let (status, card) = rest_post(
        app.clone(),
        format!("/api/waves/{wave_id}/terminal-cards"),
        json!({
            "program": "printf 'READY\\n' && exec cat",
            "cwd": "",
            "env": {},
            "sort": 1.0, "theme": {"fg": [216,219,226], "bg": [15,20,24]} }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "card={card:?}");
    let raw_terminal_id = card["payload"]["terminal_id"]
        .as_str()
        .expect("terminal_id is a string")
        .to_string();
    let hyphenated_terminal_id = Uuid::parse_str(&raw_terminal_id)
        .expect("terminal id is a uuid")
        .to_string();

    // Open an observer WS so we can watch for the echo back from `cat`.
    let ws_url = format!("ws://{addr}/api/terminals/{raw_terminal_id}");
    let (mut observer_ws, _resp) =
        tokio::time::timeout(STEP_TIMEOUT, tokio_tungstenite::connect_async(&ws_url))
            .await
            .expect("observer ws connect")
            .expect("observer ws connect");

    let observer_hello = ClientMsg::ClientHello {
        protocol_version: PROTOCOL_VERSION,
        terminal_id: raw_terminal_id.clone(),
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
            kernel_originated_input: false,
        },
    };
    observer_ws
        .send(TMessage::Text(
            serde_json::to_string(&observer_hello).unwrap(),
        ))
        .await
        .unwrap();
    // Drain the ServerHello and wait until the child is reported ready
    // (or already was). We can't simply use `is_child_ready` from the
    // observer's ServerHello because the daemon's quiescent-detection
    // might not have fired yet — but for `cat`, output is purely
    // input-driven, so `detect_ready` will fire promptly once cat is
    // listening. We let `inject_stdin` (below) handle the wait itself.
    let _hello_frame = recv_daemon_frame(&mut observer_ws).await;

    // Now run the kernel-private inject_stdin path. It connects to the
    // same daemon socket the WS observer is on (the daemon
    // multiplexes), asserts kernel_originated_input=true, waits for
    // ChildReady if needed, sends `Input` with seq=1, and blocks on
    // InputAck{seq=1}. If anything in that pipeline regresses, this
    // call either errors or hangs past STEP_TIMEOUT.
    // sock_path is keyed on the *simple* (no-dashes) form because
    // `model::new_id` and the terminal row both store that form, and
    // `routes::terminal::spawn_daemon_for` writes the socket file at
    // `<data_dir>/<simple>.sock`. The hyphenated form is what we put
    // into `ClientHello.terminal_id` for the daemon's identity match
    // — `inject_stdin` handles that normalization internally.
    let sock = daemon.sock_path(&raw_terminal_id);
    let _ = hyphenated_terminal_id; // confirmed parses as UUID; not used directly
    tokio::time::timeout(
        STEP_TIMEOUT,
        daemon.inject_stdin(
            &sock,
            &raw_terminal_id, // simple form — inject_stdin normalizes
            b"HELLO\r",
            Duration::from_secs(4),
        ),
    )
    .await
    .expect("inject_stdin should complete within budget")
    .expect("inject_stdin should return Ok (InputAck received)");

    // The observer should see the bytes echoed via a RenderPatch. We
    // skip past the cat-startup RenderSnapshot / ChildReady / interim
    // RenderPatch frames until we find one whose decoded data contains
    // our marker.
    let deadline = tokio::time::Instant::now() + STEP_TIMEOUT;
    let mut saw_echo = false;
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline - tokio::time::Instant::now();
        let Ok(Some(Ok(msg))) = tokio::time::timeout(remaining, observer_ws.next()).await else {
            break;
        };
        let TMessage::Text(t) = msg else { continue };
        let Ok(frame) = serde_json::from_str::<DaemonMsg>(&t) else {
            continue;
        };
        match frame {
            DaemonMsg::RenderPatch(p) if String::from_utf8_lossy(&p.data).contains("HELLO") => {
                saw_echo = true;
                break;
            }
            DaemonMsg::RenderSnapshot(s) if String::from_utf8_lossy(&s.data).contains("HELLO") => {
                saw_echo = true;
                break;
            }
            _ => {}
        }
    }
    assert!(
        saw_echo,
        "expected to see HELLO echoed back from `cat` via inject_stdin"
    );
}

#[tokio::test]
async fn auto_submit_subscriber_skips_card_without_prompt() {
    // Stands the subscriber up against a stub daemon (sock path points
    // nowhere) and an in-memory repo holding one codex-kind card whose
    // payload has no `prompt`. Emits a synthetic
    // `hook.codex.session_start` for the card. Subscriber should
    // observe "no prompt → skip" and never touch the daemon — proven
    // by the absence of a "inject_stdin failed" log (we just rely on
    // no panic / clean shutdown; if the subscriber tried to connect
    // to the bogus socket it would warn but not crash).
    //
    // The positive path (prompt present → inject_stdin called) is
    // covered by the live-daemon test above; here we lock down the
    // gate.
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let cove = repo
        .cove_create(NewCove {
            name: "gate".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id,
            title: "gate".into(),
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
            kind: "codex".into(),
            sort: None,
            payload: json!({
                "schemaVersion": 1,
                "terminal_id": "deadbeef-not-a-real-uuid",
                // intentionally NO `prompt` field
            }),
        })
        .await
        .unwrap();

    let events = EventBus::new();
    let daemon = Arc::new(DaemonClient::new_stub());
    calm_server::codex_auto_submit::spawn(repo.clone(), daemon, events.clone());

    // Give the subscriber a tick to wire up its receiver.
    tokio::time::sleep(Duration::from_millis(20)).await;

    // Emit the event the subscriber listens for. With no `prompt` on
    // the card, the gate at `maybe_submit` should short-circuit
    // *before* it touches the daemon socket.
    events.emit(
        ActorId::AiCodex(CardId::from("test")),
        Event::CodexHook {
            card_id: card.id.clone(),
            kind: "hook.codex.session_start".into(),
            payload: json!({}),
        },
    );

    // Allow the subscriber's fire-and-forget task to run. There's no
    // affirmative signal we can wait on (the no-op path has no
    // observable side-effect); 100ms is plenty for the subscriber to
    // recv, lookup, and short-circuit.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // If we got here without a panic and the test process is still
    // running, the gate held. The only way this could regress
    // silently is if the subscriber crashed the task — `tokio::spawn`
    // would swallow that, but the surrounding test framework also
    // catches panics on the runtime's join. Add an active assertion
    // anyway: the card should still be readable through the same
    // repo handle the subscriber is using (rules out repo poisoning).
    let still_there = repo.card_get(card.id.as_str()).await.unwrap();
    assert!(
        still_there.is_some(),
        "card should remain queryable after subscriber dispatch"
    );
}

// ---------------------------------------------------------------------------
// End-to-end negative gate: route → subscriber chain must NOT auto-submit
// when the route receives `prompt: ""` or no `prompt` field at all.
//
// The subscriber-side unit gate is covered above
// (`auto_submit_subscriber_skips_card_without_prompt`), and the route
// itself normalizes prompt at parse time before stamping payload. This
// test wires the two halves together: it actually hits
// `POST /api/waves/:id/codex-cards` so that the payload-stamping logic
// in the route is what writes the card, then exercises the live
// subscriber against the bus.
//
// Detection strategy: bind a UnixListener at the exact socket path
// `DaemonClient::inject_stdin` would dial (`<data_dir>/<term>.sock`)
// *before* emitting the synthetic session_start event. If the
// subscriber called `inject_stdin`, the listener's `accept()` future
// would complete; we assert it stays pending across a tight window.
// This is the same negative-path style the existing test uses (no
// affirmative signal exists for "skipped") but with an active
// connection-observed guard instead of just "no panic".
// ---------------------------------------------------------------------------
#[tokio::test]
async fn route_to_subscriber_chain_skips_auto_submit_for_empty_or_absent_prompt() {
    // Re-use `boot_full` minus the daemon binary — we deliberately
    // point at a bogus path so the route's spawn step fails (500 to
    // the client). Per #132's existing
    // `returns_500_on_daemon_spawn_failure_but_persists_row` test, the
    // card + terminal row are still committed; that's what we want
    // here so the subscriber has a real card to look up.
    let tmp = TempDir::new().expect("tempdir");
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let cove = repo
        .cove_create(NewCove {
            name: "no-prompt".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id,
            title: "no-prompt".into(),
            sort: None,
            cwd: String::new(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();

    // Bogus daemon binary so spawn fails fast; data_dir is a tempdir
    // so we control where `sock_path` resolves.
    let bad_bin = std::env::temp_dir().join("definitely-not-a-real-daemon-binary-hf");
    let _ = std::fs::remove_file(&bad_bin);
    let daemon = Arc::new(DaemonClient {
        data_dir: tmp.path().to_path_buf(),
        session_daemon_bin: bad_bin,
    });
    let events = EventBus::new();
    let state = AppState::from_parts(
        repo.clone(),
        events.clone(),
        daemon.clone(),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
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
    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state);

    // Wire up the real subscriber against the same bus + daemon + repo
    // the route will write to. This is the chain we're locking down.
    calm_server::codex_auto_submit::spawn(repo.clone(), daemon.clone(), events.clone());
    tokio::time::sleep(Duration::from_millis(20)).await;

    // Sub-case runner. `body` is the JSON we POST to the route; `label`
    // tags assertion messages so a failure pinpoints which variant
    // regressed.
    let run_case = |body: Value, label: &'static str| {
        let app = app.clone();
        let wave_id = wave.id.clone();
        let events = events.clone();
        let repo = repo.clone();
        let daemon = daemon.clone();
        async move {
            let (status, resp) =
                rest_post(app, format!("/api/waves/{wave_id}/codex-cards"), body).await;
            // 500 because the bogus daemon binary makes spawn fail —
            // but the card+terminal txn still committed.
            assert_eq!(
                status,
                StatusCode::INTERNAL_SERVER_ERROR,
                "{label}: expected 500 (daemon spawn fails on bogus bin); body={resp:?}"
            );

            // Recover the persisted card so we can pull its
            // terminal_id and emit a CodexHook matching it.
            let cards = repo.cards_by_wave(wave_id.as_str()).await.unwrap();
            let card = cards
                .iter()
                .find(|c| c.kind == "codex" && c.payload.get("prompt").is_none_or(Value::is_null))
                .unwrap_or_else(|| {
                    panic!(
                        "{label}: expected at least one codex card with no prompt; got {cards:?}"
                    )
                })
                .clone();
            let terminal_id = card.payload["terminal_id"]
                .as_str()
                .expect("payload.terminal_id stamped")
                .to_string();

            // Bind a listener at the exact socket path the subscriber
            // would dial. If `codex_auto_submit` mistakenly called
            // `inject_stdin`, our `accept()` would complete; the
            // assertion below proves it stays pending.
            let sock_path = daemon.sock_path(&terminal_id);
            if let Some(parent) = sock_path.parent() {
                std::fs::create_dir_all(parent).expect("mkdir data_dir for sock listener");
            }
            let _ = std::fs::remove_file(&sock_path);
            let listener = tokio::net::UnixListener::bind(&sock_path)
                .unwrap_or_else(|e| panic!("{label}: bind sock listener at {sock_path:?}: {e}"));

            // Emit the trigger event. With no prompt on the card, the
            // subscriber's gate should short-circuit before touching
            // the socket.
            events.emit(
                ActorId::AiCodex(CardId::from("test")),
                Event::CodexHook {
                    card_id: card.id.clone(),
                    kind: "hook.codex.session_start".into(),
                    payload: json!({}),
                },
            );

            // Tight window — the subscriber's fire-and-forget task
            // would complete its connect attempt well inside 200ms on
            // any reasonable test host. If `accept()` *does* fire,
            // that's a regression: prompt-less card auto-submitted.
            let observed = tokio::time::timeout(Duration::from_millis(200), listener.accept())
                .await
                .ok();
            assert!(
                observed.is_none(),
                "{label}: subscriber must NOT connect to the daemon socket when prompt is empty/absent; observed connection from auto-submit path"
            );

            // Clean up so the next sub-case starts from a known state.
            // (Card stays in the DB — the next case will find its own
            // card via the same `prompt is None` predicate.)
            drop(listener);
            let _ = std::fs::remove_file(&sock_path);
        }
    };

    // Sub-case 1: explicit empty-string prompt. Route normalizes via
    // `.trim().filter(!empty)` before stamping, so payload.prompt ends
    // up absent — subscriber gate matches the same shape.
    run_case(
        json!({ "prompt": "", "theme": {"fg": [216,219,226], "bg": [15,20,24]} }),
        "empty-string prompt",
    )
    .await;
    // Reset: clear cards so the next case's lookup-by-no-prompt is
    // unambiguous (we want exactly one prompt-less card to find).
    // Issue #197 — `terminals.card_id` is now `ON DELETE RESTRICT`, so
    // we drop the terminal row first (the eager-teardown shape the
    // route handler applies). The actual daemon process was never
    // spawned (the bogus binary path made `spawn_daemon_for` 500
    // immediately), so there's nothing to SIGTERM here.
    for c in repo.cards_by_wave(wave.id.as_str()).await.unwrap() {
        if let Some(t) = repo.terminal_get_by_card(c.id.as_str()).await.unwrap() {
            repo.terminal_delete(t.id.as_str()).await.unwrap();
        }
        repo.card_delete(c.id.as_str()).await.unwrap();
    }

    // Sub-case 2: prompt field omitted entirely. Same expected
    // behavior — subscriber should not auto-submit.
    run_case(
        json!({ "theme": {"fg": [216,219,226], "bg": [15,20,24]} }),
        "absent prompt field",
    )
    .await;
}
