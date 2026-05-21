//! calm-session-daemon — per-session supervisor.
//!
//! Two modes share the same binary, socket, and framing:
//!
//! - **Terminal mode** (default): spawn the user's program under a PTY.
//!   PR-2 introduced a server-side VT model (`RenderPlane` →
//!   `TerminalModel` in `calm-session/src/terminal_model.rs`) that
//!   parses every PTY byte into a cell grid + scrollback, and emits
//!   geometry-bound `RenderSnapshot` / `RenderPatch` frames on
//!   `ClientHello` and PTY chunks respectively. Patch `data` is still
//!   the raw PTY bytes (`encoding = Vt`) so xterm.js can drive its own
//!   grid; the snapshot is the model's serialized viewport at the
//!   client's `desired_size`. Resizes broadcast a fresh `RenderSnapshot`.
//!
//! - **Chat mode** (`--mode chat`): spawn the Node sidecar runner
//!   (`runners/neige-chat-runner/cli.js`) under
//!   `node <runner-path> --session-id <uuid> --cwd <cwd> [--resume]
//!   [--mcp-config <path>] [--program <prog>]`. The runner uses
//!   `@anthropic-ai/claude-agent-sdk` and emits one already-serialized
//!   `NeigeEvent` JSON per stdout line, which the daemon forwards
//!   opaquely (no parsing) into the replay buffer + broadcast. Control
//!   frames sent the other direction are NDJSON lines on the runner's
//!   stdin: `{"kind":"user_message","content":...}`,
//!   `{"kind":"stop"}`, or
//!   `{"kind":"answer_question","question_id":...,"answers":{...}}`.
//!
//! Both modes survive all client disconnects; both exit when the child does.

use std::collections::{HashMap, VecDeque};
use std::io::Write as _;
use std::os::unix::io::FromRawFd;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use clap::{Parser, ValueEnum};
use portable_pty::{CommandBuilder, MasterPty, PtySize as PtPtySize, native_pty_system};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::process::{ChildStdin, Command};
use tokio::sync::{broadcast, mpsc, oneshot};
use uuid::Uuid;

use calm_session::terminal_model::ScrollbackLimit;
use calm_session::terminal_session::{
    Effect, OwnerRegistry, RenderPlane, SessionContext, TerminalSessionState,
};
use calm_session::{ClientMsg, DaemonMsg, PtySize, read_frame, write_frame};

/// One work item on the PTY-writer channel. Carries the bytes to write
/// plus the metadata needed to ack the originating connection after the
/// write completes.
///
/// `ack` is the per-client mpsc sender into the connection that produced
/// the [`ClientMsg::Input`]; the writer thread fires
/// [`DaemonMsg::InputAck`] back through it after `write_all + flush`
/// returns. `ack` is `None` when the originating frame had `input_seq ==
/// 0` (browser-typing default, "no ack requested") — the writer still
/// performs the PTY write but skips the ack to avoid frame-noise on the
/// hot browser path. See [`calm_session::ClientMsg::Input`] for the
/// design rationale (option (b) from issue #115).
struct PtyWrite {
    data: Vec<u8>,
    input_seq: u64,
    ack: Option<mpsc::UnboundedSender<DaemonMsg>>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum Mode {
    /// PTY-backed terminal session. Daemon proxies raw bytes both ways.
    Terminal,
    /// Headless stream-json child (e.g. `claude --print`). Daemon parses
    /// stdout as NDJSON and forwards typed events.
    Chat,
}

#[derive(Parser, Debug)]
#[command(
    name = "calm-session-daemon",
    about = "Per-session supervisor for neige-calm"
)]
struct Cli {
    /// Session ID. Used for logging and (by convention) socket path.
    #[arg(long)]
    id: Uuid,

    /// Unix socket path to listen on. Parent directory is created if missing.
    #[arg(long)]
    sock: PathBuf,

    /// Replay buffer size in bytes. In terminal mode this caps the rolling
    /// window of recent PTY output. In chat mode it caps the cumulative
    /// serialized size of buffered NeigeEvent JSON lines.
    #[arg(long, default_value_t = 1024 * 1024)]
    buffer_bytes: usize,

    /// Initial PTY columns. First Attach resizes to the real client size.
    /// Ignored in chat mode.
    #[arg(long, default_value_t = 80)]
    cols: u16,

    /// Initial PTY rows. Ignored in chat mode.
    #[arg(long, default_value_t = 24)]
    rows: u16,

    /// Working directory for the spawned child. Defaults to the daemon's cwd.
    #[arg(long)]
    cwd: Option<PathBuf>,

    /// File descriptor to write "ready\n" to after the socket is bound.
    /// The parent passes an open pipe fd here so it can block until we're
    /// accepting connections without racing to stat(2) the socket path.
    #[arg(long)]
    ready_fd: Option<i32>,

    /// Session mode. Default is `terminal` (PTY); `chat` spawns the Node
    /// sidecar runner under `node <runner-path> ...` and forwards NDJSON
    /// stdout opaquely.
    #[arg(long, value_enum, default_value_t = Mode::Terminal)]
    mode: Mode,

    /// Path to `runners/neige-chat-runner/cli.js`. Daemon spawns
    /// `node <runner-path> --session-id <id> --cwd <cwd> ...`. Required
    /// when `--mode chat`; ignored otherwise.
    #[arg(long)]
    runner_path: Option<PathBuf>,

    /// If set, the runner is asked to resume the prior agent session for
    /// `--id` (`--resume` is forwarded on the runner's argv). Daemon
    /// itself doesn't persist anything; the parent decides this. Chat
    /// mode only.
    #[arg(long, default_value_t = false)]
    resume: bool,

    /// Optional `--mcp-config <path>` forwarded to the runner. Chat mode
    /// only.
    #[arg(long)]
    mcp_config: Option<PathBuf>,

    /// Optional `--program <name>` forwarded to the runner (informational
    /// label, e.g. "claude-code"). Chat mode only.
    #[arg(long)]
    program: Option<String>,

