//! `GET /api/terminals/:id` (WebSocket upgrade): a thin bridge shuttling `ClientMsg` / `DaemonMsg` as JSON
//! text frames; history, replay and reconnect epochs all live in the daemon.

use crate::error::Result;
use crate::model::Terminal;
use crate::state::AppState;
use crate::terminal_renderer::{ClientPumpContext, RendererEntry, run_client_pump};
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
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, mpsc};
// `tokio::time::Instant` so `tokio::time::pause()` in tests virtual-advances `elapsed()` with the interval ticks.
use tokio::time::Instant;

/// Well under the typical 60s idle-disconnect window of HTTP intermediaries; three attempts before [`PONG_TIMEOUT`].
const PING_INTERVAL: Duration = Duration::from_secs(10);

/// No frame of any kind from the client for this long closes the connection with 1011; one missed ping is tolerated.
const PONG_TIMEOUT: Duration = Duration::from_secs(30);

const PONG_TIMEOUT_REASON: &str = "no pong";

/// The JS client matches on this exact string to distinguish a clean child exit from a network-level disconnect.
pub(crate) const CLOSE_REASON_CHILD_EXITED: &str = "child-exited";

pub fn router() -> Router<AppState> {
    Router::new().route("/api/terminals/{id}", get(upgrade))
}

async fn upgrade(
    ws: WebSocketUpgrade,
    Path(id): Path<String>,
    State(s): State<AppState>,
) -> impl IntoResponse {
    match resolve_live_renderer(&s, &id).await {
        Ok(LiveRenderer::Alive(entry)) => ws
            .on_upgrade(move |socket| handle_renderer(socket, entry, id))
            .into_response(),
        Ok(LiveRenderer::ChildExited { exit_code }) => ws
            .on_upgrade(move |socket| send_child_exited_close(socket, exit_code))
            .into_response(),
        Err(e) => e.into_response(),
    }
}

/// What [`resolve_live_renderer_from_terminal`] found. `ChildExited` says only that NO renderer was
/// obtained on this call (exit recorded, reattach failed, no live PTY, probe error); it is not proof the process is dead.
pub(crate) enum LiveRenderer {
    Alive(Arc<RendererEntry>),
    ChildExited { exit_code: Option<i32> },
}

#[cfg(feature = "fixtures")]
pub enum TestLiveRenderer {
    Alive(Arc<RendererEntry>),
    ChildExited { exit_code: Option<i32> },
}

#[cfg(feature = "fixtures")]
pub async fn resolve_live_renderer_for_test(s: &AppState, id: &str) -> Result<TestLiveRenderer> {
    match resolve_live_renderer(s, id).await? {
        LiveRenderer::Alive(entry) => Ok(TestLiveRenderer::Alive(entry)),
        LiveRenderer::ChildExited { exit_code } => Ok(TestLiveRenderer::ChildExited { exit_code }),
    }
}

#[cfg(feature = "fixtures")]
pub async fn resolve_live_renderer_from_terminal_for_test(
    s: &AppState,
    term: Terminal,
) -> Result<TestLiveRenderer> {
    match resolve_live_renderer_from_terminal(s, term).await? {
        LiveRenderer::Alive(entry) => Ok(TestLiveRenderer::Alive(entry)),
        LiveRenderer::ChildExited { exit_code } => Ok(TestLiveRenderer::ChildExited { exit_code }),
    }
}

async fn resolve_live_renderer(s: &AppState, id: &str) -> Result<LiveRenderer> {
    let term = s
        .repo
        .terminal_get(id)
        .await?
        .ok_or_else(|| crate::error::CalmError::NotFound(format!("terminal {id}")))?;

    resolve_live_renderer_from_terminal(s, term).await
}

