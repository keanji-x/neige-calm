//! `GET /api/terminals/:id` (WebSocket upgrade). **Owned by Track D.**
//!
//! ## Protocol
//!
//! Frames carry the `calm_session::ClientMsg` / `DaemonMsg` enums encoded as
//! JSON text. Each WS text frame is exactly one serde-JSON `ClientMsg` (going
//! up) or `DaemonMsg` (coming down). Binary WS frames are not used in this
//! bridge — the wave's own xterm.js client handles VT replay on top of
//! `DaemonMsg::Hello.replay` / `DaemonMsg::Stdout` byte arrays delivered as
//! JSON byte-arrays.
//!
//! This is intentionally a *thin* bridge: history, replay, seq numbering,
//! reconnect epochs etc. all live in the daemon (Hello.replay) or are handled
//! at the daemon attach layer. Calm-server just shuttles frames.

use crate::error::Result;
use crate::routes::terminal::spawn_daemon_for;
use crate::state::AppState;
use axum::{
    Router,
    extract::{
        Path, State,
        ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade},
    },
    response::IntoResponse,
    routing::get,
};
use calm_session::{ClientMsg, DaemonMsg, FrameError, read_frame, write_frame};
use futures::{SinkExt, StreamExt};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::sync::Mutex;
// `tokio::time::Instant` (not `std::time::Instant`) so `tokio::time::pause()`
// in tests virtual-advances `elapsed()` along with `interval` ticks. In
// production this is a thin wrapper over `std::time::Instant`.
use tokio::time::Instant;

/// Interval between server-sent WebSocket Ping frames. Ten seconds is well
/// under the typical idle-disconnect window for HTTP intermediaries (60s) and
/// gives us three Ping attempts before [`PONG_TIMEOUT`] fires.
const PING_INTERVAL: Duration = Duration::from_secs(10);

/// If we don't see any frame (pong, text, binary, close) from the client for
/// this long, treat the connection as dead and close it with a 1011 frame.
/// Set to 30s so a single ping miss still tolerates a normal interval, but two
/// in a row trips detection.
const PONG_TIMEOUT: Duration = Duration::from_secs(30);

/// Custom close-code description we send when the heartbeat trips. 1011 is the
/// IANA-registered "server error" code; the reason text is purely advisory and
/// surfaced in server logs when troubleshooting.
const PONG_TIMEOUT_REASON: &str = "no pong";

pub fn router() -> Router<AppState> {
    Router::new().route("/api/terminals/{id}", get(upgrade))
}

async fn upgrade(
    ws: WebSocketUpgrade,
    Path(id): Path<String>,
    State(s): State<AppState>,
) -> impl IntoResponse {
    // Resolve the socket path *before* the upgrade so a missing terminal
    // returns a proper HTTP error instead of a 101 + immediate close.
    // If the daemon for an existing row has died (shell exited, OS killed
    // it, calm-server restart unhooked it, …), respawn here so the client
    // re-attach feels seamless.
    let sock = match resolve_live_sock(&s, &id).await {
        Ok(p) => p,
        Err(e) => return e.into_response(),
    };
    ws.on_upgrade(move |socket| handle(socket, sock, id))
        .into_response()
}

/// Resolve the socket path for a terminal row, **revive a dead daemon if
/// necessary**. The revive path:
///   1. Read the terminal row from the repo.
///   2. If `daemon_handle` is set, probe the socket (`UnixStream::connect`).
///      If it connects, the daemon is alive — return the path.
///   3. Otherwise re-spawn the daemon with the row's original program /
///      cwd / env, wait for it to be ready, persist the new handle, and
///      return that path.
async fn resolve_live_sock(s: &AppState, id: &str) -> Result<PathBuf> {
    let term = s
        .repo
        .terminal_get(id)
        .await?
        .ok_or_else(|| crate::error::CalmError::NotFound(format!("terminal {id}")))?;

    if let Some(handle) = term.daemon_handle.as_ref() {
        if let Ok(_probe) = UnixStream::connect(handle).await {
            // Live daemon — fast path.
            return Ok(PathBuf::from(handle));
        }
        tracing::info!(
            terminal_id = %term.id,
            sock = %handle,
            "daemon socket unreachable — respawning"
        );
    } else {
        tracing::info!(terminal_id = %term.id, "terminal has no daemon_handle — spawning");
    }

    // Cold path: respawn. `spawn_daemon_for` updates `daemon_handle`
    // when it succeeds, so we re-read to get the canonical path.
    let env = term.env.clone();
    spawn_daemon_for(s, &term, &term.program, &term.cwd, &env).await?;
    let refreshed = s.repo.terminal_get(id).await?.ok_or_else(|| {
        crate::error::CalmError::Internal("terminal vanished after respawn".into())
    })?;
    let handle = refreshed.daemon_handle.ok_or_else(|| {
        crate::error::CalmError::Internal(format!(
            "terminal {id}: daemon_handle still missing after respawn"
        ))
    })?;
    Ok(PathBuf::from(handle))
}

