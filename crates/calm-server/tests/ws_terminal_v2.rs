//! Wire-level e2e tests for the v2 terminal protocol in the WS↔daemon
//! bridge (`pump`). Mounts the public [`calm_server::ws::terminal::pump`]
//! function under a minimal axum router and drives it with one end of a
//! `tokio::io::duplex` pair playing the role of the daemon socket. No
//! terminal renderer is started — every byte of the
//! JSON↔bincode bridge is exercised in-process.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use axum::Router;
use axum::extract::ws::WebSocketUpgrade;
use axum::routing::get;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::model::{NewCard, NewCove, NewTerminal, NewWave, Terminal};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes::theme::RequestTheme;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_server::terminal_renderer::{
    ClientPumpContext, RendererConfig, SharedExitState, SharedOwnerRegistry, SharedRenderPlane,
    SupervisorControl, run_client_pump,
};
use calm_server::ws;
use calm_server::ws::terminal::pump;
use calm_session::terminal_session::{OwnerRegistry, RenderPlane};
use calm_session::{
    ClientCapabilities, ClientMsg, DaemonMsg, InitialScrollback, PROTOCOL_VERSION, PtySize,
    RenderEncoding, RenderPatch, RenderSnapshot, read_frame, write_frame,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tempfile::TempDir;
use tokio::io::DuplexStream;
use tokio::net::TcpListener;
use tokio::sync::{Mutex, broadcast, mpsc};
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
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
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

struct RendererWsFixture {
    _tmp: TempDir,
    repo: Arc<dyn Repo>,
    state: AppState,
    addr: SocketAddr,
}

async fn boot_renderer_ws() -> RendererWsFixture {
    let tmp = TempDir::new().expect("tempdir");
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let events = EventBus::new();
    let state = AppState::from_parts(
        repo.clone(),
        events.clone(),
        Arc::new(DaemonClient {
            data_dir: tmp.path().to_path_buf(),
            proc_supervisor_sock: Some(tmp.path().join("missing-proc-supervisor.sock")),
        }),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            tmp.path().join("plugins-data"),
            Vec::new(),
            events,
            calm_server::state::WriteContext::new(
                calm_server::card_role_cache::CardRoleCache::new(),
                calm_server::wave_cove_cache::WaveCoveCache::new(),
            ),
        )),
        Arc::new(CodexClient::new_stub()),
        None,
        None,
    );

    let app = Router::new()
        .merge(ws::terminal::router())
        .with_state(state.clone());
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
    tokio::time::sleep(Duration::from_millis(50)).await;

    RendererWsFixture {
        _tmp: tmp,
        repo,
        state,
        addr,
    }
}

async fn seed_terminal_with_scrollback(fixture: &RendererWsFixture, label: &str) -> Terminal {
    let cove = fixture
        .repo
        .cove_create(NewCove {
            name: format!("scrollback-{label}"),
            color: "#000".into(),
            sort: None,
        })
        .await
        .expect("create cove");
    let wave = fixture
        .repo
        .wave_create(NewWave {
            cove_id: cove.id,
            title: format!("scrollback-{label}"),
            sort: None,
            cwd: fixture._tmp.path().display().to_string(),
            workflow_id: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        })
        .await
        .expect("create wave");
    let card = fixture
        .repo
        .card_create(NewCard {
            wave_id: wave.id,
            kind: "terminal".into(),
            sort: None,
            payload: json!({}),
        })
        .await
        .expect("create card");
    let term = fixture
        .repo
        .terminal_create(NewTerminal {
            card_id: card.id,
            program: "/bin/sh".into(),
            cwd: fixture._tmp.path().display().to_string(),
            env: json!({}),
            theme: RequestTheme::default_dark(),
        })
        .await
        .expect("create terminal");

    let entry = fixture
        .state
        .terminal_renderer
        .insert_test_entry(RendererConfig {
            terminal_id: term.id.clone(),
            cols: 10,
            rows: 2,
            buffer_bytes: 1 << 20,
            terminal_fg: (216, 219, 226),
            terminal_bg: (15, 20, 24),
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "sleep 30".into()],
            envs: Vec::new(),
            cwd: fixture._tmp.path().display().to_string(),
            supervisor_sock: fixture
                .state
                .daemon
                .proc_supervisor_sock
                .clone()
                .expect("supervisor sock"),
        });

    {
        let mut render_plane = entry.handle.render_plane.lock().expect("render plane");
        for i in 0..7 {
            render_plane.on_pty_chunk(format!("line{i}\n").into_bytes());
        }
    }

    term
}