/// The registry entry for `term`, or a lazy reattach to the PTY the supervisor still runs (the
/// post-restart shape). `pub(crate)` for the terminal sweeper, which needs the same reattach before it can reap.
pub(crate) async fn resolve_live_renderer_from_terminal(
    s: &AppState,
    term: Terminal,
) -> Result<LiveRenderer> {
    if let Some(entry) = s.terminal_renderer.get(&term.id) {
        return Ok(LiveRenderer::Alive(entry));
    }

    // Lazy reattach bypasses OperationRuntime, so it shares DELETE's per-track fence; re-read after acquiring it.
    let initial_card = s
        .repo
        .card_get(term.card_id.as_str())
        .await?
        .ok_or_else(|| crate::error::CalmError::NotFound(format!("card {}", term.card_id)))?;
    let _track_delete_guard =
        crate::per_card_lock::lock_key(s.track_delete_locks(), initial_card.track_id.as_str())
            .await;
    let term = s
        .repo
        .terminal_get(term.id.as_str())
        .await?
        .ok_or_else(|| crate::error::CalmError::NotFound(format!("terminal {}", term.id)))?;
    let card = s
        .repo
        .card_get(term.card_id.as_str())
        .await?
        .ok_or_else(|| crate::error::CalmError::NotFound(format!("card {}", term.card_id)))?;
    if card.track_id != initial_card.track_id
        || s.repo.track_get(card.track_id.as_str()).await?.is_none()
    {
        return Err(crate::error::CalmError::NotFound(format!(
            "track {}",
            initial_card.track_id
        )));
    }

    if term.exit_code.is_some() {
        tracing::info!(
            terminal_id = %term.id,
            exit_code = ?term.exit_code,
            "terminal has no live renderer entry and row is exited",
        );
        return Ok(LiveRenderer::ChildExited {
            exit_code: term.exit_code,
        });
    }

    // Probe the supervisor first: `spawn_terminal_for` on a proc it does not know would SPAWN a fresh child
    // instead of reattaching.
    match crate::probe_supervisor_for_terminal(s, &term.id).await {
        Ok(true) => {
            tracing::info!(
                terminal_id = %term.id,
                "supervisor confirms live PTY; attempting lazy renderer reattach",
            );
            match crate::routes::terminal::spawn_terminal_for(
                s,
                &term,
                &term.program,
                &term.cwd,
                &term.env,
            )
            .await
            {
                Ok(entry) => Ok(LiveRenderer::Alive(entry)),
                Err(e) => {
                    tracing::warn!(
                        terminal_id = %term.id,
                        error = %e,
                        "ws upgrade: lazy renderer reattach failed; surfacing child-exit",
                    );
                    Ok(LiveRenderer::ChildExited { exit_code: None })
                }
            }
        }
        Ok(false) => {
            tracing::info!(
                terminal_id = %term.id,
                "no live PTY at supervisor and no exit recorded; surfacing child-exit without respawn",
            );
            Ok(LiveRenderer::ChildExited { exit_code: None })
        }
        Err(e) => {
            tracing::warn!(
                terminal_id = %term.id,
                error = %e,
                "supervisor probe failed; surfacing child-exit",
            );
            Ok(LiveRenderer::ChildExited { exit_code: None })
        }
    }
}