async fn handle(socket: WebSocket, sock: PathBuf, terminal_id: String) {
    let stream = match UnixStream::connect(&sock).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, sock = ?sock, "connect daemon socket failed");
            return;
        }
    };
    pump(socket, stream, PING_INTERVAL, PONG_TIMEOUT).await
}

/// Daemon transport abstraction. The WS bridge only needs the
/// bidirectional `AsyncRead + AsyncWrite` half — in production this is a
/// `tokio::net::UnixStream`; in tests it's one end of a
/// `tokio::io::duplex` pair so we can drive the pump in-process without
/// forking a real `calm-session-daemon`.
///
/// A blanket impl covers any type with the right combination of bounds;
/// callers don't need to opt in explicitly.
pub(crate) trait DaemonTransport:
    tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + 'static
{
}

impl<T> DaemonTransport for T where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + 'static
{
}

/// Core WS↔daemon bridge loop. Splits the `daemon` transport into its
/// read/write halves, splits the `ws` socket, and drives three concurrent
/// arms (up: WS Text → daemon bincode frame; down: daemon bincode frame
/// → WS Text; heartbeat: pings + dead-client detection) inside a single
/// `tokio::select!`. Exits as soon as any arm completes — closing one
/// half cancels the rest.
///
/// `ping_interval` and `pong_timeout` are parameters so tests can pass
/// short windows; production values are [`PING_INTERVAL`] and
/// [`PONG_TIMEOUT`] (10s / 30s) and are wired in by [`handle`].
///
/// Generic over `DaemonTransport` so unit tests can substitute
/// `tokio::io::DuplexStream`. The split-into-halves pattern requires
/// `AsyncRead + AsyncWrite` on the concrete type, which the trait bound
/// provides via [`tokio::io::split`].
pub(crate) async fn pump<T: DaemonTransport>(
    ws: WebSocket,
    daemon: T,
    ping_interval: Duration,
    pong_timeout: Duration,
) {
    let (mut rd, mut wr) = tokio::io::split(daemon);
    let (ws_tx, mut ws_rx) = ws.split();

    // Share the write half across the three tasks (down-stream, ping, and
    // the heartbeat close path) behind a mutex. Contention here is trivial:
    // pings fire every 10s, close fires at most once, and downstream sends
    // are usually orders of magnitude apart from those.
    let ws_tx = Arc::new(Mutex::new(ws_tx));

    // Most recent moment we received *any* frame from the client. Pong
    // frames count; so do Text/Binary, since any traffic proves the socket
    // is alive.
    let last_seen = Arc::new(Mutex::new(Instant::now()));

    // WS → daemon: parse each text frame as ClientMsg, write to socket.
    // Also bumps `last_seen` so the heartbeat detector knows the client is
    // alive without relying on pong frames alone (axum auto-pongs, but
    // pongs DO come back through the read half).
    let last_seen_up = last_seen.clone();
    let up = async move {
        while let Some(Ok(msg)) = ws_rx.next().await {
            // Any frame counts as liveness — pong, text, binary, ping.
            *last_seen_up.lock().await = Instant::now();
            match msg {
                Message::Text(text) => {
                    let parsed: ClientMsg = match serde_json::from_str(&text) {
                        Ok(m) => m,
                        Err(e) => {
                            tracing::warn!(error = %e, "unparseable ClientMsg JSON; dropping");
                            continue;
                        }
                    };
                    if write_frame(&mut wr, &parsed).await.is_err() {
                        break;
                    }
                }
                // Binary frames could be used as an optimization for Stdin
                // (skip JSON wrapping). Not part of the documented contract
                // — drop for now, surface if the frontend wants it.
                Message::Binary(_) => {}
                Message::Close(_) => break,
                // Ping/Pong: axum auto-responds to client Ping; client Pong
                // is what we want to observe — `last_seen` already bumped
                // above.
                _ => {}
            }
        }
    };

    // Daemon → WS: read framed bincode DaemonMsg, ship as JSON text.
    let ws_tx_down = ws_tx.clone();
    let terminal_id_down = terminal_id.clone();
    let down = async move {
        loop {
            let msg: DaemonMsg = match read_frame(&mut rd).await {
                Ok(m) => m,
                Err(e) => {
                    // Version-skew on the kernel↔daemon Unix socket: this
                    // means a daemon binary was started against a stale
                    // `calm-session` schema. Log loudly with the daemon
                    // identity (terminal id + socket path) so an operator
                    // can correlate to the deploy that introduced the skew,
                    // then fall through to the connection-close path below.
                    //
                    // We intentionally do NOT flag `needs_restart` on the
                    // terminal row here: that field doesn't exist on the
                    // current `Terminal` model and plumbing one through the
                    // repo trait is out of scope for the framing PR. Tracked
                    // as a follow-up in the PR description for #45.
                    match &e {
                        FrameError::BadMagic { got, expected } => {
                            tracing::error!(
                                terminal_id = %terminal_id_down,
                                got = ?got,
                                expected = ?expected,
                                "daemon framing magic mismatch — closing WS"
                            );
                        }
                        FrameError::UnsupportedFrameVersion { got, supported } => {
                            tracing::error!(
                                terminal_id = %terminal_id_down,
                                got,
                                supported,
                                "daemon framing version mismatch — closing WS"
                            );
                        }
                        // Oversize / decode / io are the existing failure
                        // modes; debug-log to avoid spamming on normal
                        // peer-close paths (EOF shows up as Io here).
                        other => {
                            tracing::debug!(
                                terminal_id = %terminal_id_down,
                                error = %other,
                                "daemon read_frame ended"
                            );
                        }
                    }
                    break;
                }
            };
            let exit = matches!(msg, DaemonMsg::ChildExited { .. });
            let text = match serde_json::to_string(&msg) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(error = %e, "serialize DaemonMsg failed");
                    continue;
                }
            };
            if ws_tx_down
                .lock()
                .await
                .send(Message::Text(text.into()))
                .await
                .is_err()
            {
                break;
            }
            if exit {
                break;
            }
        }
        let _ = ws_tx_down.lock().await.send(Message::Close(None)).await;
    };

    // Heartbeat: ping every `ping_interval`; if `last_seen` is older than
    // `pong_timeout`, log + close with 1011. Browsers don't expose pongs to
    // JS, but our `last_seen` is bumped on *any* frame, and clients ack
    // pings with pongs at the protocol layer — that's all we need for
    // server-side death detection. Exits when the socket gets closed (a
    // send error trips us out and the `select!` cancels the other arms).
    let ws_tx_hb = ws_tx.clone();
    let last_seen_hb = last_seen.clone();
    let heartbeat = run_heartbeat(ws_tx_hb, last_seen_hb, ping_interval, pong_timeout);

    tokio::select! {
        _ = up => {}
        _ = down => {}
        _ = heartbeat => {}
    }
}