    /// Program and args to run **in terminal mode only**. Use `--` to
    /// separate from daemon flags. Required when `--mode terminal`;
    /// ignored when `--mode chat` (the chat-mode argv is built from
    /// `--runner-path`, `--id`, `--cwd`, `--resume`, `--mcp-config`,
    /// `--program`).
    #[arg(last = true)]
    cmd: Vec<String>,
}

/// Events fanned out in chat mode. Each `Event` here is one already-
/// serialized `NeigeEvent` JSON line.
#[derive(Clone, Debug)]
enum ChatEvt {
    Event(String),
    Exit(Option<i32>),
}

/// Same chunk-granular eviction strategy as the terminal-mode [`ByteRing`]
/// (in `calm_session::terminal_session`), but each chunk is
/// one serialized NeigeEvent JSON line. Used in chat mode.
struct EventBuffer {
    events: VecDeque<String>,
    total_bytes: usize,
    max_bytes: usize,
}

impl EventBuffer {
    fn new(max_bytes: usize) -> Self {
        Self {
            events: VecDeque::new(),
            total_bytes: 0,
            max_bytes,
        }
    }

    fn append(&mut self, json: String) {
        self.total_bytes += json.len();
        self.events.push_back(json);
        while self.total_bytes > self.max_bytes && self.events.len() > 1 {
            let dropped = self.events.pop_front().unwrap();
            self.total_bytes -= dropped.len();
        }
    }

    fn snapshot(&self) -> Vec<String> {
        self.events.iter().cloned().collect()
    }
}

/// Shared render plane. Owns the server-side [`TerminalModel`] (VT-driven
/// grid + scrollback) and the transcript byte ring. Each PTY chunk feeds
/// the model and produces an [`Effect::Broadcast`] carrying a
/// `RenderPatch{ encoding: Vt, data: raw bytes, render_rev: model.rev() }`.
/// The IO shell pushes those onto `event_tx`.
type SharedRenderPlane = Arc<Mutex<RenderPlane>>;
type SharedEventBuffer = Arc<Mutex<EventBuffer>>;
type SharedMaster = Arc<Mutex<Box<dyn MasterPty + Send>>>;
type SharedOwnerRegistry = Arc<Mutex<OwnerRegistry>>;

/// Default scrollback line cap for the terminal model. Mirrors xterm's
/// vanilla default. Surfaced as a constant so we can parameterize later
/// without re-threading through every call site.
const SCROLLBACK_MAX_LINES: usize = 2000;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    tracing::info!(
        id = %cli.id,
        mode = ?cli.mode,
        cmd = ?cli.cmd,
        runner_path = ?cli.runner_path,
        resume = cli.resume,
        "starting daemon"
    );

    match cli.mode {
        Mode::Terminal => {
            if cli.cmd.is_empty() {
                anyhow::bail!("--mode terminal requires a `-- <program> [args...]` trailing argv");
            }
            run_terminal(cli).await
        }
        Mode::Chat => {
            if cli.runner_path.is_none() {
                anyhow::bail!("--mode chat requires --runner-path <path-to-cli.js>");
            }
            run_chat(cli).await
        }
    }
}

async fn run_terminal(cli: Cli) -> anyhow::Result<()> {
    // ---- PTY + child ----
    let pty_system = native_pty_system();
    let pair = pty_system.openpty(PtPtySize {
        rows: cli.rows,
        cols: cli.cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;
    let mut cmd = CommandBuilder::new(&cli.cmd[0]);
    for a in &cli.cmd[1..] {
        cmd.arg(a);
    }
    if let Some(cwd) = &cli.cwd {
        cmd.cwd(cwd);
    }
    // Forward every env var we have to the child. The caller (neige-server)
    // sets the env it wants (TERM, COLORTERM, proxy vars, ...) when it spawns
    // us, and the child should see the same environment.
    for (k, v) in std::env::vars() {
        cmd.env(k, v);
    }
    let child = pair.slave.spawn_command(cmd)?;
    // Split out a separately-owned killer before the child moves into the
    // waiter task. A ClientMsg::Kill handler calls through this.
    let killer: Arc<Mutex<Box<dyn portable_pty::ChildKiller + Send + Sync>>> =
        Arc::new(Mutex::new(child.clone_killer()));
    drop(pair.slave);

    let render_plane: SharedRenderPlane = Arc::new(Mutex::new(RenderPlane::new(
        cli.cols,
        cli.rows,
        cli.buffer_bytes,
        SCROLLBACK_MAX_LINES,
    )));
    let master: SharedMaster = Arc::new(Mutex::new(pair.master));
    // Daemon-level owner registry. Single instance shared across every
    // accepted connection; the first successful handshake becomes Owner,
    // later attaches default to Observer.
    let owner_registry: SharedOwnerRegistry = Arc::new(Mutex::new(OwnerRegistry::new()));
    // Session UUID that rolls on every daemon respawn. Surfaced to the
    // client in `DaemonMsg::ServerHello.session_id`.
    let session_id = Uuid::new_v4();
    // Broadcast channel carries the already-shaped DaemonMsg frames produced
    // by `RenderPlane::on_pty_chunk` / `on_child_exit` / `on_resize`. The
    // handler tasks forward those onto each client's socket verbatim.
    let (event_tx, _) = broadcast::channel::<DaemonMsg>(2048);
    let (stdin_tx, stdin_rx) = mpsc::unbounded_channel::<PtyWrite>();

    // ---- PTY reader → buffer + broadcast ----
    let reader = master.lock().unwrap().try_clone_reader()?;
    spawn_pty_reader(reader, render_plane.clone(), event_tx.clone());

    // ---- PTY writer ← client stdin ----
    let writer = master.lock().unwrap().take_writer()?;
    spawn_pty_writer(writer, stdin_rx);

    // ---- Child-exit watcher ----
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    spawn_child_waiter(child, render_plane.clone(), event_tx.clone(), shutdown_tx);

    // ---- ChildReady poll timer ----
    //
    // Polls `RenderPlane::detect_ready()` every 50ms. The render plane
    // returns `Some(Effect::Broadcast(DaemonMsg::ChildReady{..}))` exactly
    // once per session — after `render_rev` has been quiescent for
    // `CHILD_READY_QUIESCENT_MS` (100ms) following the first chunk that
    // moved the model state. The poll-based shape avoids the per-chunk
    // deadline-task race window: each new chunk simply resets the timer
    // and the poller picks the new deadline up on the next tick.
    //
    // Terminal mode only — chat mode never spawns this task (chat has
    // its own first-event ready notion).
    let ready_task = {
        let render_plane = render_plane.clone();
        let event_tx = event_tx.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(50));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            // The first tick is immediate; skip it so we don't fire
            // against a freshly-constructed plane.
            tick.tick().await;
            loop {
                tick.tick().await;
                let effect = match render_plane.lock() {
                    Ok(mut rp) => rp.detect_ready(),
                    Err(_) => None,
                };
                if let Some(Effect::Broadcast(msg)) = effect {
                    let _ = event_tx.send(msg);
                    // detect_ready is one-shot; once it returned Some
                    // we'll never see another. Cheap to keep ticking
                    // but stopping here avoids the per-tick lock.
                    break;
                }
            }
        })
    };

    // ---- Socket ----
    let listener = bind_socket(&cli.sock)?;
    tracing::info!(sock = %cli.sock.display(), "listening");

    // Tell the parent we're accepting — lets it avoid racing to connect.
    notify_ready(cli.ready_fd);

    // ---- Accept loop ----
    let accept_task = tokio::spawn(accept_loop(
        listener,
        event_tx.clone(),
        render_plane.clone(),
        master.clone(),
        stdin_tx.clone(),
        killer.clone(),
        owner_registry.clone(),
        session_id,
        cli.id.to_string(),
    ));

    // Block until the child exits.
    let _ = shutdown_rx.await;
    tracing::info!("child exited, shutting down");

    // Let in-flight clients flush the ChildExited frame before we close.
    tokio::time::sleep(Duration::from_millis(200)).await;
    accept_task.abort();
    ready_task.abort();

    let _ = std::fs::remove_file(&cli.sock);
    Ok(())
}

