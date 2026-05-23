//! `GET /api/terminals/:id` (WebSocket upgrade). **Owned by Track D.**
//!
//! ## Protocol
//!
//! Frames carry the `calm_session::ClientMsg` / `DaemonMsg` enums encoded as
//! JSON text. Each WS text frame is exactly one serde-JSON `ClientMsg` (going
//! up) or `DaemonMsg` (coming down). Binary WS frames are not used in this
//! bridge today — the wave's own xterm.js client handles VT replay on top of
//! `DaemonMsg::ServerHello.snapshot.data` / subsequent `RenderPatch.data`
//! byte arrays delivered as JSON byte-arrays. A future PR may introduce a
//! binary-frame fast path for `Input`; the wire format is reserved.
//!
//! This is intentionally a *thin* bridge: history, replay, seq numbering,
//! reconnect epochs etc. all live in the daemon (`ServerHello.snapshot` +
//! `RenderPatch` cursors) or are handled at the daemon attach layer.
//! Calm-server just shuttles frames.

use crate::db::{RepoOutOfDomain, RouteRepo};
use crate::error::Result;
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
use std::path::{Path as StdPath, PathBuf};
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
    let repo = s.repo.clone();
    ws.on_upgrade(move |socket| handle(socket, sock, id, repo))
        .into_response()
}

/// Resolve the socket path for a terminal row. Probe-only after the
/// #177 root-cause refactor (commit 3): if the daemon's `unix` socket
/// is unreachable, we surface 500 here and the browser's existing
/// "Reconnect" UI handles the retry — we do **not** spawn from the
/// WS handler anymore. The prior auto-revive lived here purely to
/// recover from a kernel restart while daemons were still running;
/// that one legitimate case is now handled at boot by
/// [`crate::revive_orphans_on_boot`], which sees a live `AppState`
/// and the terminal row's freshly stamped theme columns.
///
/// Why removed: the WS revive path had no access to the host
/// browser's theme at the moment a WS upgrade arrived, so its spawn
/// would race the row-creation spawn with mismatched argv. Removing
/// it leaves exactly one spawn path per row (the create-time spawn)
/// plus the boot-time orphan revival, both of which read theme from
/// the row.
async fn resolve_live_sock(s: &AppState, id: &str) -> Result<PathBuf> {
    let term = s
        .repo
        .terminal_get(id)
        .await?
        .ok_or_else(|| crate::error::CalmError::NotFound(format!("terminal {id}")))?;

    let handle = term.daemon_handle.as_ref().ok_or_else(|| {
        crate::error::CalmError::Internal(format!("terminal {id} has no live daemon"))
    })?;
    UnixStream::connect(handle).await.map_err(|e| {
        tracing::info!(
            terminal_id = %term.id,
            sock = %handle,
            error = %e,
            "daemon socket unreachable; WS upgrade rejected (browser will retry)"
        );
        crate::error::CalmError::Internal(format!("terminal {id}: daemon socket unreachable ({e})"))
    })?;
    Ok(PathBuf::from(handle))
}

async fn handle(socket: WebSocket, sock: PathBuf, terminal_id: String, repo: Arc<dyn RouteRepo>) {
    let stream = match UnixStream::connect(&sock).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, sock = ?sock, "connect daemon socket failed");
            return;
        }
    };
    let outcome = pump(
        socket,
        stream,
        terminal_id.clone(),
        PING_INTERVAL,
        PONG_TIMEOUT,
        // #177 PR2: hand the repo to the pump so the up-arm can persist
        // `TerminalThemeUpdate` frames onto the terminals row.
        Some(repo.clone()),
    )
    .await;
    if let PumpOutcome::FramingSkew { error } = outcome {
        // Stale daemon binary still bound to this socket. Clear the row's
        // `daemon_handle` and unlink the socket file so the next attach hits
        // `resolve_live_sock`'s spawn path with a clean slate. The old daemon
        // process exits on its own when its accept loop sees the socket gone
        // (no PID bookkeeping needed here).
        tracing::warn!(
            terminal_id = %terminal_id,
            sock = ?sock,
            error = %error,
            "framing skew — clearing stale daemon_handle and unlinking socket"
        );
        cleanup_stale_daemon(repo.as_ref(), &terminal_id, &sock).await;
    }
}

/// Drop the stale daemon's footprint after a framing-skew close: clear the
/// terminal row's `daemon_handle` and remove the socket file on disk. Pulled
/// into a free function so it can be exercised by a focused unit test
/// without standing up a full WS round-trip.
///
/// Errors are downgraded to warn-log: a row that's already been deleted
/// concurrently, or a socket file that another path already removed, are
/// both benign — the next attach will respawn either way.
pub(crate) async fn cleanup_stale_daemon(
    repo: &dyn RepoOutOfDomain,
    terminal_id: &str,
    sock: &StdPath,
) {
    if let Err(e) = repo.terminal_set_handle(terminal_id, None).await {
        tracing::warn!(
            terminal_id = %terminal_id,
            error = %e,
            "clearing daemon_handle after framing skew failed"
        );
    }
    match std::fs::remove_file(sock) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            tracing::warn!(
                terminal_id = %terminal_id,
                sock = ?sock,
                error = %e,
                "unlinking stale daemon socket failed"
            );
        }
    }
}