// ---- Heartbeat (testable in isolation) ---------------------------------

/// Sink abstraction for the heartbeat task. Behind a trait so tests can
/// substitute an in-memory `Vec<Message>` for the real
/// `SplitSink<WebSocket, Message>`. Production code only ever sends Ping
/// and (one) Close. Method is named `hb_send` to avoid name clashing with
/// `futures::SinkExt::send` on the same concrete type.
#[async_trait::async_trait]
pub(crate) trait HeartbeatSink: Send + 'static {
    async fn hb_send(&mut self, msg: Message) -> std::result::Result<(), ()>;
}

/// Production blanket impl: the axum WebSocket SplitSink.
#[async_trait::async_trait]
impl HeartbeatSink for futures::stream::SplitSink<WebSocket, Message> {
    async fn hb_send(&mut self, msg: Message) -> std::result::Result<(), ()> {
        <Self as SinkExt<Message>>::send(self, msg)
            .await
            .map_err(|_| ())
    }
}

/// Pings at `ping_interval`; if `last_seen.elapsed() > pong_timeout`, sends
/// `Close(1011 "no pong")` and exits. Pulled out of `handle()` so the timing
/// behavior can be unit-tested without standing up a real WebSocket /
/// daemon socket pair.
pub(crate) async fn run_heartbeat<S>(
    sink: Arc<Mutex<S>>,
    last_seen: Arc<Mutex<Instant>>,
    ping_interval: Duration,
    pong_timeout: Duration,
) where
    S: HeartbeatSink,
{
    let mut tick = tokio::time::interval(ping_interval);
    // First tick fires immediately by default — skip it; we don't need a
    // ping in the first interval of a fresh connection.
    tick.tick().await;
    loop {
        tick.tick().await;
        if last_seen.lock().await.elapsed() > pong_timeout {
            tracing::warn!(
                timeout_secs = pong_timeout.as_secs(),
                "terminal WS: no pong from client; closing"
            );
            let _ = sink
                .lock()
                .await
                .hb_send(Message::Close(Some(CloseFrame {
                    code: 1011,
                    reason: PONG_TIMEOUT_REASON.into(),
                })))
                .await;
            break;
        }
        // axum::extract::ws::Message::Ping wraps `Bytes`. An empty payload
        // is the smallest valid ping.
        if sink
            .lock()
            .await
            .hb_send(Message::Ping(Default::default()))
            .await
            .is_err()
        {
            break;
        }
    }
}

