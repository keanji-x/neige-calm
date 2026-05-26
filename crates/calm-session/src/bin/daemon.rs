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
use std::io::{self, Write as _};
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
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::time::timeout;
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

    /// Default foreground RGB the daemon advertises to the PTY child in
    /// reply to OSC 10 color queries (#177). Format: `r,g,b` (decimal
    /// 0..=255). REQUIRED in terminal mode — PR1 (#262) made
    /// `terminals.theme_fg` NOT NULL (migration 0017), so every kernel-
    /// side spawn site carries this. A missing flag in terminal mode
    /// fails fast at clap parse so a regression that forgets to thread
    /// theme through `spawn_daemon_with_parts` surfaces at daemon
    /// startup rather than degrading silently to a built-in default.
    /// Chat mode never instantiates a `TerminalModel` and ignores this.
    #[arg(long, value_parser = parse_rgb, required_if_eq("mode", "terminal"))]
    terminal_fg: Option<(u8, u8, u8)>,

    /// Default background RGB the daemon advertises to the PTY child in
    /// reply to OSC 11 color queries (#177). Same shape and required-
    /// when-terminal posture as `--terminal-fg`. Codex's startup probe
    /// (#177) reads this to pick a contrasting text color against the
    /// host browser's theme; missing it would force codex to fall back
    /// to its built-in default and visually clash with the host theme.
    #[arg(long, value_parser = parse_rgb, required_if_eq("mode", "terminal"))]
    terminal_bg: Option<(u8, u8, u8)>,

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

/// Parse `--terminal-fg`/`--terminal-bg` payloads. Format: three
/// comma-separated decimal `u8` channels (e.g. `216,219,226`). Used by
/// clap's value_parser; fails fast with a human-readable error so a
/// typo on the spawn arg list surfaces before the daemon comes up.
fn parse_rgb(s: &str) -> Result<(u8, u8, u8), String> {
    let parts: Vec<&str> = s.split(',').collect();
    if parts.len() != 3 {
        return Err(format!(
            "expected `r,g,b` (three comma-separated u8 channels), got {s:?}"
        ));
    }
    let parse = |i: usize| -> Result<u8, String> {
        parts[i]
            .trim()
            .parse::<u8>()
            .map_err(|e| format!("channel {i} ({:?}): {e}", parts[i]))
    };
    Ok((parse(0)?, parse(1)?, parse(2)?))
}

/// Events fanned out in chat mode. Each `Event` here is one already-
/// serialized `NeigeEvent` JSON line tagged with a monotonic `seq` so
/// just-attached subscribers can filter out frames they already
/// received via `HelloChat.replay` (issue #244).
#[derive(Clone, Debug)]
enum ChatEvt {
    Event { seq: u64, json: String },
    Exit(Option<i32>),
}

/// Same chunk-granular eviction strategy as the terminal-mode [`ByteRing`]
/// (in `calm_session::terminal_session`), but each chunk is one
/// serialized NeigeEvent JSON line. Used in chat mode.
///
/// Each event carries a monotonic `seq` allocated on `append`. Snapshots
/// return both the payload and the highest seq stored at snapshot time,
/// so a just-attached subscriber can tell its broadcast receiver to skip
/// any frame whose seq is `<=` that watermark (issue #244 dedup).
struct EventBuffer {
    events: VecDeque<(u64, String)>,
    total_bytes: usize,
    max_bytes: usize,
    /// Monotonic counter handing out the next `seq` on `append`. Starts at
    /// 1 so a snapshot watermark of 0 unambiguously means "nothing in the
    /// replay yet, accept every live frame".
    next_seq: u64,
}

/// A snapshot of the chat-mode replay buffer plus the dedup watermark.
///
/// `last_seq` is the highest `seq` included in `events`, or `0` if the
/// buffer was empty when the snapshot was taken. A client's broadcast
/// receiver MUST skip any incoming `ChatEvt::Event { seq, .. }` with
/// `seq <= last_seq` — those frames are already in `events` and would
/// otherwise be delivered twice.
struct BufferSnapshot {
    events: Vec<String>,
    last_seq: u64,
}

impl EventBuffer {
    fn new(max_bytes: usize) -> Self {
        Self {
            events: VecDeque::new(),
            total_bytes: 0,
            max_bytes,
            next_seq: 1,
        }
    }

