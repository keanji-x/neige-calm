//! The connection loop (#1699). One task, one `select!` over the two
//! wires, lines parsed on both sides so the pump knows which requests
//! the kernel still owes an answer to.
//!
//! When the kernel hangs up while codex's stdin is still open, the pump
//! reconnects on the same path with exponential backoff, replays the
//! cached token-injected `initialize`, swallows the replayed handshake
//! response, and resumes. Requests that were written but unanswered at
//! the moment of the hang-up get a synthesized `-32000` error right
//! then (the kernel may have executed them; see
//! [`frames::lost_error_frame`]); a request whose socket write failed
//! was never queued and is re-sent after the handshake (notifications
//! and other frames are not). Socket writes are driven one `write` at a
//! time from inside the same `select!`, so the socket keeps being read
//! while a large request is in flight: the kernel is serial and would
//! otherwise block writing a response while we block writing a request.
//! The kernel side of this contract is
//! `calm-server/src/mcp_server/transport.rs` (`handle_connection`): one
//! identity per connection, bound by `initialize`; serial
//! request/response; notifications dropped; the kernel never sends
//! requests.

use std::io::{self, Write};
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader, Stdin, Stdout};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

use crate::budget::{INITIAL_CONNECT_BUDGET, RECONNECT_BUDGET, ReconnectBudget};
use crate::frames::{self, Frame, RPC_INTERNAL_ERROR, classify, lost_error_frame};

/// How the pump ended; `main` maps these to exit codes.
pub(crate) enum Exit {
    /// Both sides closed in the clean order (stdin first), or stdin
    /// closed while the shim was between connections.
    Clean,
    /// The first connection never came up within the budget.
    InitialConnectFailed,
    /// The kernel answered an `initialize` with a non-retryable error.
    InitializeRejected,
    /// A reconnect budget ran out.
    BudgetExhausted,
}

/// The `initialize` codex sent, token already injected, kept for replay.
struct CachedInitialize {
    frame: String,
    id: serde_json::Value,
    /// codex has received a response to it, so a replayed response is
    /// swallowed instead of forwarded.
    acked: bool,
}

/// Why one connection ended.
enum Ended {
    /// Socket EOF/error after stdin had already closed.
    Clean,
    /// Socket EOF/error, or a socket write failure, with stdin open.
    Lost,
    /// An `initialize` (original or replayed) answered -32603. The
    /// kernel drops the connection after any handshake error, so this
    /// is an outage too; it runs under the same budget as `Lost` but
    /// nothing was lost, so the stderr line says what happened instead.
    InitializeRetry,
    /// A handshake response with a non-retryable error.
    Rejected,
    /// stdin closed while waiting for a replayed handshake response.
    StdinClosed,
    /// The outage deadline passed while a replayed handshake was pending.
    BudgetExhausted,
}

/// A frame being written to the socket, one `write` per `select!` turn.
struct PendingWrite {
    kind: WriteKind,
    bytes: Vec<u8>,
    off: usize,
}

/// What to record when a pending write completes or fails.
enum WriteKind {
    /// A request from codex: its id joins `outstanding` on completion;
    /// on failure the frame becomes `held`.
    Request(serde_json::Value),
    /// The held request being re-sent after a handshake; same
    /// bookkeeping as `Request`.
    Held(serde_json::Value),
    /// The cached `initialize`, original or replayed: the cache is the
    /// record, so nothing is recorded either way.
    Initialize,
    /// A notification, response or unclassifiable line: forwarded once,
    /// never re-sent.
    Passthrough,
}

impl PendingWrite {
    fn new(kind: WriteKind, bytes: Vec<u8>) -> Self {
        Self {
            kind,
            bytes,
            off: 0,
        }
    }
}

/// Result of probing stdin during a reconnect wait.
enum Probe {
    Eof,
    Data,
}