#[cfg(test)]
mod heartbeat_tests {
    use super::*;

    /// Capturing sink — records every message sent and never fails.
    struct VecSink(Vec<Message>);

    #[async_trait::async_trait]
    impl HeartbeatSink for VecSink {
        async fn hb_send(&mut self, msg: Message) -> std::result::Result<(), ()> {
            self.0.push(msg);
            Ok(())
        }
    }

    fn is_close_1011(msg: &Message) -> bool {
        matches!(msg, Message::Close(Some(CloseFrame { code: 1011, .. })))
    }

    fn is_ping(msg: &Message) -> bool {
        matches!(msg, Message::Ping(_))
    }

    /// With pongs never arriving (`last_seen` frozen), `run_heartbeat` should
    /// send Pings until `pong_timeout` elapses, then issue a Close(1011) and
    /// exit. Uses 100ms/300ms windows to keep wall-clock cost negligible; the
    /// production constants (10s / 30s) share the same timing logic.
    #[tokio::test]
    async fn closes_when_no_pong_within_timeout() {
        let sink = Arc::new(Mutex::new(VecSink(Vec::new())));
        let last_seen = Arc::new(Mutex::new(Instant::now()));
        let ping = Duration::from_millis(100);
        let pong = Duration::from_millis(300);

        let sink_clone = sink.clone();
        let ls_clone = last_seen.clone();
        let h = tokio::spawn(async move { run_heartbeat(sink_clone, ls_clone, ping, pong).await });

        // Wait long enough for the heartbeat to send a few pings and trip
        // the timeout. 600ms ≫ 300ms so the close branch must have fired.
        let _ = tokio::time::timeout(Duration::from_millis(800), h).await;

        let log = &sink.lock().await.0;
        assert!(
            log.iter().any(is_close_1011),
            "expected a Close(1011) frame, got: {:?}",
            log
        );
    }

    /// If the client keeps the connection live by bumping `last_seen`, the
    /// heartbeat should keep pinging and never issue a Close.
    #[tokio::test]
    async fn pings_continue_when_pongs_keep_coming() {
        let sink = Arc::new(Mutex::new(VecSink(Vec::new())));
        let last_seen = Arc::new(Mutex::new(Instant::now()));
        let ping = Duration::from_millis(50);
        let pong = Duration::from_millis(200);

        let sink_clone = sink.clone();
        let ls_clone = last_seen.clone();
        let h = tokio::spawn(async move { run_heartbeat(sink_clone, ls_clone, ping, pong).await });

        // Simulate a healthy client: bump `last_seen` every 25ms for 300ms.
        // Pong-window is 200ms so the heartbeat must NOT see a timeout.
        for _ in 0..12 {
            tokio::time::sleep(Duration::from_millis(25)).await;
            *last_seen.lock().await = Instant::now();
        }

        h.abort();

        let log = &sink.lock().await.0;
        assert!(
            log.iter().any(is_ping),
            "expected at least one Ping, got: {:?}",
            log
        );
        assert!(
            !log.iter().any(is_close_1011),
            "did NOT expect a Close(1011), got: {:?}",
            log
        );
    }
}