/// Control frames the daemon writes onto the Node runner's stdin.
///
/// Wire shape (one NDJSON line per frame, opaque to anyone but the
/// runner): `{"kind":"user_message","content":"..."}`,
/// `{"kind":"stop"}`,
/// `{"kind":"answer_question","question_id":"<uuid>","answers":{...}}`.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ChatControl {
    UserMessage {
        content: String,
    },
    Stop,
    AnswerQuestion {
        question_id: Uuid,
        answers: HashMap<String, String>,
    },
}

async fn run_chat(cli: Cli) -> anyhow::Result<()> {
    // ---- spawn the Node runner (piped stdio, no PTY) ----
    let runner_path = cli
        .runner_path
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("run_chat called without --runner-path"))?;

    // Build argv: node <runner-path> --session-id <id> --cwd <cwd>
    //   [--resume] [--mcp-config <path>] [--program <prog>]
    // The runner expects --cwd to be the user's project cwd. If the daemon
    // wasn't told one we fall back to its own cwd so the SDK still has a
    // sensible working directory.
    let runner_cwd = match &cli.cwd {
        Some(p) => p.clone(),
        None => std::env::current_dir()?,
    };

    let mut cmd = Command::new("node");
    cmd.arg(runner_path);
    cmd.arg("--session-id").arg(cli.id.to_string());
    cmd.arg("--cwd").arg(&runner_cwd);
    if cli.resume {
        cmd.arg("--resume");
    }
    if let Some(p) = &cli.mcp_config {
        cmd.arg("--mcp-config").arg(p);
    }
    if let Some(p) = &cli.program {
        cmd.arg("--program").arg(p);
    }

    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // The daemon survives neige-server restarts, so a dropped Child
        // handle must not kill the running child.
        .kill_on_drop(false);

    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to spawn `node {}`: {e}", runner_path.display()))?;
    let child_stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("missing child stdin"))?;
    let child_stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("missing child stdout"))?;
    let child_stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("missing child stderr"))?;

    let buffer: SharedEventBuffer = Arc::new(Mutex::new(EventBuffer::new(cli.buffer_bytes)));
    let (event_tx, _) = broadcast::channel::<ChatEvt>(2048);
    let (stdin_tx, stdin_rx) = mpsc::unbounded_channel::<ChatControl>();

    spawn_chat_stdout_reader(child_stdout, buffer.clone(), event_tx.clone());
    spawn_chat_stderr_reader(child_stderr);
    spawn_chat_stdin_writer(child_stdin, stdin_rx);

    // ---- child-exit watcher ----
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let event_tx_w = event_tx.clone();
    tokio::spawn(async move {
        let status = child.wait().await.ok();
        let code = status.and_then(|s| s.code());
        tracing::info!(?code, "chat runner wait returned");
        let _ = event_tx_w.send(ChatEvt::Exit(code));
        let _ = shutdown_tx.send(());
    });

    // ---- Socket ----
    let listener = bind_socket(&cli.sock)?;
    tracing::info!(sock = %cli.sock.display(), "listening (chat mode)");

    notify_ready(cli.ready_fd);

    let accept_task = tokio::spawn(accept_chat_loop(
        listener,
        event_tx.clone(),
        buffer.clone(),
        stdin_tx.clone(),
    ));

    let _ = shutdown_rx.await;
    tracing::info!("chat runner exited, shutting down");
    tokio::time::sleep(Duration::from_millis(200)).await;
    accept_task.abort();

    let _ = std::fs::remove_file(&cli.sock);
    Ok(())
}