/// What a socket line means while a replayed handshake is pending.
enum Replay {
    Accepted,
    Retry,
    Rejected,
}

struct Pump {
    socket_path: String,
    token: String,
    stdin: BufReader<Stdin>,
    /// Partial line carried across cancelled `next_line` steps.
    stdin_acc: Vec<u8>,
    stdin_eof: bool,
    stdout: Stdout,
    init: Option<CachedInitialize>,
    /// ids of requests written to the kernel and not yet answered, in
    /// send order.
    outstanding: Vec<serde_json::Value>,
    /// A request whose socket write failed or was still in flight when
    /// the connection ended, re-sent after the next handshake. A write
    /// to a dead peer fails before the first byte, and a frame cut
    /// short has no newline, so the kernel's `read_line` framer never
    /// dispatches it either way.
    held: Option<(serde_json::Value, Vec<u8>)>,
    /// Set from the moment a connection is lost until a handshake on a
    /// new one is accepted; the -32603 retry path keeps it set so one
    /// budget covers the whole outage.
    reconnecting: bool,
    /// The socket write in flight, if any. stdin is not read while it
    /// is set (backpressure); the socket is.
    pending: Option<PendingWrite>,
    /// The most recent thing that went wrong in the current outage (a
    /// connect error, a -32603 answer, an unanswered replay), for the
    /// exit-3 and exit-5 lines.
    last_error: String,
}

/// Run the shim against `socket_path` until one of the [`Exit`] shapes.
pub(crate) async fn run(socket_path: String, token: String) -> Exit {
    let mut pump = Pump {
        socket_path,
        token,
        stdin: BufReader::new(tokio::io::stdin()),
        stdin_acc: Vec::new(),
        stdin_eof: false,
        stdout: tokio::io::stdout(),
        init: None,
        outstanding: Vec::new(),
        held: None,
        reconnecting: false,
        pending: None,
        last_error: "no connect attempt made".to_string(),
    };

    let mut budget = ReconnectBudget::new(Instant::now(), INITIAL_CONNECT_BUDGET);
    let mut stream = match pump.connect_loop(&mut budget, true).await {
        Ok(s) => s,
        Err(ConnectEnd::StdinClosed) => {
            stderr_line("stdin closed before the first connection; exiting");
            return Exit::Clean;
        }
        Err(ConnectEnd::Exhausted) => {
            stderr_line(&format!(
                "connect {}: {}",
                pump.socket_path, pump.last_error
            ));
            return Exit::InitialConnectFailed;
        }
    };

    loop {
        let ended = pump.pump_connection(stream, &budget).await;
        match ended {
            Ended::Clean => return Exit::Clean,
            Ended::StdinClosed => {
                pump.fail_all().await;
                stderr_line("stdin closed while reconnecting; exiting");
                return Exit::Clean;
            }
            Ended::Rejected => {
                pump.fail_all().await;
                return Exit::InitializeRejected;
            }
            Ended::BudgetExhausted => return pump.budget_exhausted(&budget).await,
            Ended::Lost | Ended::InitializeRetry => {
                let failed = pump.outstanding.len();
                pump.fail_outstanding().await;
                if matches!(ended, Ended::InitializeRetry) {
                    pump.last_error = "initialize answered -32603".to_string();
                    stderr_line("initialize answered -32603; retrying under the outage budget");
                } else if !pump.reconnecting {
                    stderr_line(&format!(
                        "connection to kernel lost ({failed} unanswered requests failed); reconnecting"
                    ));
                }
                if !pump.reconnecting {
                    pump.reconnecting = true;
                    budget.reset(Instant::now(), pump.outage_budget());
                }
            }
        }
        stream = match pump.connect_loop(&mut budget, false).await {
            Ok(s) => s,
            Err(ConnectEnd::StdinClosed) => {
                pump.fail_all().await;
                stderr_line("stdin closed while reconnecting; exiting");
                return Exit::Clean;
            }
            Err(ConnectEnd::Exhausted) => return pump.budget_exhausted(&budget).await,
        };
    }
}