async fn attach_and_read_server_hello(
    addr: SocketAddr,
    term: &Terminal,
    initial_scrollback: InitialScrollback,
) -> DaemonMsg {
    let url = format!("ws://{addr}/api/terminals/{}", term.id);
    let (mut ws, _) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("ws connect");
    let hello = ClientMsg::ClientHello {
        protocol_version: PROTOCOL_VERSION,
        terminal_id: term.id.clone(),
        client_id: Uuid::new_v4(),
        desired_size: PtySize {
            cols: 10,
            rows: 2,
            pixel_width: None,
            pixel_height: None,
        },
        cell_size: None,
        initial_scrollback,
        resume_from: None,
        role_hint: None,
        capabilities: ClientCapabilities {
            render_encodings: vec![RenderEncoding::Vt],
            supports_scrollback: true,
            supports_sixel: false,
            supports_images: false,
            kernel_originated_input: false,
        },
    };
    ws.send(TMessage::Text(serde_json::to_string(&hello).unwrap()))
        .await
        .unwrap();

    let msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .expect("ws recv timed out")
        .expect("ws closed unexpectedly")
        .expect("ws error");
    match msg {
        TMessage::Text(t) => serde_json::from_str(&t.to_string()).expect("server hello json"),
        other => panic!("expected text, got {other:?}"),
    }
}

#[tokio::test]
async fn initial_scrollback_all_round_trips_into_server_hello_snapshot() {
    let fixture = boot_renderer_ws().await;
    let with_all = seed_terminal_with_scrollback(&fixture, "all").await;
    let with_none = seed_terminal_with_scrollback(&fixture, "none").await;

    let msg = attach_and_read_server_hello(fixture.addr, &with_all, InitialScrollback::All).await;
    match msg {
        DaemonMsg::ServerHello { snapshot, .. } => {
            let scrollback = snapshot
                .scrollback
                .expect("InitialScrollback::All should populate snapshot.scrollback");
            assert!(
                !scrollback.is_empty(),
                "scrollback bytes should be non-empty"
            );
        }
        other => panic!("expected ServerHello for All, got {other:?}"),
    }

    let msg = attach_and_read_server_hello(fixture.addr, &with_none, InitialScrollback::None).await;
    match msg {
        DaemonMsg::ServerHello { snapshot, .. } => {
            assert!(
                snapshot.scrollback.is_none(),
                "InitialScrollback::None should leave snapshot.scrollback empty"
            );
        }
        other => panic!("expected ServerHello for None, got {other:?}"),
    }
}

/// `ws_terminal_v2` normally exercises the WS bridge. The lag-recovery bug
/// lives one layer lower in `client_pump`, so this focused in-process test
/// drives `run_client_pump` directly: a tiny broadcast channel is overrun
/// while the per-client outbound queue is full, producing `RecvError::Lagged`
/// without relying on websocket timing.
#[tokio::test]
async fn lag_recovery_snapshot_respects_initial_scrollback_all() {
    let terminal_id = Uuid::new_v4().to_string();
    let render_plane: SharedRenderPlane =
        Arc::new(StdMutex::new(RenderPlane::new(12, 2, 64 * 1024, 2000)));
    seed_scrollback(&render_plane);

    let owner_registry: SharedOwnerRegistry = Arc::new(StdMutex::new(OwnerRegistry::new()));
    let exit: SharedExitState = Arc::new(StdMutex::new(None));
    let (event_tx, event_rx) = broadcast::channel::<DaemonMsg>(2);
    let (supervisor_tx, _supervisor_rx) = mpsc::unbounded_channel::<SupervisorControl>();
    let (incoming_tx, incoming_rx) = mpsc::channel::<ClientMsg>(4);
    let (outgoing_tx, mut outgoing_rx) = mpsc::channel::<DaemonMsg>(1);

    let pump = tokio::spawn({
        let render_plane = render_plane.clone();
        let owner_registry = owner_registry.clone();
        let exit = exit.clone();
        let event_tx_for_pump = event_tx.clone();
        let terminal_id_for_pump = terminal_id.clone();
        async move {
            run_client_pump(
                incoming_rx,
                outgoing_tx,
                ClientPumpContext {
                    event_rx,
                    event_tx: event_tx_for_pump,
                    render_plane,
                    exit,
                    supervisor_tx,
                    owner_registry,
                    session_id: Uuid::new_v4(),
                    terminal_id: terminal_id_for_pump,
                },
            )
            .await
        }
    });

    incoming_tx
        .send(ClientMsg::ClientHello {
            protocol_version: PROTOCOL_VERSION,
            terminal_id,
            client_id: Uuid::new_v4(),
            desired_size: PtySize {
                cols: 12,
                rows: 2,
                pixel_width: None,
                pixel_height: None,
            },
            cell_size: None,
            initial_scrollback: InitialScrollback::All,
            resume_from: None,
            role_hint: None,
            capabilities: ClientCapabilities {
                render_encodings: vec![RenderEncoding::Vt],
                supports_scrollback: true,
                supports_sixel: false,
                supports_images: false,
                kernel_originated_input: false,
            },
        })
        .await
        .expect("send client hello");

    let hello = tokio::time::timeout(Duration::from_secs(2), outgoing_rx.recv())
        .await
        .expect("server hello timeout")
        .expect("server hello channel closed");
    assert!(matches!(hello, DaemonMsg::ServerHello { .. }));

    for i in 0..16 {
        event_tx
            .send(DaemonMsg::RenderPatch(RenderPatch {
                render_rev: i + 1,
                prev_render_rev: i,
                pty_seq: i + 1,
                encoding: RenderEncoding::Vt,
                data: format!("lag-{i}\r\n").into_bytes(),
            }))
            .expect("send broadcast frame");
    }

    let snap = wait_for_lag_snapshot(&mut outgoing_rx).await;
    assert!(
        snap.scrollback
            .as_ref()
            .is_some_and(|bytes| !bytes.is_empty()),
        "lag-recovery RenderSnapshot should preserve InitialScrollback::All"
    );

    drop(incoming_tx);
    tokio::time::timeout(Duration::from_secs(2), pump)
        .await
        .expect("pump join timeout")
        .expect("pump join")
        .expect("pump result");
}