    /// Append `json` to the ring and return the `seq` assigned to it.
    /// The caller MUST forward `(seq, json)` to the broadcast channel
    /// while still holding (or at least having held, atomically with)
    /// this append — otherwise a concurrent snapshot could read a
    /// `last_seq` that exceeds anything yet seen on the broadcast.
    fn append(&mut self, json: String) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.total_bytes += json.len();
        self.events.push_back((seq, json));
        while self.total_bytes > self.max_bytes && self.events.len() > 1 {
            let (_, dropped) = self.events.pop_front().unwrap();
            self.total_bytes -= dropped.len();
        }
        seq
    }

    fn snapshot(&self) -> BufferSnapshot {
        let last_seq = self.events.back().map(|(s, _)| *s).unwrap_or(0);
        let events = self.events.iter().map(|(_, j)| j.clone()).collect();
        BufferSnapshot { events, last_seq }
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

/// Bounded fallback timeout for the chat-mode attach handshake to wait on
/// the runner's first emitted frame before sending `HelloChat` (issue #243).
///
/// Normal path: the Node runner emits `session_init` synchronously on
/// startup, well under a second even on contended CI. Attach handshakes
/// wait on that frame so the client's `HelloChat.replay` always contains
/// `session_init` and the post-`HelloChat` live broadcast never has to
/// deliver startup frames.
///
/// Fallback: if the runner is broken (missing binary, crashed before any
/// stdout, infinite startup hang) we still need the handshake to complete
/// so the client gets a clear "empty replay, then ChildExited" signal
/// rather than hanging forever. 5 s mirrors the pre-#241 frame-read
/// timeout — long enough that a healthy-but-slow node cold start has
/// plenty of headroom (worst observed ~165 ms under 6× CPU contention,
/// see #240 diagnostics) while still bounding the handshake within the
/// chat client's own retry budget.
const CHAT_FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(5);

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // #267 — die when the parent calm-server dies. Without this the
    // daemon (and the codex PTY child it owns) outlives every test
    // teardown, panic, manual kill, or container restart and gets
    // reparented to PID 1; over ~4.5 h of accidental test-runner
    // background runtime that left 250+ orphan daemons + 175+ orphan
    // codex CLIs + 134 GB of stale codex-home state. Installed at the
    // top of `main` (before any other tokio work) so the kernel signal
    // (Linux) or the fallback ppid watcher (non-Linux) is armed before
    // we touch the PTY / socket / process tree.
    //
    // #272 (R3) — the tokio SIGTERM/SIGHUP handlers are registered
    // INSIDE `install_parent_death_watcher` (and BEFORE prctl(2) /
    // before the race-guard self-SIGTERM) so they catch the
    // PR_SET_PDEATHSIG-delivered SIGTERM that arrives if the parent
    // already exited between fork(2) and prctl(2). Previously the
    // handlers were registered later inside `run_terminal` /
    // `run_chat`, leaving a sub-ms window where the race-guard
    // self-SIGTERM would hit default disposition (terminate) and
    // `kill_child` would never run — orphaning the codex child.
    let parent_death_signals = install_parent_death_watcher();

    let cli = Cli::parse();
    if let Some(fd) = cli.ready_fd {
        // The parent deliberately clears CLOEXEC so this daemon receives
        // `--ready-fd` across exec. Re-enable it before spawning any PTY or
        // chat child so daemon death still closes the last write end and the
        // parent can observe EOF if we exit before `notify_ready`.
        set_fd_cloexec(fd, true)?;
    }
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
            run_terminal(cli, parent_death_signals).await
        }
        Mode::Chat => {
            if cli.runner_path.is_none() {
                anyhow::bail!("--mode chat requires --runner-path <path-to-cli.js>");
            }
            run_chat(cli, parent_death_signals).await
        }
    }
}

fn set_fd_cloexec(fd: i32, cloexec: bool) -> io::Result<()> {
    // SAFETY: fcntl is called with a live file descriptor and commands
    // that do not require pointer arguments.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags == -1 {
        return Err(io::Error::last_os_error());
    }
    let next = if cloexec {
        flags | libc::FD_CLOEXEC
    } else {
        flags & !libc::FD_CLOEXEC
    };
    // SAFETY: see F_GETFD call above.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, next) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// #272 (R3) — bundle of tokio signal receivers installed by
/// [`install_parent_death_watcher`] BEFORE the prctl race-guard self-
/// SIGTERM fires. The per-mode `run_terminal` / `run_chat` loops
/// `.recv()` on these from inside their main `tokio::select!`.
///
/// SIGTERM is the signal `PR_SET_PDEATHSIG` delivers (Linux) and what
/// the non-Linux ppid-watcher self-delivers. SIGHUP is the
/// controlling-terminal-gone signal with the same "owner is gone"
/// intent. Both are wired to the same shutdown branch in the caller.
struct ParentDeathSignals {
    sigterm: tokio::signal::unix::Signal,
    sighup: tokio::signal::unix::Signal,
}

impl ParentDeathSignals {
    /// Await whichever signal arrives first; return its static name for
    /// the shutdown log line.
    async fn recv(&mut self) -> &'static str {
        tokio::select! {
            _ = self.sigterm.recv() => "SIGTERM",
            _ = self.sighup.recv() => "SIGHUP",
        }
    }
}