enum ConnectEnd {
    StdinClosed,
    /// The budget ran out; `Pump::last_error` says what failed last.
    Exhausted,
}

impl Pump {
    /// Connect with backoff under `budget`. `attempt_first` is true for
    /// the initial connection; after a hang-up the first delay comes
    /// first, and doubles as the window in which a stdin EOF that raced
    /// the hang-up (codex tearing down both ends) is observed.
    async fn connect_loop(
        &mut self,
        budget: &mut ReconnectBudget,
        attempt_first: bool,
    ) -> Result<UnixStream, ConnectEnd> {
        let mut poll_stdin = !self.stdin_eof;
        let mut try_now = attempt_first;
        loop {
            if try_now {
                budget.record_attempt();
                match UnixStream::connect(&self.socket_path).await {
                    Ok(stream) => return Ok(stream),
                    Err(e) => self.last_error = e.to_string(),
                }
            }
            try_now = true;
            let Some(delay) = budget.next_delay(Instant::now()) else {
                return Err(ConnectEnd::Exhausted);
            };
            let until = tokio::time::Instant::now() + delay;
            loop {
                tokio::select! {
                    _ = tokio::time::sleep_until(until) => break,
                    probe = stdin_probe(&mut self.stdin), if poll_stdin => match probe {
                        Probe::Eof => {
                            self.stdin_eof = true;
                            return Err(ConnectEnd::StdinClosed);
                        }
                        // Data stays in the BufReader (backpressure);
                        // stop probing until we are connected again.
                        Probe::Data => poll_stdin = false,
                    },
                }
            }
        }
    }

    /// Drive one connection until it ends. While `reconnecting`, the
    /// cached `initialize` is written first and nothing is forwarded
    /// until its response arrives (the kernel answers pre-handshake
    /// requests with -32002 and drops the connection). Whatever write
    /// was in flight when the connection ended is held or dropped by
    /// [`Pump::hold_pending`].
    async fn pump_connection(&mut self, stream: UnixStream, budget: &ReconnectBudget) -> Ended {
        let (rd, wr) = stream.into_split();
        let mut awaiting_handshake = false;
        if self.reconnecting {
            if let Some(init) = &self.init {
                self.pending = Some(PendingWrite::new(
                    WriteKind::Initialize,
                    init.frame.clone().into_bytes(),
                ));
                awaiting_handshake = true;
            } else {
                // Nothing cached (first frame was not an initialize):
                // resume forwarding directly.
                self.reconnected(budget);
            }
        }
        let ended = self
            .drive(BufReader::new(rd), wr, awaiting_handshake, budget)
            .await;
        self.hold_pending();
        ended
    }