fn seed_scrollback(render_plane: &SharedRenderPlane) {
    let mut rp = render_plane.lock().expect("render plane lock");
    for i in 0..12 {
        let _ = rp.on_pty_chunk(format!("line-{i:02}\r\n").into_bytes());
    }
}

async fn wait_for_lag_snapshot(rx: &mut mpsc::Receiver<DaemonMsg>) -> RenderSnapshot {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    let mut saw_snapshot_required = false;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "timed out waiting for lag recovery snapshot"
        );
        let msg = tokio::time::timeout(remaining, rx.recv())
            .await
            .expect("lag snapshot timeout")
            .expect("daemon channel closed");
        match msg {
            DaemonMsg::SnapshotRequired { .. } => {
                saw_snapshot_required = true;
            }
            DaemonMsg::RenderSnapshot(snap) if saw_snapshot_required => return snap,
            _ => {}
        }
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
        is_child_ready: false,
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
    let input = ClientMsg::Input {
        data: b"keystrokes".to_vec(),
        input_seq: 0,
    };
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
        ClientMsg::Input { data, input_seq } => {
            assert_eq!(data, b"keystrokes");
            // Browser-path default: seq 0 ("no ack requested" — option
            // (b) from issue #115). The WS bridge must NOT synthesize a
            // non-zero seq; if a future change adds bridge-side rewrite
            // this assertion catches the regression.
            assert_eq!(input_seq, 0);
        }
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

/// SECURITY: the WS bridge is the untrusted-network ingress for daemon
/// ClientMsg frames. `ClientCapabilities.kernel_originated_input` is a
/// daemon-side trust flag — the daemon does NOT verify it, so ingress
/// layers must sanitize. This test forges a ClientHello with
/// `kernel_originated_input: true` from the browser side and asserts the
/// pump zeroes the flag before forwarding to the daemon. Without this
/// strip, a browser-connected Observer could write arbitrary bytes to
/// another user's PTY by claiming to be a kernel-originated client.
#[tokio::test]
async fn ws_strips_kernel_originated_input_flag() {
    let (mut daemon_side, server_side) = tokio::io::duplex(8192);
    let addr = boot(server_side, TID).await;

    let url = format!("ws://{}/pump", addr);
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    // Forge a ClientHello that claims kernel-originated trust.
    let forged = ClientMsg::ClientHello {
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
            // The attack: a browser asserting kernel-private trust.
            kernel_originated_input: true,
        },
    };
    ws.send(TMessage::Text(serde_json::to_string(&forged).unwrap()))
        .await
        .unwrap();

    let got: ClientMsg = tokio::time::timeout(
        Duration::from_secs(2),
        read_frame::<ClientMsg, _>(&mut daemon_side),
    )
    .await
    .expect("daemon-side hello read timed out")
    .expect("daemon-side hello decode failed");

    match got {
        ClientMsg::ClientHello { capabilities, .. } => {
            assert!(
                !capabilities.kernel_originated_input,
                "WS bridge MUST strip kernel_originated_input on ingress; \
                 a browser-asserted `true` would let an Observer write to \
                 another user's PTY"
            );
        }
        other => panic!("expected ClientHello, got {other:?}"),
    }
}

