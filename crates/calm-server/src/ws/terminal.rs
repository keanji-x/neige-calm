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
use futures::{SinkExt, StreamExt};
use calm_session::{ClientMsg, DaemonMsg, read_frame, write_frame};
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
    ws.on_upgrade(move |socket| handle(socket, sock))
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
    let refreshed = s
        .repo
        .terminal_get(id)
        .await?
        .ok_or_else(|| crate::error::CalmError::Internal("terminal vanished after respawn".into()))?;
    let handle = refreshed.daemon_handle.ok_or_else(|| {
        crate::error::CalmError::Internal(format!(
            "terminal {id}: daemon_handle still missing after respawn"
        ))
    })?;
    Ok(PathBuf::from(handle))
}

async fn handle(socket: WebSocket, sock: PathBuf) {
    let stream = match UnixStream::connect(&sock).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, sock = ?sock, "connect daemon socket failed");
            return;
        }
    };
    let (mut rd, mut wr) = stream.into_split();
    let (ws_tx, mut ws_rx) = socket.split();

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
    let down = async move {
        loop {
            let msg: DaemonMsg = match read_frame(&mut rd).await {
                Ok(m) => m,
                Err(_) => break,
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

    // Heartbeat: ping every `PING_INTERVAL`; if `last_seen` is older than
    // `PONG_TIMEOUT`, log + close with 1011. Browsers don't expose pongs to
    // JS, but our `last_seen` is bumped on *any* frame, and clients ack
    // pings with pongs at the protocol layer — that's all we need for
    // server-side death detection. Exits when the socket gets closed (a
    // send error trips us out and the `select!` cancels the other arms).
    let ws_tx_hb = ws_tx.clone();
    let last_seen_hb = last_seen.clone();
    let heartbeat = run_heartbeat(ws_tx_hb, last_seen_hb, PING_INTERVAL, PONG_TIMEOUT);

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
        <Self as SinkExt<Message>>::send(self, msg).await.map_err(|_| ())
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
        matches!(
            msg,
            Message::Close(Some(CloseFrame { code: 1011, .. }))
        )
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
