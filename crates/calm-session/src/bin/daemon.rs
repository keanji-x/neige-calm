//! calm-session-daemon — per-session supervisor.
//!
//! Two modes share the same binary, socket, and framing:
//!
//! - **Terminal mode** (default): spawn the user's program under a PTY,
//!   broadcast raw PTY output to every attached client, keep a small ring
//!   buffer of recent bytes for replay. The daemon does NO terminal-state
//!   parsing — cursor / scrollback / cell-grid interpretation lives on the
//!   client side (xterm.js). This trades a slightly larger reattach payload
//!   (~1 MiB instead of a single-screen snapshot) for never having a
//!   server-side vt100 parser to maintain or hit edge cases on.
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
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::process::{ChildStdin, Command};
use tokio::sync::{broadcast, mpsc, oneshot};
use uuid::Uuid;

use calm_session::{ClientMsg, DaemonMsg, read_frame, write_frame};

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

/// Events fanned out to every attached client in terminal mode.
#[derive(Clone, Debug)]
enum Event {
    Output(Vec<u8>),
    Exit(Option<i32>),
}

/// Events fanned out in chat mode. Each `Event` here is one already-
/// serialized `NeigeEvent` JSON line.
#[derive(Clone, Debug)]
enum ChatEvt {
    Event(String),
    Exit(Option<i32>),
}

/// Rolling byte ring used to seed the Hello replay for a new client in
/// terminal mode.
///
/// Stored as a deque of chunks (each = one PTY read) so eviction is
/// chunk-granular — we never split an escape sequence. When `total_bytes`
/// exceeds `max_bytes` we drop chunks from the front, which can lose
/// older context but keeps memory bounded.
struct ByteBuffer {
    chunks: VecDeque<Vec<u8>>,
    total_bytes: usize,
    max_bytes: usize,
}

impl ByteBuffer {
    fn new(max_bytes: usize) -> Self {
        Self {
            chunks: VecDeque::new(),
            total_bytes: 0,
            max_bytes,
        }
    }

    fn append(&mut self, bytes: Vec<u8>) {
        self.total_bytes += bytes.len();
        self.chunks.push_back(bytes);
        while self.total_bytes > self.max_bytes && self.chunks.len() > 1 {
            let dropped = self.chunks.pop_front().unwrap();
            self.total_bytes -= dropped.len();
        }
    }

    /// Concatenated copy of the current buffer — fed straight into the
    /// `DaemonMsg::Hello { replay }` field. Cheap clone-out is fine here:
    /// only happens on attach, not in the hot path.
    fn snapshot(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.total_bytes);
        for c in &self.chunks {
            out.extend_from_slice(c);
        }
        out
    }
}

/// Same chunk-granular eviction strategy as `ByteBuffer`, but each chunk is
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