/// CORRECTNESS: `model::new_id()` returns the *simple* (32-hex, no dashes)
/// UUID form. The API response leaks that verbatim to the browser; the
/// browser sends it back inside `ClientHello.terminal_id`. The daemon, on
/// the other hand, validates `ClientHello.terminal_id ==
/// cli.id.to_string()` byte-for-byte, and `Uuid::to_string()` is always
/// the hyphenated form. Without normalization at the WS bridge, every
/// browser hello would fail with `BadHandshake` — the daemon side would
/// see "0123456789abcdef…" but compare against "01234567-89ab-cdef-…".
///
/// This test forges a ClientHello whose `terminal_id` is the simple form
/// of a valid UUID, runs it through the pump, and asserts the daemon side
/// reads the hyphenated form. Mirrors the
/// `ws_strips_kernel_originated_input_flag` pattern.
#[tokio::test]
async fn ws_normalizes_terminal_id_to_hyphenated() {
    // Use a deterministic UUID so the assertion can compare against a
    // known hyphenated string. `Uuid::nil()` is trivially distinguishable;
    // any v4 works, this one is just a literal for clarity.
    let uuid = Uuid::parse_str("da163adc-4ccf-4b50-9e2e-3248afe7dcd1").unwrap();
    let simple = uuid.simple().to_string();
    let hyphenated = uuid.to_string();
    assert_eq!(
        hyphenated, "da163adc-4ccf-4b50-9e2e-3248afe7dcd1",
        "uuid Display must be hyphenated"
    );
    assert_eq!(
        simple, "da163adc4ccf4b509e2e3248afe7dcd1",
        "uuid simple form must omit dashes"
    );

    let (mut daemon_side, server_side) = tokio::io::duplex(8192);
    let addr = boot(server_side, &hyphenated).await;

    let url = format!("ws://{}/pump", addr);
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    // Forge a ClientHello carrying the *simple* form, exactly what the
    // browser would send after reading the API response.
    let forged = ClientMsg::ClientHello {
        protocol_version: PROTOCOL_VERSION,
        terminal_id: simple.clone(),
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
    };
    ws.send(TMessage::Text(serde_json::to_string(&forged).unwrap()))
        .await
        .unwrap();

    let got: ClientMsg = tokio::time::timeout(
        Duration::from_secs(2),
        read_frame::<ClientMsg, _>(&mut daemon_side),
    )
    .await
    .expect("daemon-side hello read timed out")
    .expect("daemon-side hello decode failed");

    match got {
        ClientMsg::ClientHello { terminal_id, .. } => {
            assert_eq!(
                terminal_id, hyphenated,
                "WS bridge MUST normalize terminal_id to hyphenated form so \
                 daemon byte-level handshake (uses `cli.id.to_string()` which \
                 is hyphenated) succeeds against server-generated simple-form \
                 ids (`model::new_id` uses `Uuid::simple()`)"
            );
        }
        other => panic!("expected ClientHello, got {other:?}"),
    }
}

/// CORRECTNESS: if the client somehow sends a `terminal_id` that isn't a
/// valid UUID, the WS bridge must pass it through verbatim so the daemon
/// can reject it as `BadHandshake`. Silently mangling malformed input
/// would mask client bugs and make debugging harder.
#[tokio::test]
async fn ws_passes_through_malformed_terminal_id_unchanged() {
    let (mut daemon_side, server_side) = tokio::io::duplex(8192);
    let addr = boot(server_side, TID).await;

    let url = format!("ws://{}/pump", addr);
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    let garbage = "not-a-uuid-at-all".to_string();
    let forged = ClientMsg::ClientHello {
        protocol_version: PROTOCOL_VERSION,
        terminal_id: garbage.clone(),
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
    };
    ws.send(TMessage::Text(serde_json::to_string(&forged).unwrap()))
        .await
        .unwrap();

    let got: ClientMsg = tokio::time::timeout(
        Duration::from_secs(2),
        read_frame::<ClientMsg, _>(&mut daemon_side),
    )
    .await
    .expect("daemon-side hello read timed out")
    .expect("daemon-side hello decode failed");

    match got {
        ClientMsg::ClientHello { terminal_id, .. } => {
            assert_eq!(
                terminal_id, garbage,
                "WS bridge MUST pass malformed terminal_id through unchanged so \
                 the daemon's BadHandshake remains fail-loud"
            );
        }
        other => panic!("expected ClientHello, got {other:?}"),
    }
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