async fn handle_renderer(socket: WebSocket, entry: Arc<RendererEntry>, terminal_id: String) {
    let (incoming_tx, incoming_rx) = mpsc::channel::<ClientMsg>(64);
    let (outgoing_tx, mut outgoing_rx) = mpsc::channel::<DaemonMsg>(256);
    let event_rx = entry.subscribe();
    let ctx = ClientPumpContext {
        input_barrier: entry.handle.input_barrier.clone(),
        input_scope: crate::terminal_renderer::ClientInputScope::InteractiveUser,
        event_rx,
        event_tx: entry.handle.event_tx.clone(),
        render_plane: entry.handle.render_plane.clone(),
        exit: entry.exit.clone(),
        supervisor_tx: entry.handle.supervisor_tx.clone(),
        owner_registry: entry.handle.owner_registry.clone(),
        session_id: entry.handle.session_id,
        terminal_id: terminal_id.clone(),
    };
    let pump_task = tokio::spawn(async move {
        if let Err(e) = run_client_pump(incoming_rx, outgoing_tx, ctx).await {
            tracing::warn!(error = %e, "terminal renderer client pump ended with error");
        }
    });

    let (ws_tx, mut ws_rx) = socket.split();
    let ws_tx = Arc::new(Mutex::new(ws_tx));
    let last_seen = Arc::new(Mutex::new(Instant::now()));

    let last_seen_up = last_seen.clone();
    let up_terminal_id = terminal_id.clone();
    let up = async move {
        while let Some(Ok(msg)) = ws_rx.next().await {
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
                    sanitize_client_msg(&mut parsed);
                    if incoming_tx.send(parsed).await.is_err() {
                        break;
                    }
                }
                Message::Close(_) => break,
                Message::Binary(_) => {}
                _ => {}
            }
        }
        tracing::debug!(terminal_id = %up_terminal_id, "terminal WS upstream ended");
    };

    let ws_tx_down = ws_tx.clone();
    let down = async move {
        while let Some(msg) = outgoing_rx.recv().await {
            let exit = matches!(msg, DaemonMsg::TerminalExited { .. });
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
        let _ = ws_tx_down
            .lock()
            .await
            .send(Message::Close(Some(CloseFrame {
                code: 1000,
                reason: CLOSE_REASON_CHILD_EXITED.into(),
            })))
            .await;
    };

    let heartbeat = run_heartbeat(ws_tx.clone(), last_seen, PING_INTERVAL, PONG_TIMEOUT);
    tokio::select! {
        _ = up => {}
        _ = down => {}
        _ = heartbeat => {}
    }
    if let Err(e) = pump_task.await {
        tracing::warn!(error = %e, "terminal renderer client pump join failed");
    }
}

fn sanitize_client_msg(parsed: &mut ClientMsg) {
    if let ClientMsg::ClientHello {
        capabilities,
        terminal_id,
        ..
    } = parsed
    {
        capabilities.kernel_originated_input = false;
        if let Ok(uuid) = uuid::Uuid::parse_str(terminal_id) {
            *terminal_id = uuid.simple().to_string();
        }
    }
}

/// Accept the upgrade, optionally send a JSON `TerminalExited` with the sidecar exit code, then `Close(1000,
/// "child-exited")`. `exit_code: None` skips the JSON frame: a `code: null` frame would clobber `signal_killed` on the client.
async fn send_child_exited_close(mut socket: WebSocket, exit_code: Option<i32>) {
    if let Some(code) = exit_code {
        // Match the on-the-wire shape produced by the pump path (serde external tagging).
        let msg = DaemonMsg::TerminalExited {
            code: Some(code),
            pty_seq: 0,
            render_rev: 0,
        };
        match serde_json::to_string(&msg) {
            Ok(text) => {
                if let Err(e) = socket.send(Message::Text(text.into())).await {
                    tracing::debug!(
                        error = %e,
                        "send_child_exited_close: TerminalExited send failed (client may have hung up)",
                    );
                    // Fall through to the close attempt anyway — best effort.
                }
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "send_child_exited_close: serializing TerminalExited failed",
                );
            }
        }
    }
    let _ = socket
        .send(Message::Close(Some(CloseFrame {
            code: 1000,
            reason: CLOSE_REASON_CHILD_EXITED.into(),
        })))
        .await;
}

/// Outcome reported by [`pump`]; side effects stay in the caller so `pump` is purely I/O-bound.
#[derive(Debug)]
pub enum PumpOutcome {
    /// Connection ended cleanly; no socket-level cleanup is needed.
    Clean,
    /// The bytes on the kernel↔daemon socket are not the current renderer protocol: the renderer entry is stale
    /// and must be cleared before the next attach.
    FramingSkew { error: FrameError },
}

/// Renderer transport: a `UnixStream` in production, one end of a `tokio::io::duplex` pair in tests.
pub trait DaemonTransport:
    tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + 'static
{
}

impl<T> DaemonTransport for T where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + 'static
{
}