/// #267 — register a "die when my parent dies" hook so the daemon doesn't
/// outlive `calm-server`.
///
/// Two implementations:
///
/// - **Linux**: `prctl(PR_SET_PDEATHSIG, SIGTERM, 0, 0, 0)` — the kernel
///   delivers SIGTERM to this process the instant its parent task exits
///   (i.e. when the reparenting to PID 1 happens). Tokio's signal handler
///   below catches the SIGTERM and runs the graceful-shutdown path that
///   tears down the PTY child too. We also defensively check ppid right
///   away: if the parent already exited between fork and prctl install
///   (the documented race for PR_SET_PDEATHSIG), we self-deliver SIGTERM
///   so the shutdown handler still runs.
///
/// - **Other unix (macOS, BSDs)**: no kernel equivalent. We spawn a
///   tokio interval that polls `getppid()` every 500 ms and self-kills
///   with SIGTERM when the ppid changes from its launch-time value (or
///   becomes 1, the reparent-to-init sentinel). Same downstream effect.
///
/// Regardless of platform the signal handler (`spawn_shutdown_signal_handler`)
/// translates SIGTERM/SIGHUP into a process-wide shutdown flag the
/// per-mode `run_terminal` / `run_chat` loops poll, which then kills
/// the codex child via the existing `kill_child` (terminal mode) or
/// drops the runner's stdin (chat mode) so codex sees EOF and exits.
///
/// #272 (R3) — the tokio SIGTERM/SIGHUP handlers are installed *first*
/// inside this function, **before** the prctl(2) call and **before** the
/// race-guard self-SIGTERM. Previously they were installed later from
/// inside `run_terminal` / `run_chat`, leaving a sub-millisecond window
/// in which the race-guard SIGTERM (or an honest kernel-delivered SIGTERM
/// arriving immediately after prctl install) hit the default
/// disposition (terminate) — daemon dies, `kill_child` never runs, codex
/// orphans. The handler must exist before any signal can be delivered to
/// us, so install order is strictly: signal handlers → prctl → race-guard
/// check → (non-Linux) ppid poller spawn.
fn install_parent_death_watcher() -> ParentDeathSignals {
    use tokio::signal::unix::{SignalKind, signal};
    // Step 1: install the tokio SIGTERM / SIGHUP handlers BEFORE any
    // signal can be delivered (prctl race-guard, ppid poller, kernel
    // PR_SET_PDEATHSIG delivery). `expect` is appropriate: this is a
    // one-shot setup call at daemon boot; if signal registration fails
    // the process is misconfigured beyond what graceful shutdown can
    // fix.
    let sigterm = signal(SignalKind::terminate()).expect("install SIGTERM handler");
    let sighup = signal(SignalKind::hangup()).expect("install SIGHUP handler");

    // Step 2: arm the kernel parent-death hook (Linux) or its
    // poller fallback (other unix).
    #[cfg(target_os = "linux")]
    unsafe {
        // SAFETY: prctl(PR_SET_PDEATHSIG, ...) takes a signal number;
        // the call only writes a per-task kernel flag, no userspace
        // state is touched. Returning <0 only means EINVAL on an
        // unsupported signal, which `SIGTERM` is not.
        if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM, 0, 0, 0) != 0 {
            tracing::warn!(
                error = %std::io::Error::last_os_error(),
                "prctl(PR_SET_PDEATHSIG) failed; daemon may outlive its parent"
            );
        }
        // Race-guard: the parent may have exited between our fork(2)
        // and the prctl(2) above, in which case the kernel never
        // delivers the death signal (PR_SET_PDEATHSIG only fires on
        // transitions). If ppid is already 1 we self-deliver SIGTERM
        // so the shutdown handler still runs. The tokio SIGTERM
        // receiver installed in step 1 above is already armed, so
        // this self-deliver lands on `sigterm.recv()` rather than
        // the default-disposition terminate path.
        if libc::getppid() == 1 {
            tracing::warn!(
                "parent already exited before PR_SET_PDEATHSIG installed; self-terminating"
            );
            libc::kill(libc::getpid(), libc::SIGTERM);
        }
    }

    #[cfg(all(unix, not(target_os = "linux")))]
    {
        // SAFETY: getppid is a pure read of kernel state; no preconditions.
        let initial_ppid = unsafe { libc::getppid() };
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(500));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            // First tick is immediate — skip so we don't fire before
            // anyone is listening for SIGTERM.
            tick.tick().await;
            loop {
                tick.tick().await;
                let cur = unsafe { libc::getppid() };
                if cur != initial_ppid || cur == 1 {
                    tracing::warn!(initial_ppid, cur, "parent process exited; self-terminating");
                    // SAFETY: kill on self is always safe.
                    unsafe {
                        libc::kill(libc::getpid(), libc::SIGTERM);
                    }
                    break;
                }
            }
        });
    }

    ParentDeathSignals { sigterm, sighup }
}