    /// The connected `select!`. The socket read branch is always active;
    /// the write branch while a frame is pending; stdin while nothing is
    /// pending (a line read, or only an EOF probe while the handshake is
    /// pending, D2); the deadline while the handshake is pending.
    async fn drive(
        &mut self,
        mut sock: BufReader<OwnedReadHalf>,
        mut wr: OwnedWriteHalf,
        mut awaiting_handshake: bool,
        budget: &ReconnectBudget,
    ) -> Ended {
        let mut sock_acc = Vec::new();
        let deadline = tokio::time::Instant::from_std(budget.deadline());
        let mut poll_stdin = true;

        loop {
            if self.pending.is_none()
                && !awaiting_handshake
                && let Some((id, bytes)) = self.held.take()
            {
                self.pending = Some(PendingWrite::new(WriteKind::Held(id), bytes));
            }
            let stdin_active =
                !self.stdin_eof && self.pending.is_none() && (!awaiting_handshake || poll_stdin);
            let event = tokio::select! {
                line = next_line(&mut sock, &mut sock_acc) => Event::Socket(line),
                n = write_step(&mut wr, &self.pending), if self.pending.is_some() => Event::Written(n),
                ev = stdin_step(&mut self.stdin, &mut self.stdin_acc, awaiting_handshake),
                    if stdin_active => ev,
                _ = tokio::time::sleep_until(deadline), if awaiting_handshake => Event::Deadline,
            };
            match event {
                Event::Socket(Ok(Some(line))) => {
                    let frame = classify(&line);
                    if awaiting_handshake {
                        if let Frame::Response { id, error_code } = frame {
                            match self.on_replay_response(&line, &id, error_code).await {
                                Replay::Accepted => {
                                    awaiting_handshake = false;
                                    self.reconnected(budget);
                                }
                                Replay::Retry => return Ended::InitializeRetry,
                                Replay::Rejected => return Ended::Rejected,
                            }
                        } else {
                            // The kernel only writes responses; anything
                            // else is passed through untouched.
                            self.write_stdout(&line).await;
                        }
                    } else if let Some(ended) = self.on_socket_line(line, frame).await {
                        return ended;
                    }
                }
                Event::Socket(Ok(None)) | Event::Socket(Err(_)) => {
                    return if self.stdin_eof {
                        Ended::Clean
                    } else {
                        Ended::Lost
                    };
                }
                Event::Written(Ok(n)) if n > 0 => match self.advance_pending(n) {
                    Some(WriteKind::Request(id) | WriteKind::Held(id)) => self.outstanding.push(id),
                    Some(WriteKind::Initialize | WriteKind::Passthrough) | None => {}
                },
                // `Ok(0)` or an error: the peer is gone.
                Event::Written(_) => return Ended::Lost,
                Event::Stdin(Ok(Some(line))) => self.on_stdin_line(line),
                Event::Stdin(Ok(None)) | Event::Stdin(Err(_)) => {
                    // codex closed our stdin: half-close so the kernel
                    // reads EOF and drops the connection, then keep
                    // draining its side until it does.
                    self.stdin_eof = true;
                    let _ = wr.shutdown().await;
                }
                Event::Probe(Probe::Eof) => {
                    self.stdin_eof = true;
                    return Ended::StdinClosed;
                }
                Event::Probe(Probe::Data) => poll_stdin = false,
                Event::Deadline => {
                    self.last_error = "replayed initialize unanswered".to_string();
                    return Ended::BudgetExhausted;
                }
            }
        }
    }

    /// `n` more bytes of the pending frame are on the wire; returns the
    /// kind once the whole frame is.
    fn advance_pending(&mut self, n: usize) -> Option<WriteKind> {
        let p = self
            .pending
            .as_mut()
            .expect("a write completed, so a frame was pending");
        p.off += n;
        if p.off < p.bytes.len() {
            return None;
        }
        self.pending.take().map(|p| p.kind)
    }

    /// The connection is over. A request still pending has not had its
    /// trailing newline written, so the kernel's `read_line` never
    /// dispatched it: it is held for re-sending. Anything else pending
    /// is dropped.
    fn hold_pending(&mut self) {
        if let Some(p) = self.pending.take()
            && let WriteKind::Request(id) | WriteKind::Held(id) = p.kind
        {
            self.held = Some((id, p.bytes));
        }
    }

    /// The first response on a replayed connection is the handshake
    /// response (the kernel is serial and nothing else was forwarded).
    async fn on_replay_response(
        &mut self,
        line: &[u8],
        id: &serde_json::Value,
        error_code: Option<i64>,
    ) -> Replay {
        let init = self
            .init
            .as_mut()
            .expect("replay response only awaited with a cached initialize");
        if *id != init.id {
            stderr_line(&format!(
                "handshake response id {id} does not match replayed initialize id {}; treating it as the handshake response",
                init.id
            ));
        }
        match error_code {
            None => {
                if !init.acked {
                    // codex never saw a response to its initialize
                    // (the first connection died with it in flight).
                    init.acked = true;
                    self.write_stdout(line).await;
                }
                Replay::Accepted
            }
            Some(RPC_INTERNAL_ERROR) => Replay::Retry,
            Some(code) => {
                if !init.acked {
                    init.acked = true;
                    self.write_stdout(line).await;
                }
                stderr_line(&format!(
                    "replayed initialize rejected by kernel (code {code}); exiting"
                ));
                Replay::Rejected
            }
        }
    }