#[cfg(test)]
mod pump_tests {
    //! In-process tests for the WS↔daemon bridge that don't fork a real
    //! `calm-session-daemon`. We mount [`pump`] under a tiny `axum::Router`
    //! with a single WS route, drive it with a `tokio_tungstenite` client
    //! over a local TCP listener (the same pattern used in
    //! `tests/ws_events.rs`), and on the daemon side substitute a
    //! `tokio::io::duplex` pair for `UnixStream`. Net result: the only thing
    //! we're skipping vs. production is the kernel socket — every byte of
    //! the JSON↔bincode bridge is exercised.
    //!
    //! Timing: ping/pong values are kept very large (10s / 60s) so the
    //! heartbeat arm never fires inside test wall-clock; the cases we
    //! actually want to assert are about up/down translation and graceful
    //! shutdown, not heartbeat behavior (covered by `heartbeat_tests`).
    use super::*;
    use axum::Router;
    use axum::extract::ws::WebSocketUpgrade;
    use axum::routing::get;
    use calm_session::{ClientMsg, DaemonMsg, read_frame, write_frame};
    use futures_util::{SinkExt, StreamExt};
    use std::net::SocketAddr;
    use tokio::io::DuplexStream;
    use tokio::net::TcpListener;
    use tokio_tungstenite::tungstenite::Message as TMessage;