type SharedBuffer = Arc<Mutex<ByteBuffer>>;
type SharedEventBuffer = Arc<Mutex<EventBuffer>>;
type SharedMaster = Arc<Mutex<Box<dyn MasterPty + Send>>>;

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
    let pair = pty_system.openpty(PtySize {
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

    let buffer: SharedBuffer = Arc::new(Mutex::new(ByteBuffer::new(cli.buffer_bytes)));
    let master: SharedMaster = Arc::new(Mutex::new(pair.master));
    let (event_tx, _) = broadcast::channel::<Event>(2048);
    let (stdin_tx, stdin_rx) = mpsc::unbounded_channel::<Vec<u8>>();

    // ---- PTY reader → buffer + broadcast ----
    let reader = master.lock().unwrap().try_clone_reader()?;
    spawn_pty_reader(reader, buffer.clone(), event_tx.clone());

    // ---- PTY writer ← client stdin ----
    let writer = master.lock().unwrap().take_writer()?;
    spawn_pty_writer(writer, stdin_rx);

    // ---- Child-exit watcher ----
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    spawn_child_waiter(child, event_tx.clone(), shutdown_tx);

    // ---- Socket ----
    let listener = bind_socket(&cli.sock)?;
    tracing::info!(sock = %cli.sock.display(), "listening");

    // Tell the parent we're accepting — lets it avoid racing to connect.
    notify_ready(cli.ready_fd);

    // ---- Accept loop ----
    let accept_task = tokio::spawn(accept_loop(
        listener,
        event_tx.clone(),
        buffer.clone(),
        master.clone(),
        stdin_tx.clone(),
        killer.clone(),
    ));

    // Block until the child exits.
    let _ = shutdown_rx.await;
    tracing::info!("child exited, shutting down");

    // Let in-flight clients flush the ChildExited frame before we close.
    tokio::time::sleep(Duration::from_millis(200)).await;
    accept_task.abort();

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

fn spawn_pty_reader(
    mut reader: Box<dyn std::io::Read + Send>,
    buffer: SharedBuffer,
    event_tx: broadcast::Sender<Event>,
) {
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break, // EOF; child closed stdout — child-waiter will signal exit
                Ok(n) => {
                    let bytes = buf[..n].to_vec();
                    if let Ok(mut b) = buffer.lock() {
                        b.append(bytes.clone());
                    }
                    let _ = event_tx.send(Event::Output(bytes));
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

fn spawn_pty_writer(
    mut writer: Box<dyn std::io::Write + Send>,
    mut stdin_rx: mpsc::UnboundedReceiver<Vec<u8>>,
) {
    std::thread::spawn(move || {
        while let Some(bytes) = stdin_rx.blocking_recv() {
            if let Err(e) = writer.write_all(&bytes) {
                tracing::warn!(error = %e, "pty write error; stopping writer");
                break;
            }
            let _ = writer.flush();
        }
    });
}

fn spawn_child_waiter(
    mut child: Box<dyn portable_pty::Child + Send + Sync>,
    event_tx: broadcast::Sender<Event>,
    shutdown_tx: oneshot::Sender<()>,
) {
    tokio::task::spawn_blocking(move || {
        let status = child.wait().ok();
        let code = status.map(|s| s.exit_code() as i32);
        tracing::info!(?code, "child wait returned");
        let _ = event_tx.send(Event::Exit(code));
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

async fn accept_loop(
    listener: UnixListener,
    event_tx: broadcast::Sender<Event>,
    buffer: SharedBuffer,
    master: SharedMaster,
    stdin_tx: mpsc::UnboundedSender<Vec<u8>>,
    killer: Arc<Mutex<Box<dyn portable_pty::ChildKiller + Send + Sync>>>,
) {
    loop {
        match listener.accept().await {
            Ok((sock, _)) => {
                let event_rx = event_tx.subscribe();
                let buffer = buffer.clone();
                let master = master.clone();
                let stdin_tx = stdin_tx.clone();
                let killer = killer.clone();
                tokio::spawn(async move {
                    if let Err(e) =
                        handle_client(sock, event_rx, buffer, master, stdin_tx, killer).await
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

async fn handle_client(
    sock: UnixStream,
    mut event_rx: broadcast::Receiver<Event>,
    buffer: SharedBuffer,
    master: SharedMaster,
    stdin_tx: mpsc::UnboundedSender<Vec<u8>>,
    killer: Arc<Mutex<Box<dyn portable_pty::ChildKiller + Send + Sync>>>,
) -> anyhow::Result<()> {
    let (mut rd, mut wr) = sock.into_split();

    // First frame must be Attach.
    let first: ClientMsg = read_frame(&mut rd).await?;
    let (cols, rows) = match first {
        ClientMsg::Attach { cols, rows } => (cols, rows),
        other => anyhow::bail!("expected Attach as first message, got {other:?}"),
    };

    // Resize PTY to this client's viewport (latest-attach-wins). No parser
    // state to keep in sync — the snapshot is just the recent byte stream.
    apply_resize(&master, cols, rows);

    // Snapshot the recent PTY bytes; the client will feed them into xterm.js
    // which interprets them and reproduces the screen state.
    let replay = {
        let b = buffer.lock().unwrap();
        b.snapshot()
    };
    write_frame(&mut wr, &DaemonMsg::Hello { replay }).await?;

    // Fan out events to this client.
    let down_task = tokio::spawn(async move {
        loop {
            match event_rx.recv().await {
                Ok(Event::Output(b)) => {
                    if write_frame(&mut wr, &DaemonMsg::Stdout(b)).await.is_err() {
                        break;
                    }
                }
                Ok(Event::Exit(code)) => {
                    let _ = write_frame(&mut wr, &DaemonMsg::ChildExited { code }).await;
                    break;
                }
                // Slow client — skip dropped frames rather than tear down.
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(lagged = n, "client lagged; dropping frames");
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // Read client → PTY.
    loop {
        let msg: ClientMsg = match read_frame(&mut rd).await {
            Ok(m) => m,
            Err(_) => break,
        };
        match msg {
            ClientMsg::Stdin(b) => {
                if stdin_tx.send(b).is_err() {
                    break;
                }
            }
            ClientMsg::Resize { cols, rows } => {
                apply_resize(&master, cols, rows);
            }
            ClientMsg::Attach { .. } => {
                // Ignore re-attach on a live connection.
            }
            ClientMsg::Kill => {
                tracing::info!("client requested Kill; signaling child");
                kill_child(&master, &killer);
            }
            ClientMsg::ChatUserMessage { .. }
            | ClientMsg::ChatStop
            | ClientMsg::AnswerQuestion { .. } => {
                // Wrong mode — ignore quietly. Server should never send chat-
                // mode frames to a terminal-mode daemon.
                tracing::debug!("ignoring chat-mode frame in terminal mode");
            }
        }
    }

    // Client's read half closed; drop the sender side so down_task terminates.
    down_task.abort();
    let _ = down_task.await;
    Ok(())
}

async fn handle_chat_client(
    sock: UnixStream,
    mut event_rx: broadcast::Receiver<ChatEvt>,
    buffer: SharedEventBuffer,
    stdin_tx: mpsc::UnboundedSender<ChatControl>,
) -> anyhow::Result<()> {
    let (mut rd, mut wr) = sock.into_split();

    // First frame must be Attach. cols/rows are placeholder in chat mode.
    let first: ClientMsg = read_frame(&mut rd).await?;
    match first {
        ClientMsg::Attach { .. } => {}
        other => anyhow::bail!("expected Attach as first message, got {other:?}"),
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
            ClientMsg::Attach { .. } => {
                // Ignore re-attach on a live connection.
            }
            ClientMsg::Kill => {
                tracing::info!("client requested Kill in chat mode; closing runner stdin");
                // Drop the stdin sender → writer task drops the ChildStdin →
                // runner sees EOF and exits its query() loop. Child-waiter
                // then broadcasts Exit and we shut down.
                drop(stdin_tx);
                break;
            }
            // Wrong-mode frames — quietly ignored.
            ClientMsg::Stdin(_) | ClientMsg::Resize { .. } => {
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
    if let Err(e) = m.resize(PtySize {
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