async fn run_terminal(cli: Cli, mut parent_death: ParentDeathSignals) -> anyhow::Result<()> {
    // Fail-fast on missing theme. `required_if_eq("mode", "terminal")`
    // only fires when `--mode` is set explicitly — when clap takes the
    // default it doesn't, so we re-check here. PR1 (#262) made theme a
    // NOT NULL row invariant; PR2 (#262 follow-up) wires the kernel-
    // side spawn helper to thread it through. A missing flag at this
    // point means a kernel-side regression — surface it loudly rather
    // than degrade to a built-in default. See `--terminal-fg` /
    // `--terminal-bg` docs on `Cli`.
    if cli.terminal_fg.is_none() {
        anyhow::bail!(
            "--terminal-fg is required in terminal mode (#177). The kernel must \
             thread `terminals.theme_fg` through `spawn_daemon_with_parts`."
        );
    }
    if cli.terminal_bg.is_none() {
        anyhow::bail!(
            "--terminal-bg is required in terminal mode (#177). The kernel must \
             thread `terminals.theme_bg` through `spawn_daemon_with_parts`."
        );
    }

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

    // `with_colors` pre-seeds the OSC 10/11 reply colors so codex's
    // startup probe (#177) gets an authoritative answer from the very
    // first feed. `cli.terminal_fg/_bg` are `Option<(u8,u8,u8)>` on the
    // struct only because the same struct doubles for chat mode (where
    // the args don't apply); in terminal mode clap's
    // `required_if_eq("mode", "terminal")` guarantees both are `Some`
    // by the time we reach here. We still pass through `Option` to
    // match `with_colors`' signature, which keeps `set_default_colors`
    // (used by the mid-session ThemeUpdate path and unit tests) free
    // to revert to "silent" semantics if a future feature needs it.
    let render_plane: SharedRenderPlane = Arc::new(Mutex::new(RenderPlane::with_colors(
        cli.cols,
        cli.rows,
        cli.buffer_bytes,
        SCROLLBACK_MAX_LINES,
        cli.terminal_fg,
        cli.terminal_bg,
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
    spawn_pty_reader(
        reader,
        render_plane.clone(),
        event_tx.clone(),
        stdin_tx.clone(),
    );

    // ---- PTY writer ← client stdin ----
    let writer = master.lock().unwrap().take_writer()?;
    spawn_pty_writer(writer, stdin_rx);

    // ---- Child-exit watcher ----
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    spawn_child_waiter(
        child,
        render_plane.clone(),
        event_tx.clone(),
        shutdown_tx,
        cli.sock.clone(),
    );

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

    // Block until either the child exits OR a parent-death signal
    // (SIGTERM / SIGHUP) fires. #267: in the SIGTERM branch we proactively
    // tear down the codex child so it doesn't outlive us as an orphan —
    // PR_SET_PDEATHSIG fires only on us, not on our descendants, and
    // codex would otherwise survive once our PTY master fd is closed in
    // an unclean way.
    tokio::select! {
        _ = shutdown_rx => {
            tracing::info!("child exited, shutting down");
        }
        sig = parent_death.recv() => {
            tracing::info!(?sig, "received parent-death / terminate signal; killing PTY child");
            kill_child(&master, &killer);
        }
    }

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

async fn run_chat(cli: Cli, mut parent_death: ParentDeathSignals) -> anyhow::Result<()> {
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

    // #272 (N1) — put the runner in its own process group so the
    // parent-death shutdown branch can `killpg(-pgid, SIGHUP)` /
    // `SIGKILL` it without taking the daemon down with it. Without
    // `process_group(0)` the runner shares the daemon's pgid and a
    // negative-pid kill targeting the runner would also signal the
    // daemon. Equivalent to portable-pty's `setsid` for terminal-mode
    // children; lets us reuse the same SIGHUP-then-2 s-SIGKILL safety
    // net `kill_child` uses in `run_terminal`.
    cmd.process_group(0);

    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to spawn `node {}`: {e}", runner_path.display()))?;
    // Capture the pid before the wait-task below takes ownership of
    // `child`. With `process_group(0)` above this pid == the runner's
    // pgid, so a `killpg(-pid)` reaches the runner and every
    // descendant it later forks (e.g. SDK subprocesses).
    let runner_pid = child.id().map(|p| p as i32);
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

    // Signals "runner has emitted at least one frame" (issue #243). The
    // attach handshake awaits this with a bounded timeout before sending
    // `HelloChat`, so the replay always includes `session_init` on the
    // happy path and live broadcast never carries startup frames.
    let (first_frame_tx, first_frame_rx) = watch::channel(false);

    spawn_chat_stdout_reader(
        child_stdout,
        buffer.clone(),
        event_tx.clone(),
        first_frame_tx,
    );
    spawn_chat_stderr_reader(child_stderr);
    spawn_chat_stdin_writer(child_stdin, stdin_rx);

    // ---- child-exit watcher ----
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let event_tx_w = event_tx.clone();
    let chat_exit_sock = cli.sock.clone();
    tokio::spawn(async move {
        let status = child.wait().await.ok();
        let code = status.and_then(|s| s.code());
        tracing::info!(?code, "chat runner wait returned");
        // #306 — chat-mode `Child` is `tokio::process::Child` whose
        // `ExitStatus` doesn't expose the underlying signal name on
        // this path, so for v1 we just record the numeric code (or
        // None) and stamp `signal_killed = false`. Matches terminal-
        // mode's sidecar shape so the kernel can read either branch
        // with the same parser.
        let exit_path = format!("{}.exit", chat_exit_sock.display());
        let payload = serde_json::json!({
            "code": code,
            "signal_killed": false,
        });
        if let Err(e) = std::fs::write(&exit_path, payload.to_string()) {
            tracing::warn!(error = %e, path = %exit_path, "failed to write .exit sidecar (chat)");
        }
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
        first_frame_rx,
    ));

    // #267: same shutdown-on-parent-death pattern as `run_terminal`.
    // Dropping the control-frame sender below makes the chat runner
    // see EOF on stdin and exit cleanly — the typical case.
    //
    // #272 (N1): the EOF-via-stdin-drop is still the happy path, but
    // a hung or stuck runner won't notice EOF in time, so we also
    // signal-group the runner: SIGHUP to its pgid (set via
    // `process_group(0)` at spawn) with a 2 s SIGKILL fallback.
    // Mirrors what `kill_child` does in terminal mode. Without this
    // safety net a Node runner that traps SIGPIPE or ignores stdin
    // EOF would survive daemon shutdown as an orphan grandparent of
    // every codex-SDK subprocess it had open. The OS would NOT reap
    // it automatically: `kill_on_drop(false)` is intentional and the
    // runner is in its own pgid, so the daemon's exit cascade
    // doesn't reach it.
    tokio::select! {
        _ = shutdown_rx => {
            tracing::info!("chat runner exited, shutting down");
        }
        sig = parent_death.recv() => {
            tracing::info!(?sig, "received parent-death / terminate signal; closing runner stdin + signaling runner group");
            // Dropping stdin_tx — the unique sender we hold here —
            // causes `spawn_chat_stdin_writer` to drop its
            // `ChildStdin`, the runner sees EOF, and exits cleanly.
            drop(stdin_tx);
            // Safety net for an unresponsive runner. `runner_pid` is
            // also the runner's pgid because we spawned it with
            // `process_group(0)`. Awaited inline (not `tokio::spawn`d)
            // so the SIGHUP + 2 s SIGKILL fallback actually runs
            // before this function returns and the tokio runtime
            // tears down on daemon exit — otherwise the spawned task
            // would be cancelled mid-sleep and an unresponsive runner
            // would be reparented to init still alive.
            if let Some(pid) = runner_pid {
                kill_chat_runner_group(pid).await;
            }
        }
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    accept_task.abort();

    let _ = std::fs::remove_file(&cli.sock);
    Ok(())
}

/// #272 (N1) — chat-mode counterpart to `kill_child`. SIGHUP the
/// runner's process group, then if it's still around after 2 s,
/// SIGKILL the same pgid. The runner is spawned with
/// `process_group(0)` so `pgid == pid`; signaling the negative pgid
/// catches every SDK subprocess the runner forked too.
///
/// Awaited (not spawned) by `run_chat`'s shutdown branch so the
/// fallback actually fires before the daemon process — and the tokio
/// runtime — tears down. A `tokio::spawn`'d sleep would be cancelled
/// at runtime shutdown and an unresponsive runner would survive
/// orphan'd to init.
async fn kill_chat_runner_group(pid: i32) {
    // SAFETY: killpg-style negative pid targets the process group with
    // the matching id. We created this pgid via `process_group(0)` at
    // spawn time and captured the resulting pid before the wait-task
    // took ownership of the `Child`.
    unsafe {
        libc::kill(-pid, libc::SIGHUP);
    }
    // Poll for runner exit on a short tick so we don't wait the full
    // 2 s when SIGHUP did the job (the common case). `kill(0)` is a
    // "does this pgid exist" probe — returns 0 if at least one
    // member is alive, -1/ESRCH when the whole group is gone.
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        // SAFETY: kill(-pid, 0) only signals zero — no side effects.
        let alive = unsafe { libc::kill(-pid, 0) } == 0;
        if !alive {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tracing::warn!(
        pid,
        "chat runner ignored SIGHUP for 2 s; sending SIGKILL to pgid"
    );
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
    }
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
    stdin_tx: mpsc::UnboundedSender<PtyWrite>,
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
                    apply_broadcaster_effects(&event_tx, &stdin_tx, effects);
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
/// `RenderPlane::on_pty_chunk` emits `Broadcast(RenderPatch)` and — when
/// the child probed OSC 10/11 — a `WriteToPty` carrying the synthetic
/// reply (#177). All other effects are unreachable from this caller; we
/// match exhaustively so a future `Effect` addition is a compile error
/// here rather than a silent drop.
fn apply_broadcaster_effects(
    tx: &broadcast::Sender<DaemonMsg>,
    stdin_tx: &mpsc::UnboundedSender<PtyWrite>,
    effects: Vec<Effect>,
) {
    for eff in effects {
        match eff {
            Effect::Broadcast(msg) => {
                let _ = tx.send(msg);
            }
            Effect::WriteToPty { data, input_seq } => {
                // OSC 10/11 reply synthesized by `TerminalModel` while
                // feeding the current chunk. Daemon-originated write,
                // not client-originated — no ack needed regardless of
                // `input_seq`.
                let _ = stdin_tx.send(PtyWrite {
                    data,
                    input_seq,
                    ack: None,
                });
            }
            // RenderPlane never emits these; the other variants belong
            // to the client-frame state machine.
            Effect::SendToClient(_)
            | Effect::ResizePty { .. }
            | Effect::KillChild
            | Effect::SendProtocolError { .. }
            | Effect::CloseConnection
            | Effect::AssignOwner(_)
            | Effect::BroadcastOwnerChanged(_)
            | Effect::ProtocolViolation(_)
            | Effect::TerminalThemeUpdate { .. } => {
                tracing::warn!(
                    "RenderPlane emitted non-Broadcast non-WriteToPty effect; dropping (this is a bug)"
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
    sock_path: PathBuf,
) {
    tokio::task::spawn_blocking(move || {
        let status = child.wait().ok();
        // #306 — capture both exit_code and signal_killed before the
        // broadcast effects fire, so by the time the WS pump sees the
        // socket EOF the kernel can read the sidecar file off disk and
        // persist the exit info on the terminal row. portable-pty 0.9
        // gained `ExitStatus::signal() -> Option<&str>`, which is the
        // discriminator we need: `Some(_)` ⇒ killed by signal,
        // `None`     ⇒ returned via exit() / main-return.
        let (code, signal_killed): (Option<i32>, bool) = match status.as_ref() {
            Some(s) if s.signal().is_some() => (None, true),
            Some(s) => (Some(s.exit_code() as i32), false),
            None => (None, false),
        };
        tracing::info!(?code, signal_killed, "child wait returned");
        // Write `.exit` sidecar BEFORE the broadcast effects fire. By
        // the time the WS pump observes the socket close, the kernel's
        // `resolve_live_sock` (and the pump-time `Close(1000, …)` path)
        // can read this file and persist the exit info on the terminal
        // row via `terminal_set_exit`. JSON shape matches what
        // `crates/calm-server/src/ws/terminal.rs` expects.
        let exit_path = format!("{}.exit", sock_path.display());
        let payload = serde_json::json!({
            "code": code,
            "signal_killed": signal_killed,
        });
        if let Err(e) = std::fs::write(&exit_path, payload.to_string()) {
            // Non-fatal: the kernel will treat the missing sidecar as
            // "daemon died without writing exit info" (DaemonLost in
            // v2). Logged so an operator can dig into it.
            tracing::warn!(error = %e, path = %exit_path, "failed to write .exit sidecar");
        }
        // `on_child_exit` only emits `Broadcast(TerminalExited)`; inline
        // the dispatch so we don't need to thread `stdin_tx` here.
        let effects = match render_plane.lock() {
            Ok(mut rp) => rp.on_child_exit(code),
            Err(_) => Vec::new(),
        };
        for eff in effects {
            if let Effect::Broadcast(msg) = eff {
                let _ = event_tx.send(msg);
            }
        }
        let _ = shutdown_tx.send(());
    });
}

/// Read NDJSON from the chat-mode runner's stdout. Each non-empty line is
/// already a serialized `NeigeEvent` JSON string — the daemon does NOT
/// parse it. We push the line into the replay buffer and broadcast it
/// verbatim to every attached client.
///
/// On the very first non-empty frame we flip `first_frame_tx` to `true`
/// AFTER the line has landed in the replay buffer. The attach handshake
/// (`handle_chat_client`) awaits this signal before sending `HelloChat`,
/// guaranteeing that `HelloChat.replay` always carries `session_init`
/// (and any frames that beat the attach) on the happy path — see #243.
fn spawn_chat_stdout_reader(
    stdout: tokio::process::ChildStdout,
    buffer: SharedEventBuffer,
    event_tx: broadcast::Sender<ChatEvt>,
    first_frame_tx: watch::Sender<bool>,
) {
    tokio::spawn(async move {
        let mut signalled_first_frame = false;
        let mut lines = BufReader::new(stdout).lines();
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => {
                    if line.trim().is_empty() {
                        continue;
                    }
                    // Allocate the seq, append to replay, AND send onto the
                    // broadcast channel — in that order — so a snapshot
                    // taken on another task can never observe a `last_seq`
                    // greater than what the broadcast has actually
                    // delivered. The downstream dedup invariant is "frame
                    // S included in snapshot → all `seq <= S` frames are
                    // dupes from the new subscriber's perspective", which
                    // requires snapshot's last_seq to never exceed the
                    // broadcast's emitted-seq watermark.
                    let seq = match buffer.lock() {
                        Ok(mut b) => b.append(line.clone()),
                        Err(_) => {
                            tracing::warn!("chat event buffer poisoned; dropping line");
                            continue;
                        }
                    };
                    if !signalled_first_frame {
                        // Order matters: the buffer.append() above must
                        // run first so any handshake that wakes on this
                        // signal sees a non-empty replay (#243).
                        let _ = first_frame_tx.send(true);
                        signalled_first_frame = true;
                    }
                    let _ = event_tx.send(ChatEvt::Event { seq, json: line });
                }
                Ok(None) => break, // EOF
                Err(e) => {
                    tracing::warn!(error = %e, "chat stdout read error; stopping reader");
                    break;
                }
            }
        }
        // Drop the sender on EOF/error so any handshake still waiting on
        // `changed()` wakes up immediately and falls through to the
        // empty-replay branch via the bounded timeout (or, if it's
        // already past the timeout, this is a no-op).
        drop(first_frame_tx);
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
    first_frame_rx: watch::Receiver<bool>,
) {
    loop {
        match listener.accept().await {
            Ok((sock, _)) => {
                let event_rx = event_tx.subscribe();
                let buffer = buffer.clone();
                let stdin_tx = stdin_tx.clone();
                let first_frame_rx = first_frame_rx.clone();
                tokio::spawn(async move {
                    if let Err(e) =
                        handle_chat_client(sock, event_rx, buffer, stdin_tx, first_frame_rx).await
                    {
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
            // Current OSC 10/11 colors — lets the state machine drop a
            // redundant mount-time `TerminalThemeUpdate` (fix A).
            current_default_fg: guard.default_fg(),
            current_default_bg: guard.default_bg(),
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
            | Effect::BroadcastOwnerChanged(_)
            | Effect::TerminalThemeUpdate { .. } => {
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
                // Current OSC 10/11 colors — the `TerminalThemeUpdate`
                // handler reads these to suppress a no-op theme update
                // (fix A). Post-handshake this is the live path: a
                // mount-time update whose colors equal the spawn theme
                // is dropped here before it ever reaches the PTY.
                current_default_fg: guard.default_fg(),
                current_default_bg: guard.default_bg(),
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
                Effect::TerminalThemeUpdate { fg, bg } => {
                    // Mid-session theme toggle (#177, refined by #295
                    // followup 1 — solicited-only model). Two side
                    // effects:
                    //
                    // (a) Update the model's default fg/bg so the
                    //     **next solicited** OSC 10;? / OSC 11;? query
                    //     from the child gets the new color. (Always
                    //     runs; the model's advertised colors must
                    //     track the host theme so a future query, e.g.
                    //     from a TUI launched later, gets the right
                    //     answer.)
                    //
                    // (b) Write a focus-in CSI (`ESC [ I`) to the PTY
                    //     so a focus-aware TUI (codex, claude-tui)
                    //     re-queries OSC 10/11. crossterm parses
                    //     `ESC[I` as `FocusGained`; codex handles
                    //     `FocusGained` by calling
                    //     `terminal_palette::requery_default_colors()`
                    //     which emits `OSC 10;? + OSC 11;?`. The
                    //     daemon's vte parser then synthesizes the
                    //     reply from (a), closing the loop. So
                    //     focus-in alone is sufficient — no unsolicited
                    //     OSC bytes needed.
                    //
                    // We previously also wrote unsolicited
                    // `OSC 10;rgb:… OSC 11;rgb:…` pairs ahead of the
                    // focus-in (PR #296). That was double-belt —
                    // codex's focus-in re-query was already closing
                    // the loop — and it forced a DECSET-1004 gate to
                    // avoid echoing color bytes to non-1004 children
                    // (shells at a prompt). The solicited path needs
                    // no such gate on the unsolicited bytes (there are
                    // none), so the only remaining concern is the
                    // focus-in itself.
                    //
                    // The DECSET-1004 gate is **kept** for `ESC[I`:
                    // mirrors zellij
                    // (zellij-server/src/panes/grid.rs `focus_event`),
                    // which emits focus-in only to children that
                    // opted into 1004. A shell at its prompt never
                    // enables 1004; sending it `ESC[I` would be a
                    // stray byte in its raw-mode line editor. (Also
                    // note: codex enables 1004 and re-queries on
                    // focus-in — both halves of the contract.) Do
                    // NOT gate on alt-screen or DECSET 2031: codex
                    // uses neither.
                    let focus_event_tracking = if let Ok(mut rp) = render_plane.lock() {
                        rp.set_default_colors(Some(fg), Some(bg));
                        rp.focus_event_tracking()
                    } else {
                        // Poisoned lock — fail toward NOT injecting. A
                        // dropped theme nudge is invisible; a stray
                        // `ESC[I` at a shell prompt is not.
                        false
                    };
                    if !focus_event_tracking {
                        continue;
                    }
                    if stdin_tx
                        .send(PtyWrite {
                            data: b"\x1b[I".to_vec(),
                            input_seq: 0,
                            ack: None,
                        })
                        .is_err()
                    {
                        closed = true;
                        break;
                    }
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
    mut first_frame_rx: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let (mut rd, mut wr) = sock.into_split();

    // First frame must be ClientHello (v2). cols/rows in the desired_size
    // field are placeholder in chat mode — chat has no PTY to resize.
    let first: ClientMsg = read_frame(&mut rd).await?;
    match first {
        ClientMsg::ClientHello { .. } => {}
        other => anyhow::bail!("expected ClientHello as first message, got {other:?}"),
    }

    // Defer the HelloChat send until the runner has emitted at least one
    // frame (#243). If we attach before the runner's first frame is in
    // the replay buffer, the client would have to wait on live broadcast
    // for `session_init`, which is racy under CPU contention. Once the
    // watch flips to `true` (or the channel is closed by the reader
    // dropping the sender on EOF), we proceed.
    //
    // If `*borrow() == true` already (later attachers, common case once
    // any frame has landed), this is a non-suspending wait_for that
    // returns immediately.
    //
    // The bounded `CHAT_FIRST_FRAME_TIMEOUT` is a safety net for a
    // broken runner (missing binary, crash before any stdout) — we
    // still send `HelloChat` with an empty replay so the client can
    // observe the subsequent `ChildExited` cleanly instead of hanging.
    if !*first_frame_rx.borrow_and_update() {
        match timeout(
            CHAT_FIRST_FRAME_TIMEOUT,
            first_frame_rx.wait_for(|ready| *ready),
        )
        .await
        {
            Ok(Ok(_)) => {}
            Ok(Err(_)) => {
                // Watch channel sender dropped (stdout reader returned)
                // without ever signalling — runner died before emitting.
                tracing::warn!(
                    "chat runner closed stdout without emitting any frame; sending empty HelloChat replay"
                );
            }
            Err(_) => {
                tracing::warn!(
                    timeout_ms = CHAT_FIRST_FRAME_TIMEOUT.as_millis() as u64,
                    "chat runner did not emit first frame within timeout; sending empty HelloChat replay"
                );
            }
        }
    }

    // Snapshot the replay buffer together with its dedup watermark.
    // Any broadcast frame whose `seq` is `<= last_replay_seq` is already
    // in `replay` and must be filtered out of the live fan-out, otherwise
    // a frame appended-and-broadcast between this subscriber's
    // `event_tx.subscribe()` and the snapshot below would be delivered
    // twice (once via HelloChat, once via the broadcast). See issue #244.
    let BufferSnapshot {
        events: replay,
        last_seq: last_replay_seq,
    } = {
        let b = buffer.lock().unwrap();
        b.snapshot()
    };
    write_frame(&mut wr, &DaemonMsg::HelloChat { replay }).await?;

    let down_task = tokio::spawn(async move {
        loop {
            match event_rx.recv().await {
                Ok(ChatEvt::Event { seq, json }) => {
                    // Skip frames already delivered to this subscriber
                    // via the HelloChat replay snapshot — dedup gate
                    // closing the broadcast-vs-replay race (issue #244).
                    if seq <= last_replay_seq {
                        continue;
                    }
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
            | ClientMsg::RenderAck { .. }
            | ClientMsg::TerminalThemeUpdate { .. } => {
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

    /// `EventBuffer::append` allocates strictly-increasing seqs and
    /// `snapshot` returns `last_seq == seq of the most recent append`.
    /// Empty buffer reports `last_seq == 0` (the dedup sentinel — "no
    /// replay watermark, never skip").
    #[test]
    fn event_buffer_seq_is_monotonic_and_snapshot_reports_watermark() {
        let mut buf = EventBuffer::new(64 * 1024);
        let empty = buf.snapshot();
        assert!(empty.events.is_empty());
        assert_eq!(empty.last_seq, 0);

        let s1 = buf.append("a".to_string());
        let s2 = buf.append("b".to_string());
        let s3 = buf.append("c".to_string());
        assert!(s1 < s2 && s2 < s3, "seqs must be strictly increasing");

        let snap = buf.snapshot();
        assert_eq!(snap.events, vec!["a", "b", "c"]);
        assert_eq!(snap.last_seq, s3);
    }

    /// Eviction shrinks the deque but `next_seq` keeps growing, so
    /// `last_seq` after eviction is still the seq of the latest
    /// `append` — never the seq of an evicted older frame.
    #[test]
    fn event_buffer_eviction_preserves_seq_monotonicity() {
        // max_bytes=3 forces eviction once we hit 4 single-char frames.
        let mut buf = EventBuffer::new(3);
        let s1 = buf.append("a".to_string());
        let s2 = buf.append("b".to_string());
        let s3 = buf.append("c".to_string());
        let s4 = buf.append("d".to_string()); // evicts "a"
        assert_eq!(s1, 1);
        assert_eq!(s4, 4);
        let snap = buf.snapshot();
        // "a" got evicted; "b","c","d" remain. Watermark is still s4 (4).
        assert_eq!(snap.events, vec!["b", "c", "d"]);
        assert_eq!(snap.last_seq, s4);
        let _ = s2;
        let _ = s3;
    }

    /// Reproduces issue #244 race deterministically with a synchronous
    /// timeline, then asserts the daemon-side dedup filter (`seq <=
    /// last_replay_seq → skip`) prevents double delivery.
    ///
    /// Timeline (matches the real chat-mode plumbing):
    ///   T0  subscribe — receiver attached
    ///   T1  writer appends frame A → snapshot would now include A
    ///   T2  writer broadcasts frame A → receiver gets A live
    ///   T3  handle_chat_client takes snapshot — `events=[A]`, last_seq=1
    ///   T4  receiver loop pulls A off the channel — would deliver twice
    ///       without the dedup gate
    ///
    /// Pre-fix expectation: receiver sees A via broadcast even though A
    /// is in HelloChat.replay (the bug). Fix expectation: filter skips A.
    #[tokio::test(flavor = "current_thread")]
    async fn broadcast_dedup_skips_frame_already_in_hello_chat_replay() {
        let buffer = std::sync::Arc::new(std::sync::Mutex::new(EventBuffer::new(64 * 1024)));
        let (tx, mut rx) = broadcast::channel::<ChatEvt>(64);

        // T0: subscriber attached (rx is alive).
        // T1+T2: writer appends + broadcasts frame A (atomic from the
        //        subscriber's POV — both happen before snapshot).
        let seq_a = {
            let mut b = buffer.lock().unwrap();
            b.append(r#"{"type":"session_init","session_id":"A"}"#.to_string())
        };
        let _ = tx.send(ChatEvt::Event {
            seq: seq_a,
            json: r#"{"type":"session_init","session_id":"A"}"#.to_string(),
        });

        // T3: client takes the HelloChat snapshot.
        let snap = buffer.lock().unwrap().snapshot();
        assert_eq!(snap.events.len(), 1);
        assert_eq!(snap.last_seq, seq_a);
        let last_replay_seq = snap.last_seq;

        // T4: receiver pulls the broadcast frame. Without dedup this
        // would deliver "A" to the client a second time (the bug).
        let delivered = match rx.try_recv() {
            Ok(ChatEvt::Event { seq, json }) => {
                if seq <= last_replay_seq {
                    None // dedup gate skipped — correct
                } else {
                    Some(json) // bug: would forward to socket
                }
            }
            Ok(other) => panic!("unexpected variant: {other:?}"),
            Err(broadcast::error::TryRecvError::Empty) => {
                panic!("broadcast did not deliver frame A — test setup wrong")
            }
            Err(e) => panic!("broadcast recv err: {e:?}"),
        };
        assert!(
            delivered.is_none(),
            "frame already in HelloChat.replay was re-delivered via broadcast: {delivered:?}"
        );

        // Sanity: a *new* frame B (seq > last_replay_seq) MUST still pass
        // through — the dedup gate only suppresses dupes, not live data.
        let seq_b = {
            let mut b = buffer.lock().unwrap();
            b.append(r#"{"type":"chat","content":"B"}"#.to_string())
        };
        let _ = tx.send(ChatEvt::Event {
            seq: seq_b,
            json: r#"{"type":"chat","content":"B"}"#.to_string(),
        });
        match rx.try_recv() {
            Ok(ChatEvt::Event { seq, json }) => {
                assert!(
                    seq > last_replay_seq,
                    "B's seq must exceed replay watermark"
                );
                assert!(
                    json.contains("\"content\":\"B\""),
                    "expected live B, got {json}"
                );
            }
            other => panic!("expected live frame B, got {other:?}"),
        }
    }

    /// Same race but in stress-loop form to catch any regression where
    /// the seq allocation, broadcast, or filter drifts out of sync. 1000
    /// iterations × 2 frames per iteration; each round MUST see exactly
    /// one non-skipped live frame (the post-snapshot one) and zero
    /// dupes.
    #[tokio::test(flavor = "current_thread")]
    async fn broadcast_dedup_stress_loop_no_dupes() {
        for _ in 0..1000 {
            let buffer = std::sync::Arc::new(std::sync::Mutex::new(EventBuffer::new(64 * 1024)));
            let (tx, mut rx) = broadcast::channel::<ChatEvt>(64);

            // Pre-snapshot frame (would-be dupe).
            let s1 = buffer.lock().unwrap().append("pre".to_string());
            let _ = tx.send(ChatEvt::Event {
                seq: s1,
                json: "pre".to_string(),
            });

            let snap = buffer.lock().unwrap().snapshot();
            let last_replay_seq = snap.last_seq;

            // Post-snapshot frame (live, must pass through).
            let s2 = buffer.lock().unwrap().append("post".to_string());
            let _ = tx.send(ChatEvt::Event {
                seq: s2,
                json: "post".to_string(),
            });

            let mut live_delivered: Vec<String> = Vec::new();
            while let Ok(ChatEvt::Event { seq, json }) = rx.try_recv() {
                if seq > last_replay_seq {
                    live_delivered.push(json);
                }
            }
            assert_eq!(
                live_delivered,
                vec!["post".to_string()],
                "expected exactly one live frame (\"post\"); pre-snapshot frame must be filtered"
            );
        }
    }
}