/// Outcome reported by [`pump`] when it returns. Lets [`handle`] decide
/// whether to perform stale-daemon cleanup (clear `daemon_handle`, unlink
/// the socket) before the connection fully tears down. Keeping the side
/// effects in `handle` (rather than threading the repo into `pump`) leaves
/// `pump` purely I/O-bound and easy to test against in-memory transports.
#[derive(Debug)]
pub enum PumpOutcome {
    /// Connection ended cleanly: client closed, daemon emitted
    /// `ChildExited`, heartbeat timed out, or one of the WS arms hit EOF.
    /// No socket-level cleanup is needed — the daemon either already exited
    /// (ChildExited) or is still healthy (client just walked away).
    Clean,
    /// The daemon read-half produced a framing error (bad magic or
    /// unsupported version). This means the bytes on the kernel↔daemon
    /// socket aren't from a current-protocol `calm-session-daemon`, so the
    /// row's `daemon_handle` is stale and must be cleared before the next
    /// attach.
    FramingSkew { error: FrameError },
}

/// Daemon transport abstraction. The WS bridge only needs the
/// bidirectional `AsyncRead + AsyncWrite` half — in production this is a
/// `tokio::net::UnixStream`; in tests it's one end of a
/// `tokio::io::duplex` pair so we can drive the pump in-process without
/// forking a real `calm-session-daemon`.
///
/// A blanket impl covers any type with the right combination of bounds;
/// callers don't need to opt in explicitly.
pub trait DaemonTransport:
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
pub async fn pump<T: DaemonTransport>(
    ws: WebSocket,
    daemon: T,
    terminal_id: String,
    ping_interval: Duration,
    pong_timeout: Duration,
    // #177 PR2 — optional repo handle so the up-arm can persist
    // `TerminalThemeUpdate` frames onto the `terminals` row, keeping
    // the next auto-revive's daemon argv in sync with the latest
    // mid-session theme toggle. Production passes `Some(repo)` from
    // `handle()`; the in-process `pump_tests` pass `None` because they
    // don't exercise the persistence side effect (covered by the
    // `terminal_theme_update_persisted` integration test instead).
    //
    // Typed `RouteRepo` (the trait object handlers already see via
    // `AppState::repo`) rather than `RepoOutOfDomain` so the existing
    // `Arc<dyn RouteRepo>` from `handle()` flows in without an
    // upcast dance. `RouteRepo: RepoOutOfDomain` already in the
    // supertrait chain — `terminal_set_theme` is callable directly.
    repo_for_theme: Option<Arc<dyn RouteRepo>>,
) -> PumpOutcome {
    let (mut rd, mut wr) = tokio::io::split(daemon);
    let (ws_tx, mut ws_rx) = ws.split();

    // Single-shot channel for the down arm to surface a `FramingSkew`
    // outcome up to the caller. The `Clean` case is the default — if no
    // arm sends, `try_recv` after the `select!` falls through to
    // `PumpOutcome::Clean`. Only the down arm uses the sender (it's the
    // only place that can observe a `FrameError`).
    let (outcome_tx, mut outcome_rx) = tokio::sync::oneshot::channel::<PumpOutcome>();

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
    let terminal_id_up = terminal_id.clone();
    let repo_for_theme_up = repo_for_theme.clone();
    let up = async move {
        while let Some(Ok(msg)) = ws_rx.next().await {
            // Any frame counts as liveness — pong, text, binary, ping.
            *last_seen_up.lock().await = Instant::now();
            match msg {
                Message::Text(text) => {
                    let mut parsed: ClientMsg = match serde_json::from_str(&text) {
                        Ok(m) => m,
                        Err(e) => {
                            tracing::warn!(error = %e, "unparseable ClientMsg JSON; dropping");
                            continue;
                        }
                    };
                    // SECURITY: this WS bridge is the untrusted-network
                    // ingress for daemon ClientMsg frames.
                    // `ClientCapabilities.kernel_originated_input` is a
                    // daemon-side trust flag that relaxes the owner-only
                    // gate on `ClientMsg::Input`; the only legitimate
                    // producer is a kernel-private `DaemonClient` speaking
                    // over a kernel-private unix domain socket. Any value
                    // arriving across this WS hop is, by definition,
                    // browser-controlled — strip it unconditionally so a
                    // forged ClientHello can't write to another user's
                    // PTY as an Observer. See
                    // `crates/calm-session/src/lib.rs` `ClientCapabilities`
                    // doc for the full trust model.
                    // #177 root-cause refactor — mid-session theme
                    // toggles still ship through to the live daemon via
                    // `write_frame` below; the daemon's
                    // `TerminalSession` owns the OSC reply / focus-in
                    // cycle and answers the next OSC probe with the
                    // updated RGB. We no longer persist the toggle to
                    // the `terminals` row: the row's `theme_fg/bg` are
                    // a row-creation invariant (NOT NULL via migration
                    // 0013) and reflect what the daemon was originally
                    // spawned with. A future respawn that reads from
                    // the row uses the create-time theme. If the user
                    // toggled mid-session and the daemon dies, the
                    // next respawn launches with the OLD theme and the
                    // user re-toggles — acceptable cost for removing
                    // the racing-write path. `repo_for_theme_up` stays
                    // wired for backward compat with the existing pump
                    // signature; commit 3 of the refactor drops it.
                    if let ClientMsg::TerminalThemeUpdate { fg, bg } = &parsed {
                        tracing::info!(
                            terminal_id = %terminal_id_up,
                            fg = ?fg, bg = ?bg,
                            "WS frame TerminalThemeUpdate — relaying to daemon"
                        );
                    }
                    let _ = &repo_for_theme_up; // retained until commit 3
                    if let ClientMsg::ClientHello {
                        ref mut capabilities,
                        ref mut terminal_id,
                        ..
                    } = parsed
                    {
                        capabilities.kernel_originated_input = false;
                        // CORRECTNESS: normalize terminal_id to hyphenated
                        // form so the daemon's byte-level handshake match
                        // succeeds regardless of which form the client
                        // sent. `model::new_id` returns the *simple* form
                        // (32 hex, no dashes) and the API response leaks
                        // that verbatim to the browser; `daemon.rs` renders
                        // its own `cli.id` via `Uuid` `Display`, which is
                        // always hyphenated, then does a string-equality
                        // check against the incoming `ClientHello.terminal_id`.
                        // Without this normalization, every browser hello
                        // would fail with `BadHandshake` — see the e2e test
                        // `crates/calm-server/tests/ws_terminal_e2e.rs` and
                        // the `ws_normalizes_terminal_id_to_hyphenated`
                        // regression test below.
                        //
                        // If the id isn't a valid UUID we leave it as-is and
                        // let the daemon reject it as `BadHandshake` — that
                        // is the correct fail-loud behavior for malformed
                        // input.
                        if let Ok(uuid) = uuid::Uuid::parse_str(terminal_id) {
                            *terminal_id = uuid.to_string();
                        }
                    }
                    if write_frame(&mut wr, &parsed).await.is_err() {
                        break;
                    }
                }
                // Binary WS frames are reserved for a future Input fast
                // path (PR-3). Today the pump drops them silently.
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
        let mut outcome_tx = Some(outcome_tx);
        loop {
            let msg: DaemonMsg = match read_frame(&mut rd).await {
                Ok(m) => m,
                Err(e) => {
                    // Version-skew on the kernel↔daemon Unix socket: this
                    // means a daemon binary was started against a stale
                    // `calm-session` schema. Log loudly with the daemon
                    // identity (terminal id + socket path) so an operator
                    // can correlate to the deploy that introduced the skew,
                    // then surface the skew up to `handle` (via
                    // `PumpOutcome::FramingSkew`) so it can clear the
                    // row's `daemon_handle` + unlink the socket file. The
                    // next attach to this terminal will then go through
                    // `resolve_live_sock`'s spawn path and start a fresh
                    // daemon binary.
                    let framing_skew = matches!(
                        &e,
                        FrameError::BadMagic { .. } | FrameError::UnsupportedFrameVersion { .. }
                    );
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
                    if framing_skew && let Some(tx) = outcome_tx.take() {
                        // Receiver lives in the outer `pump` body and is
                        // always polled after `select!` returns, so a
                        // send error here would mean the receiver was
                        // dropped — not possible without a programming
                        // bug. Discard the error for forward-compat.
                        let _ = tx.send(PumpOutcome::FramingSkew { error: e });
                    }
                    break;
                }
            };
            // Both terminal-mode TerminalExited and chat-mode ChildExited
            // signal end-of-session; the pump tears down the WS after
            // either lands.
            let exit = matches!(
                msg,
                DaemonMsg::TerminalExited { .. } | DaemonMsg::ChildExited { .. }
            );
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

    // Down arm is the only sender; everything else (up, heartbeat, EOF)
    // leaves the channel empty and `try_recv` yields
    // `Err(Empty)` → `Clean`. `Closed` (sender dropped without sending)
    // also maps to `Clean` for the same reason.
    match outcome_rx.try_recv() {
        Ok(outcome) => outcome,
        Err(_) => PumpOutcome::Clean,
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
    use calm_session::{
        ClientMsg, DaemonMsg, RenderEncoding, RenderPatch, read_frame, write_frame,
    };
    use futures_util::{SinkExt, StreamExt};
    use std::net::SocketAddr;
    use tokio::io::DuplexStream;
    use tokio::net::TcpListener;
    use tokio_tungstenite::tungstenite::Message as TMessage;

    /// Bring up a one-route axum app whose WS handler invokes [`pump`] with
    /// the supplied `daemon_side` of a duplex pair. Returns the bound
    /// address paired with a oneshot receiver that resolves to the
    /// [`PumpOutcome`] once `pump` returns; existing tests ignore the
    /// receiver, the framing-skew tests await it to assert on the outcome
    /// variant directly.
    ///
    /// Each call creates a fresh listener on `127.0.0.1:0` so concurrent
    /// tests don't share state. The server task is spawned and lives until
    /// the test ends; no cleanup needed because the runtime tears it down.
    pub(crate) async fn boot_pump(
        daemon_side: DuplexStream,
        ping: Duration,
        pong: Duration,
    ) -> (SocketAddr, tokio::sync::oneshot::Receiver<PumpOutcome>) {
        boot_pump_with_terminal_id(daemon_side, "test-terminal-1", ping, pong).await
    }

    /// Variant of [`boot_pump`] that pins the terminal id used by the WS
    /// route. Required for the v2 ClientHello round-trip test, which
    /// needs the daemon to validate the handshake against a known id.
    /// Returns the same `(addr, outcome_rx)` tuple as [`boot_pump`] so
    /// callers that care about framing-skew outcomes have the receiver
    /// available regardless of which variant they used to boot.
    pub(crate) async fn boot_pump_with_terminal_id(
        daemon_side: DuplexStream,
        terminal_id: &str,
        ping: Duration,
        pong: Duration,
    ) -> (SocketAddr, tokio::sync::oneshot::Receiver<PumpOutcome>) {
        // Wrap the DuplexStream in a Mutex<Option<…>> so the closure given
        // to `Router::route` can `take` it the first time the route is
        // hit. `on_upgrade` consumes the value by move; the option dance
        // is just to satisfy `Fn` (not `FnOnce`) while only firing once.
        let slot = Arc::new(Mutex::new(Some(daemon_side)));
        // Likewise for the outcome sender — `on_upgrade` is `FnOnce`-shaped
        // (consumes its captures) but `Router::route` needs `Fn`, so we
        // gate the move behind a `Mutex<Option<_>>` and `take` it the
        // first time the route fires.
        let (outcome_tx, outcome_rx) = tokio::sync::oneshot::channel();
        let outcome_slot = Arc::new(Mutex::new(Some(outcome_tx)));
        let terminal_id_str = terminal_id.to_string();
        let app = Router::new().route(
            "/pump",
            get(move |upgrade: WebSocketUpgrade| {
                let slot = slot.clone();
                let outcome_slot = outcome_slot.clone();
                let tid = terminal_id_str.clone();
                async move {
                    let daemon = slot
                        .lock()
                        .await
                        .take()
                        .expect("pump route called more than once");
                    let outcome_tx = outcome_slot
                        .lock()
                        .await
                        .take()
                        .expect("pump route called more than once");
                    upgrade.on_upgrade(move |socket| async move {
                        // #177 PR2: the in-process pump test fixture passes
                        // `None` because it doesn't stand up a real repo —
                        // mid-session theme persistence is covered by the
                        // `terminal_theme_update_persisted` integration test
                        // that DOES have a real `SqlxRepo`.
                        let outcome = pump(socket, daemon, tid, ping, pong, None).await;
                        // Receiver may have been dropped if the test exited
                        // before the pump did — that's fine, just discard.
                        let _ = outcome_tx.send(outcome);
                    })
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
        (addr, outcome_rx)
    }

    /// #177 PR2 — variant of [`boot_pump_with_terminal_id`] that hands
    /// a real repo into `pump` so the up-arm's
    /// `ClientMsg::TerminalThemeUpdate` interception can write to the
    /// `terminals` row. Returns the same `(addr, outcome_rx)` tuple;
    /// the repo is borrowed by the closure for its lifetime and the
    /// caller asserts the side effect via the same handle after
    /// driving the WS round-trip.
    pub(crate) async fn boot_pump_with_repo(
        daemon_side: DuplexStream,
        terminal_id: &str,
        repo: Arc<dyn RouteRepo>,
        ping: Duration,
        pong: Duration,
    ) -> (SocketAddr, tokio::sync::oneshot::Receiver<PumpOutcome>) {
        let slot = Arc::new(Mutex::new(Some(daemon_side)));
        let (outcome_tx, outcome_rx) = tokio::sync::oneshot::channel();
        let outcome_slot = Arc::new(Mutex::new(Some(outcome_tx)));
        let terminal_id_str = terminal_id.to_string();
        let app = Router::new().route(
            "/pump",
            get(move |upgrade: WebSocketUpgrade| {
                let slot = slot.clone();
                let outcome_slot = outcome_slot.clone();
                let tid = terminal_id_str.clone();
                let repo = repo.clone();
                async move {
                    let daemon = slot
                        .lock()
                        .await
                        .take()
                        .expect("pump route called more than once");
                    let outcome_tx = outcome_slot
                        .lock()
                        .await
                        .take()
                        .expect("pump route called more than once");
                    upgrade.on_upgrade(move |socket| async move {
                        let outcome = pump(socket, daemon, tid, ping, pong, Some(repo)).await;
                        let _ = outcome_tx.send(outcome);
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
        (addr, outcome_rx)
    }

    /// Build a minimal v2 RenderPatch from raw bytes for the down-arm
    /// translation test.
    fn render_patch(bytes: &[u8]) -> DaemonMsg {
        DaemonMsg::RenderPatch(RenderPatch {
            render_rev: 1,
            prev_render_rev: 0,
            pty_seq: 1,
            encoding: RenderEncoding::Vt,
            data: bytes.to_vec(),
        })
    }

    /// Build a minimal v2 ClientMsg::Input frame for the up-arm test.
    /// `input_seq` defaults to 0 — the browser-path "no ack requested"
    /// posture; the WS bridge does not synthesize seqs, so this matches
    /// what real browser traffic looks like on the daemon socket.
    fn client_input(bytes: &[u8]) -> ClientMsg {
        ClientMsg::Input {
            data: bytes.to_vec(),
            input_seq: 0,
        }
    }

    /// Big enough to never wake during a test. We don't want the heartbeat
    /// arm racing the assertions for up/down behavior.
    fn long_window() -> (Duration, Duration) {
        (Duration::from_secs(10), Duration::from_secs(60))
    }

    /// daemon → WS: write a `DaemonMsg::RenderPatch` on the duplex; the WS
    /// client must receive a JSON Text frame that round-trips back to the
    /// same `DaemonMsg`.
    #[tokio::test]
    async fn down_translates_daemon_frame_to_ws_text() {
        let (mut daemon_side, server_side) = tokio::io::duplex(8192);
        let (ping, pong) = long_window();
        let (addr, _outcome) = boot_pump(server_side, ping, pong).await;

        let url = format!("ws://{}/pump", addr);
        let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

        // Push one bincode frame from the "daemon" side.
        write_frame(&mut daemon_side, &render_patch(b"world"))
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
            DaemonMsg::RenderPatch(p) => assert_eq!(p.data, b"world"),
            other => panic!("expected RenderPatch(b\"world\"), got {:?}", other),
        }
    }

    /// WS → daemon: client pushes a Text frame containing
    /// `ClientMsg::Input`; the daemon side must observe one bincode-framed
    /// `ClientMsg` matching it.
    #[tokio::test]
    async fn up_translates_ws_text_to_daemon_frame() {
        let (mut daemon_side, server_side) = tokio::io::duplex(8192);
        let (ping, pong) = long_window();
        let (addr, _outcome) = boot_pump(server_side, ping, pong).await;

        let url = format!("ws://{}/pump", addr);
        let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

        let input = client_input(b"hello");
        let json = serde_json::to_string(&input).unwrap();
        ws.send(TMessage::Text(json)).await.unwrap();

        let got: ClientMsg = tokio::time::timeout(
            Duration::from_secs(2),
            read_frame::<ClientMsg, _>(&mut daemon_side),
        )
        .await
        .expect("daemon-side read timed out")
        .expect("daemon-side read failed");
        match got {
            ClientMsg::Input { data, input_seq } => {
                assert_eq!(data, b"hello");
                assert_eq!(input_seq, 0, "bridge must not synthesize seqs");
            }
            other => panic!("expected Input(b\"hello\"), got {:?}", other),
        }
    }

    /// When the daemon emits `TerminalExited`, the pump must (a) forward the
    /// frame as JSON, (b) send a WS Close, and (c) return. Asserting on
    /// the stream draining to `None` is the test's proxy for "pump
    /// returned".
    #[tokio::test]
    async fn child_exited_closes_ws_and_pump_returns() {
        let (mut daemon_side, server_side) = tokio::io::duplex(8192);
        let (ping, pong) = long_window();
        let (addr, _outcome) = boot_pump(server_side, ping, pong).await;

        let url = format!("ws://{}/pump", addr);
        let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

        write_frame(
            &mut daemon_side,
            &DaemonMsg::TerminalExited {
                code: Some(0),
                pty_seq: 7,
                render_rev: 7,
            },
        )
        .await
        .unwrap();
        // Drop our daemon-side writer so the down arm's read_frame would
        // hit EOF if the TerminalExited break didn't already trigger.
        drop(daemon_side);

        // 1) Text frame carrying the JSON TerminalExited.
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
            matches!(parsed, DaemonMsg::TerminalExited { code: Some(0), .. }),
            "expected TerminalExited, got {:?}",
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

    /// Bad JSON on the WS up path is logged + dropped. The pump itself
    /// must keep running: a following valid frame should arrive on the
    /// daemon side as if the garbage was never there.
    #[tokio::test]
    async fn bad_json_does_not_kill_pump() {
        let (mut daemon_side, server_side) = tokio::io::duplex(8192);
        let (ping, pong) = long_window();
        let (addr, _outcome) = boot_pump(server_side, ping, pong).await;

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
        let input = client_input(b"after-bad");
        ws.send(TMessage::Text(serde_json::to_string(&input).unwrap()))
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
            ClientMsg::Input { data, input_seq } => {
                assert_eq!(data, b"after-bad");
                assert_eq!(input_seq, 0, "bridge must not synthesize seqs");
            }
            other => panic!("expected Input(b\"after-bad\"), got {:?}", other),
        }
    }

    /// Daemon writes bytes whose first 4 don't match `NEIG` framing magic.
    /// The down arm's `read_frame` must return `FrameError::BadMagic`, which
    /// the pump translates to an error-log + Close + return. We assert the
    /// WS client sees the Close and the stream drains to None.
    #[tokio::test]
    async fn bad_magic_breaks_pump_cleanly() {
        use tokio::io::AsyncWriteExt;

        let (mut daemon_side, server_side) = tokio::io::duplex(8192);
        let (ping, pong) = long_window();
        let (addr, _outcome) = boot_pump(server_side, ping, pong).await;

        let url = format!("ws://{}/pump", addr);
        let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

        // Garbage magic: anything that isn't `NEIG`. Four bytes is the
        // exact width `read_frame` consumes before checking magic, so the
        // BadMagic branch fires deterministically without us having to
        // worry about partial-read interleaving.
        daemon_side.write_all(b"XXXX").await.unwrap();
        daemon_side.flush().await.unwrap();

        // Down arm should hit BadMagic, error-log, break, then send Close.
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

        // Stream drains — pump returned.
        let end = tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .expect("stream did not end after Close");
        assert!(
            end.is_none() || matches!(end, Some(Err(_))),
            "expected stream end after Close, got {:?}",
            end
        );
    }

    /// Daemon writes a syntactically well-formed framing prefix but with
    /// a version the kernel doesn't support (FRAME_VERSION+1). The down
    /// arm's `read_frame` must return `FrameError::UnsupportedFrameVersion`,
    /// which the pump translates to an error-log + Close + return.
    #[tokio::test]
    async fn unsupported_frame_version_breaks_pump_cleanly() {
        use tokio::io::AsyncWriteExt;

        let (mut daemon_side, server_side) = tokio::io::duplex(8192);
        let (ping, pong) = long_window();
        let (addr, _outcome) = boot_pump(server_side, ping, pong).await;

        let url = format!("ws://{}/pump", addr);
        let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

        // Magic `NEIG` (valid) + version=FRAME_VERSION+1 (big-endian,
        // unsupported) + length=0. read_frame validates magic first, then
        // version, so this drives the UnsupportedFrameVersion path before
        // ever touching the length / payload.
        let bogus_version = calm_session::FRAME_VERSION + 1;
        let mut wire = Vec::with_capacity(10);
        wire.extend_from_slice(b"NEIG");
        wire.extend_from_slice(&bogus_version.to_be_bytes());
        wire.extend_from_slice(&0u32.to_be_bytes());
        daemon_side.write_all(&wire).await.unwrap();
        daemon_side.flush().await.unwrap();

        // Down arm should hit UnsupportedFrameVersion, error-log, break,
        // then send Close.
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

        // Stream drains — pump returned.
        let end = tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .expect("stream did not end after Close");
        assert!(
            end.is_none() || matches!(end, Some(Err(_))),
            "expected stream end after Close, got {:?}",
            end
        );
    }

    /// Bad magic on the daemon side: `pump` must return
    /// `PumpOutcome::FramingSkew { error: FrameError::BadMagic { .. } }`
    /// so the caller (`handle`) can clear the stale `daemon_handle` and
    /// unlink the socket. We assert on the variant + the wrapped
    /// `FrameError` shape; `bad_magic_breaks_pump_cleanly` above asserts
    /// the WS-side semantics (Close frame + stream end).
    #[tokio::test]
    async fn pump_returns_framing_skew_on_bad_magic() {
        use tokio::io::AsyncWriteExt;

        let (mut daemon_side, server_side) = tokio::io::duplex(8192);
        let (ping, pong) = long_window();
        let (addr, outcome) = boot_pump(server_side, ping, pong).await;

        let url = format!("ws://{}/pump", addr);
        let (ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

        // Push 4 bytes that aren't `NEIG`. `read_frame` consumes magic
        // first, so this drives BadMagic deterministically.
        daemon_side.write_all(b"XXXX").await.unwrap();
        daemon_side.flush().await.unwrap();

        // Drop the client so the up arm sees `None` from `ws_rx.next()`
        // and exits — otherwise `pump`'s `select!` could linger waiting on
        // the up arm even after the down arm broke out on BadMagic.
        drop(ws);

        let got = tokio::time::timeout(Duration::from_secs(2), outcome)
            .await
            .expect("pump did not return within timeout")
            .expect("outcome sender dropped without sending");
        match got {
            PumpOutcome::FramingSkew { error } => {
                assert!(
                    matches!(
                        error,
                        FrameError::BadMagic {
                            got: [b'X', b'X', b'X', b'X'],
                            expected: _,
                        }
                    ),
                    "expected BadMagic with got=b\"XXXX\", got {:?}",
                    error
                );
            }
            other => panic!("expected FramingSkew, got {:?}", other),
        }
    }

    /// Unsupported framing version: `pump` must return
    /// `PumpOutcome::FramingSkew { error: FrameError::UnsupportedFrameVersion { got: 1, supported: FRAME_VERSION } }`.
    /// Pushing the legacy v1 framing prefix exercises the skew path
    /// (older daemon binary still bound to a row whose kernel has since
    /// upgraded). Asserts against the current `FRAME_VERSION` so the
    /// test stays correct across version bumps (#177 raised it from 2
    /// → 3).
    #[tokio::test]
    async fn pump_returns_framing_skew_on_unsupported_version() {
        use tokio::io::AsyncWriteExt;

        let (mut daemon_side, server_side) = tokio::io::duplex(8192);
        let (ping, pong) = long_window();
        let (addr, outcome) = boot_pump(server_side, ping, pong).await;

        let url = format!("ws://{}/pump", addr);
        let (ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

        // Valid magic + version=1 (legacy, no longer supported) + length=0.
        // Magic check passes, version check fails. We never touch the
        // (empty) payload region.
        let mut wire = Vec::with_capacity(10);
        wire.extend_from_slice(b"NEIG");
        wire.extend_from_slice(&1u16.to_be_bytes());
        wire.extend_from_slice(&0u32.to_be_bytes());
        daemon_side.write_all(&wire).await.unwrap();
        daemon_side.flush().await.unwrap();

        drop(ws);

        let got = tokio::time::timeout(Duration::from_secs(2), outcome)
            .await
            .expect("pump did not return within timeout")
            .expect("outcome sender dropped without sending");
        let expected = calm_session::FRAME_VERSION;
        match got {
            PumpOutcome::FramingSkew { error } => match error {
                FrameError::UnsupportedFrameVersion { got, supported } => {
                    assert_eq!(got, 1);
                    assert_eq!(supported, expected);
                }
                other => panic!(
                    "expected UnsupportedFrameVersion {{ got: 1, supported: {} }}, got {:?}",
                    expected, other,
                ),
            },
            other => panic!("expected FramingSkew, got {:?}", other),
        }
    }

    /// Normal close path (daemon sends `ChildExited`): `pump` must return
    /// `PumpOutcome::Clean`. The kernel must NOT clear `daemon_handle` in
    /// this case — the daemon process exits on its own and a fresh attach
    /// will see the empty handle and respawn through the cold path; we
    /// just don't want to *force* cleanup on every healthy exit.
    #[tokio::test]
    async fn pump_returns_clean_on_child_exited() {
        let (mut daemon_side, server_side) = tokio::io::duplex(8192);
        let (ping, pong) = long_window();
        let (addr, outcome) = boot_pump(server_side, ping, pong).await;

        let url = format!("ws://{}/pump", addr);
        let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

        write_frame(&mut daemon_side, &DaemonMsg::ChildExited { code: Some(0) })
            .await
            .unwrap();
        drop(daemon_side);

        // Drain WS to let the up arm exit (drop the client → up sees None).
        // Read both frames so the close handshake completes.
        let _ = tokio::time::timeout(Duration::from_secs(2), ws.next()).await;
        let _ = tokio::time::timeout(Duration::from_secs(2), ws.next()).await;
        drop(ws);

        let got = tokio::time::timeout(Duration::from_secs(2), outcome)
            .await
            .expect("pump did not return within timeout")
            .expect("outcome sender dropped without sending");
        assert!(
            matches!(got, PumpOutcome::Clean),
            "expected Clean on ChildExited, got {:?}",
            got
        );
    }

    /// #177 root-cause refactor — the up arm still relays
    /// `ClientMsg::TerminalThemeUpdate` to the live daemon, but no
    /// longer persists it to the row. The row's theme is a creation
    /// invariant (NOT NULL); mid-session toggles are ephemeral to
    /// the current daemon, and a future respawn re-reads the create-
    /// time theme. This test pins the relay-only contract: the
    /// daemon side observes the frame, the row stays unchanged.
    #[tokio::test]
    async fn terminal_theme_update_relays_to_daemon_without_persisting() {
        use crate::db::prelude::*;
        use crate::db::sqlite::SqlxRepo;
        use crate::model::{NewCard, NewCove, NewTerminal, NewWave};

        let repo: Arc<SqlxRepo> = Arc::new(
            SqlxRepo::open("sqlite::memory:")
                .await
                .expect("open sqlite"),
        );
        let cove = repo
            .cove_create(NewCove {
                name: "tu".into(),
                color: "#000".into(),
                sort: None,
            })
            .await
            .unwrap();
        let wave = repo
            .wave_create(NewWave {
                cove_id: cove.id.clone(),
                title: "tu".into(),
                sort: None,
                theme: crate::routes::theme::RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        let card = repo
            .card_create(NewCard {
                wave_id: wave.id.clone(),
                kind: "codex".into(),
                sort: None,
                payload: serde_json::json!({}),
            })
            .await
            .unwrap();
        let term = repo
            .terminal_create(NewTerminal {
                card_id: card.id.clone(),
                program: "codex".into(),
                cwd: "/".into(),
                env: serde_json::json!({}),
                theme: crate::routes::theme::RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        // Snapshot the create-time theme — it MUST not move when
        // a mid-session toggle ships.
        let original_fg = term.theme_fg.clone();
        let original_bg = term.theme_bg.clone();

        let (mut daemon_side, server_side) = tokio::io::duplex(8192);
        let (ping, pong) = long_window();
        let term_id_for_pump = term.id.clone();
        let repo_for_pump: Arc<dyn RouteRepo> = repo.clone() as Arc<dyn RouteRepo>;
        let (addr, _outcome) =
            boot_pump_with_repo(server_side, &term_id_for_pump, repo_for_pump, ping, pong).await;

        let url = format!("ws://{}/pump", addr);
        let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

        let toggle = ClientMsg::TerminalThemeUpdate {
            fg: (10, 20, 30),
            bg: (240, 241, 242),
        };
        ws.send(TMessage::Text(serde_json::to_string(&toggle).unwrap()))
            .await
            .unwrap();

        // Daemon side observes the frame.
        let got: ClientMsg = tokio::time::timeout(
            Duration::from_secs(2),
            read_frame::<ClientMsg, _>(&mut daemon_side),
        )
        .await
        .expect("daemon-side read timed out")
        .expect("daemon-side read failed");
        match got {
            ClientMsg::TerminalThemeUpdate { fg, bg } => {
                assert_eq!(fg, (10, 20, 30));
                assert_eq!(bg, (240, 241, 242));
            }
            other => panic!("daemon expected TerminalThemeUpdate, got {other:?}"),
        }

        // Row stays at its create-time theme — no async persist
        // tokio::spawn to race; give the runtime a yield then read.
        tokio::time::sleep(Duration::from_millis(50)).await;
        let row = repo.terminal_get(&term.id).await.unwrap().unwrap();
        assert_eq!(row.theme_fg, original_fg);
        assert_eq!(row.theme_bg, original_bg);
    }
}

#[cfg(test)]
mod cleanup_tests {
    //! Unit tests for [`cleanup_stale_daemon`], the helper `handle` calls
    //! after a framing-skew pump exit. We exercise the success path
    //! against a real in-memory `SqlxRepo` and a real socket file on
    //! disk: the framing-skew path's whole point is to leave the next
    //! attach with `daemon_handle = None` + socket file gone, so the
    //! `resolve_live_sock` cold path can respawn.
    use super::*;
    use crate::db::sqlite::SqlxRepo;
    use crate::model::NewTerminal;
    use serde_json::json;
    use std::sync::Arc;

    /// Seed a cove + wave + card + terminal, stamp a fake daemon_handle
    /// onto the terminal row, drop a placeholder file at that path so
    /// `cleanup_stale_daemon` has something to unlink. Returns
    /// `(repo, terminal_id, sock_path)`. The repo is wrapped in `Arc`
    /// because the helper signature wants `&dyn RepoOutOfDomain` and
    /// `SqlxRepo: RepoOutOfDomain`.
    async fn seed_terminal_with_stale_handle() -> (Arc<SqlxRepo>, String, PathBuf) {
        use crate::db::prelude::*;
        use crate::model::{NewCard, NewCove, NewWave};

        let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
        let cove = repo
            .cove_create(NewCove {
                name: "c".into(),
                color: "#000".into(),
                sort: None,
            })
            .await
            .unwrap();
        let wave = repo
            .wave_create(NewWave {
                cove_id: cove.id,
                title: "w".into(),
                sort: None,
                theme: crate::routes::theme::RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        let card = repo
            .card_create(NewCard {
                wave_id: wave.id,
                kind: "terminal".into(),
                sort: None,
                payload: json!({}),
            })
            .await
            .unwrap();
        let term = repo
            .terminal_create(NewTerminal {
                card_id: card.id,
                program: "/bin/true".into(),
                cwd: "/tmp".into(),
                env: json!({}),
                theme: crate::routes::theme::RequestTheme::default_dark(),
            })
            .await
            .unwrap();

        // Materialize a fake socket file under tempdir + stamp its path
        // onto the row. The cleanup helper unlinks the file by path; it
        // doesn't care that the file isn't a real Unix socket.
        let dir = tempfile::tempdir().unwrap().keep();
        let sock = dir.join(format!("{}.sock", term.id));
        std::fs::write(&sock, b"stub").unwrap();
        let sock_str = sock.to_string_lossy().to_string();
        repo.terminal_set_handle(&term.id, Some(&sock_str))
            .await
            .unwrap();

        (repo, term.id, sock)
    }

    /// Happy path: cleanup clears `daemon_handle` and unlinks the socket
    /// file. After the call:
    ///   * `terminal_get(id).daemon_handle == None`
    ///   * the socket file no longer exists on disk
    /// This is exactly the post-state the next `resolve_live_sock` needs
    /// to take the spawn path instead of the probe-and-reuse path.
    #[tokio::test]
    async fn cleanup_stale_daemon_clears_handle_and_unlinks_socket() {
        use crate::db::prelude::*;
        let (repo, id, sock) = seed_terminal_with_stale_handle().await;

        // Sanity: pre-state has a handle + the file on disk.
        let before = repo.terminal_get(&id).await.unwrap().unwrap();
        assert!(
            before.daemon_handle.is_some(),
            "seed did not stamp daemon_handle"
        );
        assert!(sock.exists(), "seed did not materialize the socket file");

        // Act.
        cleanup_stale_daemon(repo.as_ref(), &id, &sock).await;

        // Assert: row's daemon_handle is cleared.
        let after = repo.terminal_get(&id).await.unwrap().unwrap();
        assert!(
            after.daemon_handle.is_none(),
            "expected daemon_handle = None after cleanup, got {:?}",
            after.daemon_handle
        );
        // Assert: the on-disk socket file is gone.
        assert!(
            !sock.exists(),
            "expected socket file at {:?} to be unlinked",
            sock
        );
    }

    /// Idempotent on missing socket: if the file has already been removed
    /// (concurrent sweeper run, manual cleanup, etc.), the helper must
    /// still clear `daemon_handle` and not panic. ErrorKind::NotFound on
    /// `remove_file` is swallowed by the helper.
    #[tokio::test]
    async fn cleanup_stale_daemon_tolerates_missing_socket() {
        use crate::db::prelude::*;
        let (repo, id, sock) = seed_terminal_with_stale_handle().await;
        // Pre-delete the socket file so the helper's remove_file call
        // hits NotFound.
        std::fs::remove_file(&sock).unwrap();
        assert!(!sock.exists());

        cleanup_stale_daemon(repo.as_ref(), &id, &sock).await;

        let after = repo.terminal_get(&id).await.unwrap().unwrap();
        assert!(
            after.daemon_handle.is_none(),
            "expected daemon_handle = None after cleanup, got {:?}",
            after.daemon_handle
        );
    }
}