    /// A line from the kernel on an established connection.
    async fn on_socket_line(&mut self, line: Vec<u8>, frame: Frame) -> Option<Ended> {
        if let Frame::Response { id, error_code } = frame {
            if let Some(init) = self.init.as_mut()
                && !init.acked
                && init.id == id
            {
                // The original handshake response. An error here is the
                // same decision as for a replayed one: -32603 retries on
                // a fresh connection, anything else ends the shim.
                match error_code {
                    None => init.acked = true,
                    Some(RPC_INTERNAL_ERROR) => return Some(Ended::InitializeRetry),
                    Some(code) => {
                        init.acked = true;
                        self.write_stdout(&line).await;
                        stderr_line(&format!(
                            "initialize rejected by kernel (code {code}); exiting"
                        ));
                        return Some(Ended::Rejected);
                    }
                }
            } else if let Some(pos) = self.outstanding.iter().position(|o| *o == id) {
                self.outstanding.remove(pos);
            }
        }
        self.write_stdout(&line).await;
        None
    }

    /// A line from codex becomes the pending socket write: an
    /// `initialize` is token-injected and cached first, any other
    /// request is remembered by id once written.
    fn on_stdin_line(&mut self, line: Vec<u8>) {
        let (kind, bytes) = match classify(&line) {
            Frame::Request { id, method } if method == "initialize" => {
                let text = String::from_utf8(line)
                    .expect("classify() parsed this line as JSON, so it is UTF-8");
                let injected = frames::maybe_inject_token(&text, &self.token);
                self.init = Some(CachedInitialize {
                    frame: injected.clone(),
                    id,
                    acked: false,
                });
                (WriteKind::Initialize, injected.into_bytes())
            }
            Frame::Request { id, .. } => (WriteKind::Request(id), line),
            // Notifications are dropped by the kernel and anything
            // unclassifiable has no id to answer: neither is re-sent.
            Frame::Notification | Frame::Response { .. } | Frame::Other => {
                (WriteKind::Passthrough, line)
            }
        };
        self.pending = Some(PendingWrite::new(kind, bytes));
    }

    /// D5-b: every request the kernel owed an answer to gets the
    /// synthesized error now; the kernel may or may not have run it.
    async fn fail_outstanding(&mut self) {
        for id in std::mem::take(&mut self.outstanding) {
            self.write_stdout(lost_error_frame(&id).as_bytes()).await;
        }
    }

    /// Terminal exits: outstanding, then an unacknowledged initialize,
    /// then the held frame. Lines still unread in the stdin pipe get
    /// nothing; the process exit closes stdout and codex fails them as
    /// transport-closed itself.
    async fn fail_all(&mut self) {
        self.fail_outstanding().await;
        if let Some(init) = self.init.as_mut()
            && !init.acked
        {
            init.acked = true;
            let frame = lost_error_frame(&init.id);
            self.write_stdout(frame.as_bytes()).await;
        }
        if let Some((id, _)) = self.held.take() {
            self.write_stdout(lost_error_frame(&id).as_bytes()).await;
        }
    }

    /// stdout is the JSON-RPC wire to codex; a failed write means codex
    /// is gone, which stdin EOF reports separately.
    async fn write_stdout(&mut self, bytes: &[u8]) {
        if self.stdout.write_all(bytes).await.is_err() {
            return;
        }
        let _ = self.stdout.flush().await;
    }