/// Core WS↔daemon bridge: up, down and heartbeat arms in one `select!`; exits as soon as any arm completes.
pub async fn pump<T: DaemonTransport>(
    ws: WebSocket,
    daemon: T,
    terminal_id: String,
    ping_interval: Duration,
    pong_timeout: Duration,
) -> PumpOutcome {
    let (mut rd, mut wr) = tokio::io::split(daemon);
    let (ws_tx, mut ws_rx) = ws.split();

    // Only the down arm can observe a `FrameError`; no send means `Clean`.
    let (outcome_tx, mut outcome_rx) = tokio::sync::oneshot::channel::<PumpOutcome>();

    let ws_tx = Arc::new(Mutex::new(ws_tx));

    // Most recent frame of any kind from the client; any traffic proves the socket is alive.
    let last_seen = Arc::new(Mutex::new(Instant::now()));

    // WS → daemon.
    let last_seen_up = last_seen.clone();
    let up = async move {
        while let Some(Ok(msg)) = ws_rx.next().await {
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
                    // SECURITY: `kernel_originated_input` relaxes the daemon's owner-only gate on `Input` and is only legitimate
                    // from a kernel-private socket; anything arriving over this WS hop is browser-controlled, so strip it.
                    if let ClientMsg::ClientHello {
                        ref mut capabilities,
                        ref mut terminal_id,
                        ..
                    } = parsed
                    {
                        capabilities.kernel_originated_input = false;
                        // Normalize terminal_id to hyphenated form: the API leaks the simple (dashless) form, but the daemon compares
                        // against its hyphenated `Uuid` Display. A non-UUID is left as-is for the daemon to reject as `BadHandshake`.
                        if let Ok(uuid) = uuid::Uuid::parse_str(terminal_id) {
                            *terminal_id = uuid.to_string();
                        }
                    }
                    if write_frame(&mut wr, &parsed).await.is_err() {
                        break;
                    }
                }
                // Binary WS frames are reserved; dropped silently.
                Message::Binary(_) => {}
                Message::Close(_) => break,
                // axum auto-responds to client Ping; `last_seen` is already bumped above.
                _ => {}
            }
        }
    };

    // Daemon → WS.
    let ws_tx_down = ws_tx.clone();
    let terminal_id_down = terminal_id.clone();
    let down = async move {
        let mut outcome_tx = Some(outcome_tx);
        // Only a true protocol violation sends `Close(None)`; every other exit is attributable to a child that has
        // gone away and sends `Close(1000, CLOSE_REASON_CHILD_EXITED)`, or the browser sees 1005 on EOF.
        let mut framing_skew = false;
        loop {
            let msg: DaemonMsg = match read_frame(&mut rd).await {
                Ok(m) => m,
                Err(e) => {
                    // Version skew means a daemon binary was started against a stale `calm-session` schema; surface it to `handle`
                    // so it clears the renderer entry and the next attach spawns a fresh daemon.
                    framing_skew = matches!(
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
                        // Debug-log only: EOF shows up as Io on normal peer-close paths.
                        other => {
                            tracing::debug!(
                                terminal_id = %terminal_id_down,
                                error = %other,
                                "daemon read_frame ended"
                            );
                        }
                    }
                    if framing_skew && let Some(tx) = outcome_tx.take() {
                        // The receiver is always polled after `select!`, so a send error would be a programming bug.
                        let _ = tx.send(PumpOutcome::FramingSkew { error: e });
                    }
                    break;
                }
            };
            let exit = matches!(msg, DaemonMsg::TerminalExited { .. });
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
        let close_frame = if framing_skew {
            None
        } else {
            Some(CloseFrame {
                code: 1000,
                reason: CLOSE_REASON_CHILD_EXITED.into(),
            })
        };
        let _ = ws_tx_down
            .lock()
            .await
            .send(Message::Close(close_frame))
            .await;
    };

    // Browsers don't expose pongs to JS, but `last_seen` is bumped on any frame and clients pong at the protocol layer.
    let ws_tx_hb = ws_tx.clone();
    let last_seen_hb = last_seen.clone();
    let heartbeat = run_heartbeat(ws_tx_hb, last_seen_hb, ping_interval, pong_timeout);

    tokio::select! {
        _ = up => {}
        _ = down => {}
        _ = heartbeat => {}
    }

    // Down arm is the only sender; `Empty` and `Closed` both mean `Clean`.
    match outcome_rx.try_recv() {
        Ok(outcome) => outcome,
        Err(_) => PumpOutcome::Clean,
    }
}