    /// Bring up a one-route axum app whose WS handler invokes [`pump`] with
    /// the supplied `daemon_side` of a duplex pair. Returns the bound
    /// address; the caller then connects with `tokio_tungstenite`.
    ///
    /// Each call creates a fresh listener on `127.0.0.1:0` so concurrent
    /// tests don't share state. The server task is spawned and lives until
    /// the test ends; no cleanup needed because the runtime tears it down.
    async fn boot_pump(daemon_side: DuplexStream, ping: Duration, pong: Duration) -> SocketAddr {
        // Wrap the DuplexStream in a Mutex<Option<…>> so the closure given
        // to `Router::route` can `take` it the first time the route is
        // hit. `on_upgrade` consumes the value by move; the option dance
        // is just to satisfy `Fn` (not `FnOnce`) while only firing once.
        let slot = Arc::new(Mutex::new(Some(daemon_side)));
        let app = Router::new().route(
            "/pump",
            get(move |upgrade: WebSocketUpgrade| {
                let slot = slot.clone();
                async move {
                    let daemon = slot
                        .lock()
                        .await
                        .take()
                        .expect("pump route called more than once");
                    upgrade.on_upgrade(move |socket| pump(socket, daemon, ping, pong))
                }
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        // Tiny breathing room — same idiom as tests/ws_events.rs.
        tokio::time::sleep(Duration::from_millis(50)).await;
        addr
    }

    /// Big enough to never wake during a test. We don't want the heartbeat
    /// arm racing the assertions for up/down behavior.
    fn long_window() -> (Duration, Duration) {
        (Duration::from_secs(10), Duration::from_secs(60))
    }

    /// daemon → WS: write a `DaemonMsg::Stdout` on the duplex; the WS
    /// client must receive a JSON Text frame that round-trips back to the
    /// same `DaemonMsg`.
    #[tokio::test]
    async fn down_translates_daemon_frame_to_ws_text() {
        let (mut daemon_side, server_side) = tokio::io::duplex(8192);
        let (ping, pong) = long_window();
        let addr = boot_pump(server_side, ping, pong).await;

        let url = format!("ws://{}/pump", addr);
        let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

        // Push one bincode frame from the "daemon" side.
        write_frame(&mut daemon_side, &DaemonMsg::Stdout(b"world".to_vec()))
            .await
            .unwrap();

        // WS client should receive a single Text frame.
        let msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .expect("ws recv timed out")
            .expect("ws closed unexpectedly")
            .expect("ws error");
        let text = match msg {
            TMessage::Text(t) => t.to_string(),
            other => panic!("expected text, got {:?}", other),
        };
        let parsed: DaemonMsg = serde_json::from_str(&text).expect("valid DaemonMsg JSON");
        match parsed {
            DaemonMsg::Stdout(bytes) => assert_eq!(bytes, b"world"),
            other => panic!("expected Stdout(b\"world\"), got {:?}", other),
        }
    }

    /// WS → daemon: client pushes a Text frame containing
    /// `ClientMsg::Stdin`; the daemon side must observe one bincode-framed
    /// `ClientMsg` matching it.
    #[tokio::test]
    async fn up_translates_ws_text_to_daemon_frame() {
        let (mut daemon_side, server_side) = tokio::io::duplex(8192);
        let (ping, pong) = long_window();
        let addr = boot_pump(server_side, ping, pong).await;

        let url = format!("ws://{}/pump", addr);
        let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

        let stdin = ClientMsg::Stdin(b"hello".to_vec());
        let json = serde_json::to_string(&stdin).unwrap();
        ws.send(TMessage::Text(json)).await.unwrap();

        let got: ClientMsg = tokio::time::timeout(
            Duration::from_secs(2),
            read_frame::<ClientMsg, _>(&mut daemon_side),
        )
        .await
        .expect("daemon-side read timed out")
        .expect("daemon-side read failed");
        match got {
            ClientMsg::Stdin(b) => assert_eq!(b, b"hello"),
            other => panic!("expected Stdin(b\"hello\"), got {:?}", other),
        }
    }

    /// When the daemon emits `ChildExited`, the pump must (a) forward the
    /// frame as JSON, (b) send a WS Close, and (c) return. Asserting on
    /// the stream draining to `None` is the test's proxy for "pump
    /// returned".
    #[tokio::test]
    async fn child_exited_closes_ws_and_pump_returns() {
        let (mut daemon_side, server_side) = tokio::io::duplex(8192);
        let (ping, pong) = long_window();
        let addr = boot_pump(server_side, ping, pong).await;

        let url = format!("ws://{}/pump", addr);
        let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

        write_frame(&mut daemon_side, &DaemonMsg::ChildExited { code: Some(0) })
            .await
            .unwrap();
        // Drop our daemon-side writer so the down arm's read_frame would
        // hit EOF if the ChildExited break didn't already trigger. Either
        // way the pump must wind down.
        drop(daemon_side);

        // 1) Text frame carrying the JSON ChildExited.
        let msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .expect("ws recv timed out")
            .expect("ws closed before exit message")
            .expect("ws error");
        let text = match msg {
            TMessage::Text(t) => t.to_string(),
            other => panic!("expected text, got {:?}", other),
        };
        let parsed: DaemonMsg = serde_json::from_str(&text).unwrap();
        assert!(
            matches!(parsed, DaemonMsg::ChildExited { code: Some(0) }),
            "expected ChildExited, got {:?}",
            parsed
        );

        // 2) Close frame.
        let close = tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .expect("ws recv (close) timed out")
            .expect("ws closed without sending Close")
            .expect("ws error");
        assert!(
            matches!(close, TMessage::Close(_)),
            "expected Close frame, got {:?}",
            close
        );

        // 3) Stream drains to None — pump has dropped the WS sink.
        let end = tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .expect("stream did not end after Close");
        assert!(
            end.is_none() || matches!(end, Some(Err(_))),
            "expected stream end after Close, got {:?}",
            end
        );
    }

    /// Bad JSON on the WS up path is logged + dropped (see daemon.rs:158
    /// note in the original handler). The pump itself must keep running:
    /// a following valid frame should arrive on the daemon side as if the
    /// garbage was never there.
    #[tokio::test]
    async fn bad_json_does_not_kill_pump() {
        let (mut daemon_side, server_side) = tokio::io::duplex(8192);
        let (ping, pong) = long_window();
        let addr = boot_pump(server_side, ping, pong).await;

        let url = format!("ws://{}/pump", addr);
        let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

        // 1) Garbage. Must be dropped silently — no bincode frame appears
        //    on `daemon_side`.
        ws.send(TMessage::Text("not valid json".into()))
            .await
            .unwrap();

        // Probe: a short read on the daemon side must time out. If a
        // bincode frame had been emitted, `read_frame` would return Ok
        // immediately.
        let probe = tokio::time::timeout(
            Duration::from_millis(150),
            read_frame::<ClientMsg, _>(&mut daemon_side),
        )
        .await;
        assert!(
            probe.is_err(),
            "bad JSON unexpectedly produced a daemon-side frame: {:?}",
            probe
        );

        // 2) Subsequent valid frame must arrive — proves pump is still
        //    pumping.
        let stdin = ClientMsg::Stdin(b"after-bad".to_vec());
        ws.send(TMessage::Text(serde_json::to_string(&stdin).unwrap()))
            .await
            .unwrap();
        let got: ClientMsg = tokio::time::timeout(
            Duration::from_secs(2),
            read_frame::<ClientMsg, _>(&mut daemon_side),
        )
        .await
        .expect("daemon-side read timed out after bad JSON")
        .expect("daemon-side read failed");
        match got {
            ClientMsg::Stdin(b) => assert_eq!(b, b"after-bad"),
            other => panic!("expected Stdin(b\"after-bad\"), got {:?}", other),
        }
    }
}