fn bind_socket(path: &Path) -> anyhow::Result<UnixListener> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    if path.exists() {
        // A stale socket from a previous run — safe to remove because no one
        // else owns this id (caller guarantees uniqueness).
        std::fs::remove_file(path)?;
    }
    Ok(UnixListener::bind(path)?)
}

fn notify_ready(fd: Option<i32>) {
    if let Some(fd) = fd {
        // SAFETY: fd is owned by us (parent passed it via fork/exec), it's a
        // writable pipe, and we take exclusive ownership here by not using it
        // anywhere else in the process.
        let mut f = unsafe { std::fs::File::from_raw_fd(fd) };
        let _ = f.write_all(b"ready\n");
        drop(f);
    }
}

/// Drain PTY master stdout. Each chunk is pumped through
/// [`RenderPlane::on_pty_chunk`] which feeds the VT model (bumping
/// `render_rev` on visible state change), appends to the transcript ring,
/// and returns an [`Effect::Broadcast`] carrying a `RenderPatch`. We
/// forward the underlying [`DaemonMsg`] onto the broadcast channel for
/// every attached client to see.
fn spawn_pty_reader(
    mut reader: Box<dyn std::io::Read + Send>,
    render_plane: SharedRenderPlane,
    event_tx: broadcast::Sender<DaemonMsg>,
) {
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break, // EOF; child closed stdout — child-waiter will signal exit
                Ok(n) => {
                    let bytes = buf[..n].to_vec();
                    let effects = match render_plane.lock() {
                        Ok(mut rp) => rp.on_pty_chunk(bytes),
                        Err(_) => Vec::new(),
                    };
                    apply_broadcaster_effects(&event_tx, effects);
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    tracing::warn!(error = %e, "pty read error; stopping reader");
                    break;
                }
            }
        }
    });
}

/// Translate a list of effects produced by [`RenderPlane`] (or, in older
/// chat-mode paths, [`PtyBroadcaster`]) into broadcast channel sends.
/// The render plane only emits `Effect::Broadcast` today; the other arms
/// are unreachable from this caller — we match exhaustively so a future
/// `Effect` addition is a compile error here rather than a silent drop.
fn apply_broadcaster_effects(tx: &broadcast::Sender<DaemonMsg>, effects: Vec<Effect>) {
    for eff in effects {
        match eff {
            Effect::Broadcast(msg) => {
                let _ = tx.send(msg);
            }
            // RenderPlane only emits Broadcast today; the other variants
            // belong to the client-frame state machine.
            Effect::SendToClient(_)
            | Effect::ResizePty { .. }
            | Effect::WriteToPty { .. }
            | Effect::KillChild
            | Effect::SendProtocolError { .. }
            | Effect::CloseConnection
            | Effect::AssignOwner(_)
            | Effect::BroadcastOwnerChanged(_)
            | Effect::ProtocolViolation(_) => {
                tracing::warn!(
                    "RenderPlane emitted non-Broadcast effect; dropping (this is a bug)"
                );
            }
        }
    }
}

fn spawn_pty_writer(
    mut writer: Box<dyn std::io::Write + Send>,
    mut stdin_rx: mpsc::UnboundedReceiver<PtyWrite>,
) {
    std::thread::spawn(move || {
        while let Some(item) = stdin_rx.blocking_recv() {
            let PtyWrite {
                data,
                input_seq,
                ack,
            } = item;
            if let Err(e) = writer.write_all(&data) {
                // Do NOT emit InputAck on failure — the client will time
                // out on its outstanding seq, which is the correct
                // semantics (see `DaemonMsg::InputAck` doc).
                tracing::warn!(error = %e, "pty write error; stopping writer");
                break;
            }
            // `flush()` failure is intentionally non-fatal (mirrors the
            // pre-#115 behaviour). The bytes are already in the kernel
            // tty buffer; a failed userspace flush is a logging concern,
            // not a delivery failure. We still emit the ack.
            let _ = writer.flush();
            // Emit InputAck back to the originating connection only when
            // the client requested one (seq > 0 + ack channel attached).
            // The ack-emission point is HERE, after `write_all` returned
            // successfully — not at channel-send time in `handle_client`
            // — so the client's outstanding seq is only resolved once
            // the bytes have actually been handed to the PTY master.
            if input_seq > 0
                && let Some(ack) = ack
            {
                // mpsc send failure means the connection has already
                // gone away; not interesting.
                let _ = ack.send(DaemonMsg::InputAck { input_seq });
            }
        }
    });
}

fn spawn_child_waiter(
    mut child: Box<dyn portable_pty::Child + Send + Sync>,
    render_plane: SharedRenderPlane,
    event_tx: broadcast::Sender<DaemonMsg>,
    shutdown_tx: oneshot::Sender<()>,
) {
    tokio::task::spawn_blocking(move || {
        let status = child.wait().ok();
        let code = status.map(|s| s.exit_code() as i32);
        tracing::info!(?code, "child wait returned");
        let effects = match render_plane.lock() {
            Ok(mut rp) => rp.on_child_exit(code),
            Err(_) => Vec::new(),
        };
        apply_broadcaster_effects(&event_tx, effects);
        let _ = shutdown_tx.send(());
    });
}

/// Read NDJSON from the chat-mode runner's stdout. Each non-empty line is
/// already a serialized `NeigeEvent` JSON string — the daemon does NOT
/// parse it. We push the line into the replay buffer and broadcast it
/// verbatim to every attached client.
fn spawn_chat_stdout_reader(
    stdout: tokio::process::ChildStdout,
    buffer: SharedEventBuffer,
    event_tx: broadcast::Sender<ChatEvt>,
) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => {
                    if line.trim().is_empty() {
                        continue;
                    }
                    if let Ok(mut b) = buffer.lock() {
                        b.append(line.clone());
                    }
                    let _ = event_tx.send(ChatEvt::Event(line));
                }
                Ok(None) => break, // EOF
                Err(e) => {
                    tracing::warn!(error = %e, "chat stdout read error; stopping reader");
                    break;
                }
            }
        }
    });
}