    /// The outage is over: one stderr line, budget no longer running.
    fn reconnected(&mut self, budget: &ReconnectBudget) {
        self.reconnecting = false;
        stderr_line(&format!(
            "reconnected after {} attempts in {} ms",
            budget.attempts(),
            budget.elapsed(Instant::now()).as_millis()
        ));
    }

    /// The total for an outage starting now. Until codex has a response
    /// to its `initialize` it is inside its MCP startup timeout, which
    /// [`INITIAL_CONNECT_BUDGET`] is sized for; afterwards it is inside a
    /// `tools/call` timeout, which [`RECONNECT_BUDGET`] is sized for.
    fn outage_budget(&self) -> Duration {
        if self.init.as_ref().is_some_and(|init| init.acked) {
            RECONNECT_BUDGET
        } else {
            INITIAL_CONNECT_BUDGET
        }
    }

    /// Exit 5: fail everything owed, one stderr line with the last error.
    async fn budget_exhausted(&mut self, budget: &ReconnectBudget) -> Exit {
        self.fail_all().await;
        stderr_line(&format!(
            "reconnect budget exhausted after {} attempts in {} ms; last error: {}",
            budget.attempts(),
            budget.elapsed(Instant::now()).as_millis(),
            self.last_error
        ));
        Exit::BudgetExhausted
    }
}

enum Event {
    Socket(io::Result<Option<Vec<u8>>>),
    Written(io::Result<usize>),
    Stdin(io::Result<Option<Vec<u8>>>),
    Probe(Probe),
    Deadline,
}

/// The stdin side of the connected `select!`: a probe while a replayed
/// handshake is pending, a line read otherwise.
async fn stdin_step(stdin: &mut BufReader<Stdin>, acc: &mut Vec<u8>, probe_only: bool) -> Event {
    if probe_only {
        Event::Probe(stdin_probe(stdin).await)
    } else {
        Event::Stdin(next_line(stdin, acc).await)
    }
}

/// Read one `\n`-terminated line (trailer kept) into `acc`, returning
/// it complete, or `None` at EOF. Each step is one cancellation-safe
/// `fill_buf`; consumed bytes move into `acc` before the next await, so
/// dropping this future inside `select!` loses nothing. An unterminated
/// last line at EOF is dropped; codex terminates every frame.
async fn next_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    acc: &mut Vec<u8>,
) -> io::Result<Option<Vec<u8>>> {
    loop {
        let (complete, consumed) = {
            let buf = reader.fill_buf().await?;
            if buf.is_empty() {
                return Ok(None);
            }
            match buf.iter().position(|&b| b == b'\n') {
                Some(i) => {
                    acc.extend_from_slice(&buf[..=i]);
                    (true, i + 1)
                }
                None => {
                    acc.extend_from_slice(buf);
                    (false, buf.len())
                }
            }
        };
        reader.consume(consumed);
        if complete {
            return Ok(Some(std::mem::take(acc)));
        }
    }
}

/// Look at stdin without consuming: an empty `fill_buf` is EOF.
async fn stdin_probe(stdin: &mut BufReader<Stdin>) -> Probe {
    match stdin.fill_buf().await {
        Ok([]) | Err(_) => Probe::Eof,
        Ok(_) => Probe::Data,
    }
}

/// One `write` of what is left of the pending frame. tokio's `write` is
/// cancel-safe: when another `select!` branch wins first, nothing was
/// written. No `flush`: on a `UnixStream` it is a no-op.
async fn write_step(wr: &mut OwnedWriteHalf, pending: &Option<PendingWrite>) -> io::Result<usize> {
    let p = pending
        .as_ref()
        .expect("the write branch is only enabled with a frame pending");
    wr.write(&p.bytes[p.off..]).await
}

/// One line per state change; codex copies our stderr into its log.
fn stderr_line(msg: &str) {
    let _ = writeln!(io::stderr(), "neige-mcp-stdio-shim: {msg}");
}