/// Sink abstraction for the heartbeat task; `hb_send` avoids clashing with `futures::SinkExt::send`.
#[async_trait::async_trait]
pub(crate) trait HeartbeatSink: Send + 'static {
    async fn hb_send(&mut self, msg: Message) -> std::result::Result<(), ()>;
}

#[async_trait::async_trait]
impl HeartbeatSink for futures::stream::SplitSink<WebSocket, Message> {
    async fn hb_send(&mut self, msg: Message) -> std::result::Result<(), ()> {
        <Self as SinkExt<Message>>::send(self, msg)
            .await
            .map_err(|_| ())
    }
}

/// Pings at `ping_interval`; sends `Close(1011 "no pong")` and exits once `last_seen` is older than `pong_timeout`.
pub(crate) async fn run_heartbeat<S>(
    sink: Arc<Mutex<S>>,
    last_seen: Arc<Mutex<Instant>>,
    ping_interval: Duration,
    pong_timeout: Duration,
) where
    S: HeartbeatSink,
{
    let mut tick = tokio::time::interval(ping_interval);
    // First tick fires immediately by default — skip it.
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
        // An empty payload is the smallest valid ping.
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

    #[tokio::test]
    async fn closes_when_no_pong_within_timeout() {
        let sink = Arc::new(Mutex::new(VecSink(Vec::new())));
        let last_seen = Arc::new(Mutex::new(Instant::now()));
        let ping = Duration::from_millis(100);
        let pong = Duration::from_millis(300);

        let sink_clone = sink.clone();
        let ls_clone = last_seen.clone();
        let h = tokio::spawn(async move { run_heartbeat(sink_clone, ls_clone, ping, pong).await });

        let _ = tokio::time::timeout(Duration::from_millis(800), h).await;

        let log = &sink.lock().await.0;
        assert!(
            log.iter().any(is_close_1011),
            "expected a Close(1011) frame, got: {:?}",
            log
        );
    }

    #[tokio::test]
    async fn pings_continue_when_pongs_keep_coming() {
        let sink = Arc::new(Mutex::new(VecSink(Vec::new())));
        let last_seen = Arc::new(Mutex::new(Instant::now()));
        let ping = Duration::from_millis(50);
        let pong = Duration::from_millis(200);

        let sink_clone = sink.clone();
        let ls_clone = last_seen.clone();
        let h = tokio::spawn(async move { run_heartbeat(sink_clone, ls_clone, ping, pong).await });

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
    //! In-process bridge tests: [`pump`] under a one-route axum app driven by a `tokio_tungstenite` client, with a
    //! `tokio::io::duplex` pair standing in for the daemon socket. Heartbeat windows are kept huge so that arm never fires.
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

    /// Boot a one-route app whose WS handler runs [`pump`] on `daemon_side`; the receiver resolves to the [`PumpOutcome`].
    pub(crate) async fn boot_pump(
        daemon_side: DuplexStream,
        ping: Duration,
        pong: Duration,
    ) -> (SocketAddr, tokio::sync::oneshot::Receiver<PumpOutcome>) {
        boot_pump_with_terminal_id(daemon_side, "test-terminal-1", ping, pong).await
    }

    /// Variant of [`boot_pump`] that pins the terminal id used by the WS route.
    pub(crate) async fn boot_pump_with_terminal_id(
        daemon_side: DuplexStream,
        terminal_id: &str,
        ping: Duration,
        pong: Duration,
    ) -> (SocketAddr, tokio::sync::oneshot::Receiver<PumpOutcome>) {
        // `Mutex<Option<_>>` so the `Fn` route closure can move these out on its first (only) hit.
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
                        let outcome = pump(socket, daemon, tid, ping, pong).await;
                        // Receiver may have been dropped if the test exited before the pump did.
                        let _ = outcome_tx.send(outcome);
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
        (addr, outcome_rx)
    }

    fn render_patch(bytes: &[u8]) -> DaemonMsg {
        DaemonMsg::RenderPatch(RenderPatch {
            render_rev: 1,
            prev_render_rev: 0,
            pty_seq: 1,
            encoding: RenderEncoding::Vt,
            data: bytes.to_vec(),
        })
    }

    /// `input_seq: 0` is the browser-path "no ack requested" posture; the WS bridge does not synthesize seqs.
    fn client_input(bytes: &[u8]) -> ClientMsg {
        ClientMsg::Input {
            data: bytes.to_vec(),
            input_seq: 0,
        }
    }

    /// Big enough that the heartbeat arm never wakes during a test.
    fn long_window() -> (Duration, Duration) {
        (Duration::from_secs(10), Duration::from_secs(60))
    }

    #[tokio::test]
    async fn down_translates_daemon_frame_to_ws_text() {
        let (mut daemon_side, server_side) = tokio::io::duplex(8192);
        let (ping, pong) = long_window();
        let (addr, _outcome) = boot_pump(server_side, ping, pong).await;

        let url = format!("ws://{}/pump", addr);
        let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

        write_frame(&mut daemon_side, &render_patch(b"world"))
            .await
            .unwrap();

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

    /// The stream draining to `None` is the proxy for "pump returned".
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
        drop(daemon_side);

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

        let close = tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .expect("ws recv (close) timed out")
            .expect("ws closed without sending Close")
            .expect("ws error");
        match close {
            TMessage::Close(Some(cf)) => {
                assert_eq!(u16::from(cf.code), 1000, "expected 1000 normal close");
                assert_eq!(
                    cf.reason.as_ref(),
                    CLOSE_REASON_CHILD_EXITED,
                    "expected `child-exited` reason text"
                );
            }
            other => {
                panic!("expected Close(Some(CloseFrame {{1000, child-exited}})), got {other:?}")
            }
        }

        let end = tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .expect("stream did not end after Close");
        assert!(
            end.is_none() || matches!(end, Some(Err(_))),
            "expected stream end after Close, got {:?}",
            end
        );
    }

    /// Daemon EOF before any exit frame (the common case) must still close with `Close(1000, "child-exited")`,
    /// not `Close(None)`, which the browser surfaces as 1005.
    #[tokio::test]
    async fn eof_before_exit_frame_closes_with_child_exited() {
        let (daemon_side, server_side) = tokio::io::duplex(8192);
        let (ping, pong) = long_window();
        let (addr, _outcome) = boot_pump(server_side, ping, pong).await;

        let url = format!("ws://{}/pump", addr);
        let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

        drop(daemon_side);

        let close = tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .expect("ws recv (close) timed out")
            .expect("ws closed without sending Close")
            .expect("ws error");
        match close {
            TMessage::Close(Some(cf)) => {
                assert_eq!(u16::from(cf.code), 1000, "expected 1000 normal close");
                assert_eq!(
                    cf.reason.as_ref(),
                    CLOSE_REASON_CHILD_EXITED,
                    "expected `child-exited` reason text"
                );
            }
            other => {
                panic!("expected Close(Some(CloseFrame {{1000, child-exited}})), got {other:?}")
            }
        }

        let end = tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .expect("stream did not end after Close");
        assert!(
            end.is_none() || matches!(end, Some(Err(_))),
            "expected stream end after Close, got {:?}",
            end
        );
    }

    #[tokio::test]
    async fn bad_json_does_not_kill_pump() {
        let (mut daemon_side, server_side) = tokio::io::duplex(8192);
        let (ping, pong) = long_window();
        let (addr, _outcome) = boot_pump(server_side, ping, pong).await;

        let url = format!("ws://{}/pump", addr);
        let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

        ws.send(TMessage::Text("not valid json".into()))
            .await
            .unwrap();

        // Probe: a short read on the daemon side must time out.
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

    #[tokio::test]
    async fn bad_magic_breaks_pump_cleanly() {
        use tokio::io::AsyncWriteExt;

        let (mut daemon_side, server_side) = tokio::io::duplex(8192);
        let (ping, pong) = long_window();
        let (addr, _outcome) = boot_pump(server_side, ping, pong).await;

        let url = format!("ws://{}/pump", addr);
        let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

        // Four bytes is exactly the width `read_frame` consumes before checking magic, so BadMagic fires deterministically.
        daemon_side.write_all(b"XXXX").await.unwrap();
        daemon_side.flush().await.unwrap();

        // Framing skew is the only path that emits a bodyless `Close(None)`.
        let close = tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .expect("ws recv (close) timed out")
            .expect("ws closed without sending Close")
            .expect("ws error");
        assert!(
            matches!(close, TMessage::Close(None)),
            "expected Close(None) on framing skew, got {:?}",
            close
        );

        let end = tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .expect("stream did not end after Close");
        assert!(
            end.is_none() || matches!(end, Some(Err(_))),
            "expected stream end after Close, got {:?}",
            end
        );
    }

    #[tokio::test]
    async fn unsupported_frame_version_breaks_pump_cleanly() {
        use tokio::io::AsyncWriteExt;

        let (mut daemon_side, server_side) = tokio::io::duplex(8192);
        let (ping, pong) = long_window();
        let (addr, _outcome) = boot_pump(server_side, ping, pong).await;

        let url = format!("ws://{}/pump", addr);
        let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

        // Valid magic + unsupported version + length=0; magic is validated before version.
        let bogus_version = calm_session::FRAME_VERSION + 1;
        let mut wire = Vec::with_capacity(10);
        wire.extend_from_slice(b"NEIG");
        wire.extend_from_slice(&bogus_version.to_be_bytes());
        wire.extend_from_slice(&0u32.to_be_bytes());
        daemon_side.write_all(&wire).await.unwrap();
        daemon_side.flush().await.unwrap();

        let close = tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .expect("ws recv (close) timed out")
            .expect("ws closed without sending Close")
            .expect("ws error");
        assert!(
            matches!(close, TMessage::Close(None)),
            "expected Close(None) on framing skew, got {:?}",
            close
        );

        let end = tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .expect("stream did not end after Close");
        assert!(
            end.is_none() || matches!(end, Some(Err(_))),
            "expected stream end after Close, got {:?}",
            end
        );
    }

    #[tokio::test]
    async fn pump_returns_framing_skew_on_bad_magic() {
        use tokio::io::AsyncWriteExt;

        let (mut daemon_side, server_side) = tokio::io::duplex(8192);
        let (ping, pong) = long_window();
        let (addr, outcome) = boot_pump(server_side, ping, pong).await;

        let url = format!("ws://{}/pump", addr);
        let (ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

        daemon_side.write_all(b"XXXX").await.unwrap();
        daemon_side.flush().await.unwrap();

        // Drop the client so the up arm sees `None` and exits; otherwise `select!` could linger on it.
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

    /// Asserts against the current `FRAME_VERSION` so the test stays correct across version bumps.
    #[tokio::test]
    async fn pump_returns_framing_skew_on_unsupported_version() {
        use tokio::io::AsyncWriteExt;

        let (mut daemon_side, server_side) = tokio::io::duplex(8192);
        let (ping, pong) = long_window();
        let (addr, outcome) = boot_pump(server_side, ping, pong).await;

        let url = format!("ws://{}/pump", addr);
        let (ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

        // Valid magic + version=1 (legacy) + length=0.
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

    /// The kernel must not force cleanup on every healthy exit.
    #[tokio::test]
    async fn pump_returns_clean_on_terminal_exited() {
        let (mut daemon_side, server_side) = tokio::io::duplex(8192);
        let (ping, pong) = long_window();
        let (addr, outcome) = boot_pump(server_side, ping, pong).await;

        let url = format!("ws://{}/pump", addr);
        let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

        write_frame(
            &mut daemon_side,
            &DaemonMsg::TerminalExited {
                code: Some(0),
                pty_seq: 0,
                render_rev: 0,
            },
        )
        .await
        .unwrap();
        drop(daemon_side);

        let _ = tokio::time::timeout(Duration::from_secs(2), ws.next()).await;
        let _ = tokio::time::timeout(Duration::from_secs(2), ws.next()).await;
        drop(ws);

        let got = tokio::time::timeout(Duration::from_secs(2), outcome)
            .await
            .expect("pump did not return within timeout")
            .expect("outcome sender dropped without sending");
        assert!(
            matches!(got, PumpOutcome::Clean),
            "expected Clean on TerminalExited, got {:?}",
            got
        );
    }

    #[tokio::test]
    async fn upgrade_time_child_exited_no_code_emits_close_1000_only() {
        let (outcome_tx, outcome_rx) = tokio::sync::oneshot::channel::<()>();
        let outcome_slot = Arc::new(Mutex::new(Some(outcome_tx)));
        let app = Router::new().route(
            "/exit",
            get(move |upgrade: WebSocketUpgrade| {
                let outcome_slot = outcome_slot.clone();
                async move {
                    let outcome_tx = outcome_slot
                        .lock()
                        .await
                        .take()
                        .expect("route called more than once");
                    upgrade.on_upgrade(move |socket| async move {
                        super::send_child_exited_close(socket, None).await;
                        let _ = outcome_tx.send(());
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

        let url = format!("ws://{}/exit", addr);
        let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

        let close = tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .expect("ws recv (close) timed out")
            .expect("ws closed without sending Close")
            .expect("ws error");
        match close {
            TMessage::Close(Some(cf)) => {
                assert_eq!(u16::from(cf.code), 1000, "expected 1000 normal close");
                assert_eq!(
                    cf.reason.as_ref(),
                    CLOSE_REASON_CHILD_EXITED,
                    "expected `child-exited` reason text"
                );
            }
            other => panic!("expected Close(1000, child-exited), got {other:?}"),
        }
        let _ = tokio::time::timeout(Duration::from_secs(2), outcome_rx)
            .await
            .expect("send_child_exited_close did not return");
    }

    /// Without the JSON frame the client falls back to the close-frame backstop and renders a neutral "exit" badge instead of "exit 0".
    #[tokio::test]
    async fn upgrade_time_child_exited_with_code_emits_terminal_exited_then_close() {
        let (outcome_tx, outcome_rx) = tokio::sync::oneshot::channel::<()>();
        let outcome_slot = Arc::new(Mutex::new(Some(outcome_tx)));
        let app = Router::new().route(
            "/exit",
            get(move |upgrade: WebSocketUpgrade| {
                let outcome_slot = outcome_slot.clone();
                async move {
                    let outcome_tx = outcome_slot
                        .lock()
                        .await
                        .take()
                        .expect("route called more than once");
                    upgrade.on_upgrade(move |socket| async move {
                        super::send_child_exited_close(socket, Some(0)).await;
                        let _ = outcome_tx.send(());
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

        let url = format!("ws://{}/exit", addr);
        let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

        let first = tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .expect("ws recv (text) timed out")
            .expect("ws closed before sending TerminalExited")
            .expect("ws error");
        match first {
            TMessage::Text(t) => {
                let parsed: DaemonMsg = serde_json::from_str(&t)
                    .unwrap_or_else(|e| panic!("parsing JSON failed for {t}: {e}"));
                assert!(
                    matches!(parsed, DaemonMsg::TerminalExited { code: Some(0), .. }),
                    "expected TerminalExited {{ code: Some(0), .. }}, got {parsed:?}"
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
                assert_eq!(cf.reason.as_ref(), CLOSE_REASON_CHILD_EXITED);
            }
            other => panic!("expected Close(1000, child-exited), got {other:?}"),
        }
        let _ = tokio::time::timeout(Duration::from_secs(2), outcome_rx)
            .await
            .expect("send_child_exited_close did not return");
    }
}