/// Forward chat-mode child stderr to tracing::warn. Don't surface to clients.
fn spawn_chat_stderr_reader(stderr: tokio::process::ChildStderr) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            tracing::warn!(target: "chat_child_stderr", "{line}");
        }
    });
}

/// Serialize each [`ChatControl`] frame into one NDJSON line and write to
/// the runner's stdin. The runner reads `{"kind":"...", ...}` lines and
/// drives the SDK accordingly. Closes the child's stdin when the channel
/// closes (the runner exits on EOF).
fn spawn_chat_stdin_writer(mut stdin: ChildStdin, mut rx: mpsc::UnboundedReceiver<ChatControl>) {
    tokio::spawn(async move {
        while let Some(ctrl) = rx.recv().await {
            let line = match serde_json::to_string(&ctrl) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(error = %e, "encode ChatControl failed");
                    continue;
                }
            };
            if let Err(e) = stdin.write_all(line.as_bytes()).await {
                tracing::warn!(error = %e, "chat stdin write error; stopping writer");
                break;
            }
            if let Err(e) = stdin.write_all(b"\n").await {
                tracing::warn!(error = %e, "chat stdin write error; stopping writer");
                break;
            }
            if let Err(e) = stdin.flush().await {
                tracing::warn!(error = %e, "chat stdin flush error");
                break;
            }
        }
        // Channel closed (e.g. ClientMsg::Kill dropped the sender). Drop
        // stdin so the runner sees EOF and exits cleanly.
    });
}

#[allow(clippy::too_many_arguments)]
async fn accept_loop(
    listener: UnixListener,
    event_tx: broadcast::Sender<DaemonMsg>,
    render_plane: SharedRenderPlane,
    master: SharedMaster,
    stdin_tx: mpsc::UnboundedSender<PtyWrite>,
    killer: Arc<Mutex<Box<dyn portable_pty::ChildKiller + Send + Sync>>>,
    owner_registry: SharedOwnerRegistry,
    session_id: Uuid,
    terminal_id: String,
) {
    loop {
        match listener.accept().await {
            Ok((sock, _)) => {
                let event_rx = event_tx.subscribe();
                let event_tx_inner = event_tx.clone();
                let render_plane = render_plane.clone();
                let master = master.clone();
                let stdin_tx = stdin_tx.clone();
                let killer = killer.clone();
                let owner_registry = owner_registry.clone();
                let terminal_id = terminal_id.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_client(
                        sock,
                        event_rx,
                        event_tx_inner,
                        render_plane,
                        master,
                        stdin_tx,
                        killer,
                        owner_registry,
                        session_id,
                        terminal_id,
                    )
                    .await
                    {
                        tracing::debug!(error = %e, "client ended");
                    }
                });
            }
            Err(e) => {
                tracing::warn!(error = %e, "accept failed");
                break;
            }
        }
    }
}

async fn accept_chat_loop(
    listener: UnixListener,
    event_tx: broadcast::Sender<ChatEvt>,
    buffer: SharedEventBuffer,
    stdin_tx: mpsc::UnboundedSender<ChatControl>,
) {
    loop {
        match listener.accept().await {
            Ok((sock, _)) => {
                let event_rx = event_tx.subscribe();
                let buffer = buffer.clone();
                let stdin_tx = stdin_tx.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_chat_client(sock, event_rx, buffer, stdin_tx).await {
                        tracing::debug!(error = %e, "chat client ended");
                    }
                });
            }
            Err(e) => {
                tracing::warn!(error = %e, "accept failed");
                break;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_client(
    sock: UnixStream,
    event_rx: broadcast::Receiver<DaemonMsg>,
    event_tx: broadcast::Sender<DaemonMsg>,
    render_plane: SharedRenderPlane,
    master: SharedMaster,
    stdin_tx: mpsc::UnboundedSender<PtyWrite>,
    killer: Arc<Mutex<Box<dyn portable_pty::ChildKiller + Send + Sync>>>,
    owner_registry: SharedOwnerRegistry,
    session_id: Uuid,
    terminal_id: String,
) -> anyhow::Result<()> {
    let (mut rd, mut wr) = sock.into_split();
    let mut state = TerminalSessionState::new();

    // Per-connection direct-message channel. Used for:
    //   - `Effect::SendProtocolError` post-handshake (so NotOwner errors
    //     only reach the offending client — fixes PR-1 review nit #1).
    //   - `Effect::SendToClient` post-handshake (currently unused, but
    //     routed here for symmetry).
    //   - Backpressure: `SnapshotRequired` + fresh `RenderSnapshot` after
    //     a broadcast lag.
    //
    // Channel is unbounded — these messages are rare and small.
    let (per_client_tx, mut per_client_rx) = mpsc::unbounded_channel::<DaemonMsg>();

    // First frame must be ClientHello (v2). Synchronously translate it into
    // effects, then write `ServerHello` BEFORE spawning the broadcast
    // fan-out task. That guarantees no `RenderPatch` frame can land on the
    // socket ahead of `ServerHello`.
    let first: ClientMsg = read_frame(&mut rd).await?;
    // Capture geometry / scrollback request from the hello before the
    // state machine consumes it — needed to rebuild a geometry-bound
    // snapshot below.
    let (desired_size, scrollback_request) = match &first {
        ClientMsg::ClientHello {
            desired_size,
            initial_scrollback,
            ..
        } => (
            Some(*desired_size),
            Some(scrollback_request(*initial_scrollback)),
        ),
        _ => (None, None),
    };

    let (first_effects, current_owner, current_pty_size) = {
        let guard = render_plane.lock().unwrap();
        let mut reg = owner_registry.lock().unwrap();
        let current_pty_size = guard.current_size();
        let ctx = SessionContext {
            terminal_id: &terminal_id,
            session_id,
            pty_size: current_pty_size,
            pty_seq_head: guard.pty_seq_head(),
            pty_seq_tail: guard.pty_seq(),
            render_rev: guard.render_rev(),
            // Deterministic snapshot of the one-shot ChildReady state.
            // `child_ready_fired()` is a non-consuming read on
            // `RenderPlane` — it does NOT advance the one-shot, so late
            // joiners after `detect_ready()` already fired still see a
            // truthful `true` here without re-emitting the broadcast.
            is_child_ready: guard.child_ready_fired(),
        };
        let eff = state.on_client_frame(first, guard.transcript(), &mut reg, &ctx);
        (eff, reg.current_owner(), current_pty_size)
    };
    let mut handshake_failed = false;
    for eff in first_effects {
        match eff {
            Effect::ResizePty { cols, rows } => {
                apply_resize(&master, cols, rows);
                if let Ok(mut rp) = render_plane.lock() {
                    // Also resize the model so its grid matches the new PTY
                    // geometry. We swallow the broadcast effect this
                    // produces — it would fire a `RenderSnapshot` on every
                    // attach, which is redundant with the ServerHello
                    // snapshot we're about to send.
                    let _ = rp.on_resize(cols, rows);
                }
            }
            Effect::SendToClient(msg) => {
                // Rebuild ServerHello's snapshot bound to the client's
                // desired geometry — this is the core geometry-binding
                // promise of PR-2.
                let msg = rebuild_server_hello_snapshot(
                    msg,
                    &render_plane,
                    desired_size,
                    scrollback_request,
                );
                write_frame(&mut wr, &msg).await?;
            }
            Effect::SendProtocolError {
                code,
                message,
                expected_version,
            } => {
                // Best-effort write of the typed error frame, then close.
                // We haven't spawned `down_task` yet so `wr` is local —
                // direct write is fine.
                let _ = write_frame(
                    &mut wr,
                    &DaemonMsg::ProtocolError {
                        code,
                        message,
                        expected_version,
                    },
                )
                .await;
                handshake_failed = true;
            }
            Effect::CloseConnection => {
                handshake_failed = true;
            }
            Effect::ProtocolViolation(reason) => {
                anyhow::bail!("{reason}");
            }
            // Other effects aren't reachable from the handshake transition.
            Effect::Broadcast(_)
            | Effect::WriteToPty { .. }
            | Effect::KillChild
            | Effect::AssignOwner(_)
            | Effect::BroadcastOwnerChanged(_) => {
                tracing::warn!("unexpected effect on first frame; ignoring");
            }
        }
    }
    if handshake_failed {
        return Ok(());
    }
    let _ = current_owner; // currently unused post-handshake but kept for future hooks
    let _ = current_pty_size;

    // Fan out events to this client. `down_task` selects on the broadcast
    // receiver (RenderPatch / RenderSnapshot / OwnerChanged / ...) AND
    // the per-client mpsc (ProtocolError targeted at this client + on-
    // demand RenderSnapshot after a Lagged event).
    let down_render_plane = render_plane.clone();
    let mut down_event_rx = event_rx;
    let down_task = tokio::spawn(async move {
        // Track last-known geometry so backpressure snapshots are bound to
        // the right size. Starts at the render-plane's initial geometry
        // (the daemon's CLI cols/rows) and gets re-read from the render
        // plane each Lagged event, so we always rebuild at the freshest
        // PTY size.
        loop {
            tokio::select! {
                broadcast = down_event_rx.recv() => match broadcast {
                    Ok(msg) => {
                        let is_exit = matches!(msg, DaemonMsg::TerminalExited { .. });
                        if write_frame(&mut wr, &msg).await.is_err() {
                            break;
                        }
                        if is_exit {
                            break;
                        }
                    }
                    // Lagged: this client missed N frames. Issue a typed
                    // SnapshotRequired (so the client knows to wipe its
                    // local state) then immediately push a fresh
                    // RenderSnapshot bound to the current PTY geometry.
                    // This is the only backpressure policy we implement in
                    // PR-2; `BackpressurePolicy::LatestOnly` / `Close` are
                    // wire-only.
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(lagged = n, "client lagged; sending SnapshotRequired + fresh snapshot");
                        let snap = {
                            let rp = down_render_plane.lock().unwrap();
                            let sz = rp.current_size();
                            rp.build_snapshot(sz.cols, sz.rows, ScrollbackLimit::None)
                        };
                        let required = DaemonMsg::SnapshotRequired {
                            reason: format!("broadcast lagged by {n} frames"),
                        };
                        if write_frame(&mut wr, &required).await.is_err() {
                            break;
                        }
                        if write_frame(&mut wr, &DaemonMsg::RenderSnapshot(snap))
                            .await
                            .is_err()
                        {
                            break;
                        }
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                },
                direct = per_client_rx.recv() => match direct {
                    Some(msg) => {
                        let is_exit = matches!(msg, DaemonMsg::TerminalExited { .. });
                        if write_frame(&mut wr, &msg).await.is_err() {
                            break;
                        }
                        if is_exit {
                            break;
                        }
                    }
                    None => {
                        // Per-client sender dropped → up-loop has exited → down_task
                        // is no longer useful. Broadcast forwarding terminates with us.
                        break;
                    }
                }
            }
        }
    });

    // Read client → state machine → effects.
    let client_id_for_disconnect = state.client_id();
    loop {
        let msg: ClientMsg = match read_frame(&mut rd).await {
            Ok(m) => m,
            Err(_) => break,
        };
        let effects = {
            let guard = render_plane.lock().unwrap();
            let mut reg = owner_registry.lock().unwrap();
            let ctx = SessionContext {
                terminal_id: &terminal_id,
                session_id,
                pty_size: guard.current_size(),
                pty_seq_head: guard.pty_seq_head(),
                pty_seq_tail: guard.pty_seq(),
                render_rev: guard.render_rev(),
                // Post-handshake frames never produce ServerHello, so
                // this value is currently inert. Still snapshot it for
                // symmetry — a future state-machine path that re-emits
                // ServerHello on resync will need an accurate read.
                is_child_ready: guard.child_ready_fired(),
            };
            state.on_client_frame(msg, guard.transcript(), &mut reg, &ctx)
        };

        let mut closed = false;
        for eff in effects {
            match eff {
                Effect::SendToClient(msg) => {
                    // Post-handshake the state machine never emits
                    // SendToClient today, but route it through the
                    // per-client mpsc for symmetry.
                    let _ = per_client_tx.send(msg);
                }
                Effect::Broadcast(msg) => {
                    let _ = event_tx.send(msg);
                }
                Effect::ResizePty { cols, rows } => {
                    apply_resize(&master, cols, rows);
                    if let Ok(mut rp) = render_plane.lock() {
                        let resize_effects = rp.on_resize(cols, rows);
                        // RenderPlane::on_resize emits a Broadcast(RenderSnapshot)
                        // so every attached client repaints at the new size.
                        for re in resize_effects {
                            if let Effect::Broadcast(m) = re {
                                let _ = event_tx.send(m);
                            }
                        }
                    }
                }
                Effect::WriteToPty { data, input_seq } => {
                    // Per-connection ack routing: only attach the
                    // per-client mpsc when the client actually asked for
                    // an ack (input_seq > 0). seq == 0 is the wire
                    // default for browser-typing — no ack channel, no
                    // ack frame, no extra wire traffic on the hot path.
                    let ack = if input_seq > 0 {
                        Some(per_client_tx.clone())
                    } else {
                        None
                    };
                    if stdin_tx
                        .send(PtyWrite {
                            data,
                            input_seq,
                            ack,
                        })
                        .is_err()
                    {
                        closed = true;
                        break;
                    }
                }
                Effect::KillChild => {
                    tracing::info!("client requested Kill; signaling child");
                    kill_child(&master, &killer);
                }
                Effect::SendProtocolError {
                    code,
                    message,
                    expected_version,
                } => {
                    // Per-client direct send — fixes the PR-1 nit where
                    // post-handshake errors were broadcast to every client.
                    // Observer-typed NotOwner now goes only to the
                    // offending observer; the owner sees nothing.
                    let _ = per_client_tx.send(DaemonMsg::ProtocolError {
                        code,
                        message,
                        expected_version,
                    });
                }
                Effect::CloseConnection => {
                    closed = true;
                    break;
                }
                Effect::AssignOwner(_) => {
                    // Bookkeeping-only intent; the registry state was
                    // already updated inside on_client_frame. No socket
                    // effect.
                }
                Effect::BroadcastOwnerChanged(owner) => {
                    let _ = event_tx.send(DaemonMsg::OwnerChanged {
                        owner_client_id: owner,
                    });
                }
                Effect::ProtocolViolation(reason) => {
                    // Legacy path — new code uses SendProtocolError +
                    // CloseConnection.
                    down_task.abort();
                    let _ = down_task.await;
                    anyhow::bail!("{reason}");
                }
            }
        }
        if closed {
            break;
        }
    }

    // Client's read half closed; if this client held ownership, drop it
    // so a future attach can take over.
    if let Some(cid) = client_id_for_disconnect {
        let changed = {
            let mut reg = owner_registry.lock().unwrap();
            reg.on_release(cid)
        };
        if changed {
            let _ = event_tx.send(DaemonMsg::OwnerChanged {
                owner_client_id: None,
            });
        }
    }

    // Dropping `per_client_tx` (going out of scope below) signals the
    // direct-message half of `down_task` to wind down on its own.
    drop(per_client_tx);
    down_task.abort();
    let _ = down_task.await;
    Ok(())
}

fn scrollback_request(req: calm_session::InitialScrollback) -> ScrollbackLimit {
    match req {
        calm_session::InitialScrollback::None => ScrollbackLimit::None,
        calm_session::InitialScrollback::All => ScrollbackLimit::All,
        calm_session::InitialScrollback::Lines(n) => ScrollbackLimit::Lines(n),
    }
}

/// Replace `ServerHello.snapshot` with a fresh snapshot built by the
/// render plane at the client's desired geometry. The state machine
/// produces a raw-byte-transcript snapshot by default (for unit-test
/// parity); PR-2 swaps it for a server-rendered ANSI stream bound to
/// `desired_size`.
///
/// `desired_size = None` means the incoming frame isn't a ServerHello —
/// just pass through.
fn rebuild_server_hello_snapshot(
    msg: DaemonMsg,
    render_plane: &SharedRenderPlane,
    desired_size: Option<PtySize>,
    scrollback: Option<ScrollbackLimit>,
) -> DaemonMsg {
    match msg {
        DaemonMsg::ServerHello {
            protocol_version,
            terminal_id,
            session_id,
            client_role,
            owner_client_id,
            pty_size,
            pty_seq_head,
            pty_seq_tail,
            render_rev,
            snapshot: _,
            history_gap,
            is_child_ready,
        } => {
            let (cols, rows) = desired_size
                .map(|s| (s.cols, s.rows))
                .unwrap_or((pty_size.cols, pty_size.rows));
            let limit = scrollback.unwrap_or(ScrollbackLimit::None);
            let snapshot = match render_plane.lock() {
                Ok(rp) => rp.build_snapshot(cols, rows, limit),
                Err(_) => {
                    tracing::warn!("render_plane lock poisoned; sending empty snapshot");
                    calm_session::RenderSnapshot {
                        render_rev,
                        pty_seq: pty_seq_tail,
                        cols,
                        rows,
                        encoding: calm_session::RenderEncoding::Vt,
                        data: Vec::new(),
                        scrollback: None,
                    }
                }
            };
            DaemonMsg::ServerHello {
                protocol_version,
                terminal_id,
                session_id,
                client_role,
                owner_client_id,
                pty_size,
                pty_seq_head,
                pty_seq_tail,
                render_rev,
                snapshot,
                history_gap,
                is_child_ready,
            }
        }
        other => other,
    }
}

async fn handle_chat_client(
    sock: UnixStream,
    mut event_rx: broadcast::Receiver<ChatEvt>,
    buffer: SharedEventBuffer,
    stdin_tx: mpsc::UnboundedSender<ChatControl>,
) -> anyhow::Result<()> {
    let (mut rd, mut wr) = sock.into_split();

    // First frame must be ClientHello (v2). cols/rows in the desired_size
    // field are placeholder in chat mode — chat has no PTY to resize.
    let first: ClientMsg = read_frame(&mut rd).await?;
    match first {
        ClientMsg::ClientHello { .. } => {}
        other => anyhow::bail!("expected ClientHello as first message, got {other:?}"),
    }

    let replay = {
        let b = buffer.lock().unwrap();
        b.snapshot()
    };
    write_frame(&mut wr, &DaemonMsg::HelloChat { replay }).await?;

    let down_task = tokio::spawn(async move {
        loop {
            match event_rx.recv().await {
                Ok(ChatEvt::Event(json)) => {
                    if write_frame(&mut wr, &DaemonMsg::ChatEvent { json })
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Ok(ChatEvt::Exit(code)) => {
                    let _ = write_frame(&mut wr, &DaemonMsg::ChildExited { code }).await;
                    break;
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(lagged = n, "chat client lagged; dropping events");
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    loop {
        let msg: ClientMsg = match read_frame(&mut rd).await {
            Ok(m) => m,
            Err(_) => break,
        };
        match msg {
            ClientMsg::ChatUserMessage { content } => {
                if stdin_tx.send(ChatControl::UserMessage { content }).is_err() {
                    break;
                }
            }
            ClientMsg::ChatStop => {
                if stdin_tx.send(ChatControl::Stop).is_err() {
                    break;
                }
            }
            ClientMsg::AnswerQuestion {
                question_id,
                answers,
            } => {
                if stdin_tx
                    .send(ChatControl::AnswerQuestion {
                        question_id,
                        answers,
                    })
                    .is_err()
                {
                    break;
                }
            }
            ClientMsg::ClientHello { .. } => {
                // Ignore re-handshake on a live connection.
            }
            ClientMsg::Kill => {
                tracing::info!("client requested Kill in chat mode; closing runner stdin");
                // Drop the stdin sender → writer task drops the ChildStdin →
                // runner sees EOF and exits its query() loop. Child-waiter
                // then broadcasts Exit and we shut down.
                drop(stdin_tx);
                break;
            }
            // Terminal-mode-only frames — quietly ignored in chat mode.
            ClientMsg::Input { .. }
            | ClientMsg::ResizeCommit { .. }
            | ClientMsg::OwnerClaim
            | ClientMsg::OwnerRelease
            | ClientMsg::RenderAck { .. } => {
                tracing::debug!("ignoring terminal-mode frame in chat mode");
            }
        }
    }

    down_task.abort();
    let _ = down_task.await;
    Ok(())
}

/// Try hard to tear down the child. We first SIGHUP the whole process group
/// (portable-pty marks the child as its own session/pgid via setsid, so the
/// pgid equals the child pid), then schedule a SIGKILL fallback in case the
/// child ignored SIGHUP. Signaling the group catches transient subshells
/// (e.g. `sh -c 'bash'` spawning a separate bash process) that a single-pid
/// kill would miss.
fn kill_child(
    master: &SharedMaster,
    killer: &Arc<Mutex<Box<dyn portable_pty::ChildKiller + Send + Sync>>>,
) {
    let pgid = master.lock().ok().and_then(|m| m.process_group_leader());
    if let Some(pgid) = pgid {
        // SAFETY: killpg-style negative pid targets the process group with
        // the matching id. We created this pgid via setsid at spawn time.
        unsafe {
            libc::kill(-pgid, libc::SIGHUP);
        }
        // Hard fallback in case the child traps SIGHUP and keeps running.
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(2)).await;
            unsafe {
                libc::kill(-pgid, libc::SIGKILL);
            }
        });
    } else if let Ok(mut k) = killer.lock() {
        // Last-resort fallback through portable-pty's killer.
        let _ = k.kill();
    }
}

fn apply_resize(master: &SharedMaster, cols: u16, rows: u16) {
    if cols == 0 || rows == 0 {
        return;
    }
    let m = master.lock().unwrap();
    if let Err(e) = m.resize(PtPtySize {
        cols,
        rows,
        pixel_width: 0,
        pixel_height: 0,
    }) {
        tracing::warn!(error = %e, "pty resize failed");
    }
}

#[allow(dead_code)]
fn _ensure_is_path(_p: &Path) {} // placate some lints on older toolchains

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_control_user_message_serialization() {
        let frame = ChatControl::UserMessage {
            content: "hello".to_string(),
        };
        let json = serde_json::to_string(&frame).unwrap();
        assert_eq!(json, r#"{"kind":"user_message","content":"hello"}"#);
    }

    #[test]
    fn chat_control_stop_serialization() {
        let frame = ChatControl::Stop;
        let json = serde_json::to_string(&frame).unwrap();
        assert_eq!(json, r#"{"kind":"stop"}"#);
    }

    #[test]
    fn chat_control_answer_question_serialization() {
        let qid = Uuid::parse_str("6b1f3a4d-2b5e-4d7e-9c1a-1b2c3d4e5f60").unwrap();
        let frame = ChatControl::AnswerQuestion {
            question_id: qid,
            answers: HashMap::from([("Proceed?".to_string(), "ok".to_string())]),
        };
        let json = serde_json::to_string(&frame).unwrap();
        assert_eq!(
            json,
            r#"{"kind":"answer_question","question_id":"6b1f3a4d-2b5e-4d7e-9c1a-1b2c3d4e5f60","answers":{"Proceed?":"ok"}}"#
        );
    }
}
