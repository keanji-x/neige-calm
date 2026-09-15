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
//! [`frames::lost_error_frame`]); a frame whose socket write failed was
//! never queued and is re-sent after the handshake. The kernel side of
//! this contract is `calm-server/src/mcp_server/transport.rs`
//! (`handle_connection`): one identity per connection, bound by
//! `initialize`; serial request/response; notifications dropped; the
//! kernel never sends requests.

use std::io::{self, Write};
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader, Stdin, Stdout};
use tokio::net::UnixStream;
use tokio::net::unix::OwnedWriteHalf;

use crate::frames::{self, Frame, RPC_INTERNAL_ERROR, classify, lost_error_frame};

/// First delay between reconnect attempts; doubles each time.
pub(crate) const RECONNECT_BACKOFF_INITIAL: Duration = Duration::from_millis(100);
/// Ceiling for the doubling, so a kernel that is back is noticed
/// within 5 s.
pub(crate) const RECONNECT_BACKOFF_CAP: Duration = Duration::from_secs(5);
/// Total time one outage may take before the shim gives up (exit 5).
/// Well below codex's default 120 s `tools/call` timeout, so the shim
/// fails a call before codex abandons it and a kernel that comes back
/// later does not run it.
pub(crate) const RECONNECT_BUDGET: Duration = Duration::from_secs(30);
/// Total time the first connection may take (exit 3). Below codex's
/// default 30 s MCP startup timeout, so codex sees our exit rather than
/// its own timeout.
pub(crate) const INITIAL_CONNECT_BUDGET: Duration = Duration::from_secs(15);

/// Deadline + backoff for one outage. Pure: every method takes `now`.
#[derive(Debug)]
pub(crate) struct ReconnectBudget {
    total: Duration,
    start: Instant,
    backoff: Duration,
    attempts: u32,
}

impl ReconnectBudget {
    pub(crate) fn new(now: Instant, total: Duration) -> Self {
        Self {
            total,
            start: now,
            backoff: RECONNECT_BACKOFF_INITIAL,
            attempts: 0,
        }
    }

    /// Start a fresh outage: deadline, backoff and attempt count all reset.
    pub(crate) fn reset(&mut self, now: Instant) {
        *self = Self::new(now, self.total);
    }

    /// The delay to sleep before the next attempt, or `None` once the
    /// deadline has passed. Never sleeps past the deadline.
    pub(crate) fn next_delay(&mut self, now: Instant) -> Option<Duration> {
        let elapsed = now.saturating_duration_since(self.start);
        if elapsed >= self.total {
            return None;
        }
        let delay = self.backoff.min(self.total - elapsed);
        self.backoff = (self.backoff * 2).min(RECONNECT_BACKOFF_CAP);
        Some(delay)
    }

    pub(crate) fn record_attempt(&mut self) {
        self.attempts += 1;
    }

    pub(crate) fn attempts(&self) -> u32 {
        self.attempts
    }

    pub(crate) fn elapsed(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.start)
    }
}

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
    /// A handshake response with a non-retryable error.
    Rejected,
    /// stdin closed while waiting for a replayed handshake response.
    StdinClosed,
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
    /// A request line whose socket write failed, re-sent after the next
    /// handshake. A write to a dead peer fails before the first byte,
    /// and a frame cut short by the failure has no newline, so the
    /// kernel's `read_line` framer never dispatches it either way.
    held: Option<(serde_json::Value, Vec<u8>)>,
    /// Set from the moment a connection is lost until a handshake on a
    /// new one is accepted; the -32603 retry path keeps it set so one
    /// budget covers the whole outage.
    reconnecting: bool,
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
    };

    let mut budget = ReconnectBudget::new(Instant::now(), INITIAL_CONNECT_BUDGET);
    let mut stream = match pump.connect_loop(&mut budget, true).await {
        Ok(s) => s,
        Err(ConnectEnd::StdinClosed) => return Exit::Clean,
        Err(ConnectEnd::Exhausted(err)) => {
            stderr_line(&format!("connect {}: {err}", pump.socket_path));
            return Exit::InitialConnectFailed;
        }
    };
    let mut budget = ReconnectBudget::new(Instant::now(), RECONNECT_BUDGET);

    loop {
        match pump.pump_connection(stream, &budget).await {
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
            Ended::Lost => {
                if !pump.reconnecting {
                    pump.reconnecting = true;
                    budget.reset(Instant::now());
                    let failed = pump.outstanding.len();
                    pump.fail_outstanding().await;
                    stderr_line(&format!(
                        "connection to kernel lost ({failed} unanswered requests failed); reconnecting"
                    ));
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
            Err(ConnectEnd::Exhausted(err)) => {
                pump.fail_all().await;
                stderr_line(&format!(
                    "reconnect budget exhausted after {} attempts in {} ms; last error: {err}",
                    budget.attempts(),
                    budget.elapsed(Instant::now()).as_millis()
                ));
                return Exit::BudgetExhausted;
            }
        };
    }
}

enum ConnectEnd {
    StdinClosed,
    Exhausted(io::Error),
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
        let mut last_error = io::Error::other("no connect attempt made");
        loop {
            if try_now {
                budget.record_attempt();
                match UnixStream::connect(&self.socket_path).await {
                    Ok(stream) => return Ok(stream),
                    Err(e) => last_error = e,
                }
            }
            try_now = true;
            let Some(delay) = budget.next_delay(Instant::now()) else {
                return Err(ConnectEnd::Exhausted(last_error));
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
    /// requests with -32002 and drops the connection).
    async fn pump_connection(&mut self, stream: UnixStream, budget: &ReconnectBudget) -> Ended {
        let (rd, mut wr) = stream.into_split();
        let mut sock = BufReader::new(rd);
        let mut sock_acc = Vec::new();

        let mut awaiting_handshake = false;
        if self.reconnecting {
            if let Some(init) = &self.init {
                if write_frame(&mut wr, init.frame.as_bytes()).await.is_err() {
                    return Ended::Lost;
                }
                awaiting_handshake = true;
            } else {
                // Nothing cached (first frame was not an initialize):
                // resume forwarding directly.
                self.reconnected(budget);
                if let Some(ended) = self.send_held(&mut wr).await {
                    return ended;
                }
            }
        }
        let mut poll_stdin = true;

        loop {
            // While the handshake is pending stdin is only probed for
            // EOF (D2); otherwise it is read line by line.
            let stdin_active = !self.stdin_eof && (!awaiting_handshake || poll_stdin);
            let event = tokio::select! {
                line = next_line(&mut sock, &mut sock_acc) => Event::Socket(line),
                ev = stdin_step(&mut self.stdin, &mut self.stdin_acc, awaiting_handshake),
                    if stdin_active => ev,
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
                                    if let Some(ended) = self.send_held(&mut wr).await {
                                        return ended;
                                    }
                                }
                                Replay::Retry => return Ended::Lost,
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
                Event::Stdin(Ok(Some(line))) => {
                    if let Some(ended) = self.on_stdin_line(line, &mut wr).await {
                        return ended;
                    }
                }
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
            }
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
                    Some(RPC_INTERNAL_ERROR) => return Some(Ended::Lost),
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

    /// A line from codex: inject + cache an `initialize`, remember the
    /// id of any other request, write it through.
    async fn on_stdin_line(&mut self, line: Vec<u8>, wr: &mut OwnedWriteHalf) -> Option<Ended> {
        match classify(&line) {
            Frame::Request { id, method } if method == "initialize" => {
                let text = match String::from_utf8(line) {
                    Ok(text) => text,
                    Err(e) => {
                        return write_frame(wr, e.as_bytes())
                            .await
                            .is_err()
                            .then_some(Ended::Lost);
                    }
                };
                let injected = frames::maybe_inject_token(&text, &self.token);
                self.init = Some(CachedInitialize {
                    frame: injected.clone(),
                    id,
                    acked: false,
                });
                // On failure the cached frame is what the reconnect
                // path re-sends, so nothing is held separately.
                write_frame(wr, injected.as_bytes())
                    .await
                    .is_err()
                    .then_some(Ended::Lost)
            }
            Frame::Request { id, .. } => {
                if write_frame(wr, &line).await.is_err() {
                    self.held = Some((id, line));
                    return Some(Ended::Lost);
                }
                self.outstanding.push(id);
                None
            }
            // Notifications are dropped by the kernel and anything
            // unclassifiable has no id to answer: neither is re-sent.
            Frame::Notification | Frame::Response { .. } | Frame::Other => {
                write_frame(wr, &line).await.is_err().then_some(Ended::Lost)
            }
        }
    }

    /// Re-send the held request after a handshake, if there is one.
    async fn send_held(&mut self, wr: &mut OwnedWriteHalf) -> Option<Ended> {
        let (id, line) = self.held.take()?;
        if write_frame(wr, &line).await.is_err() {
            self.held = Some((id, line));
            return Some(Ended::Lost);
        }
        self.outstanding.push(id);
        None
    }

    /// D5-b: every request the kernel owed an answer to gets the
    /// synthesized error now; the kernel may or may not have run it.
    async fn fail_outstanding(&mut self) {
        for id in std::mem::take(&mut self.outstanding) {
            self.write_stdout(lost_error_frame(&id).as_bytes()).await;
        }
    }

    /// Terminal exits: outstanding, then an unacknowledged initialize,
    /// then the held frame — nothing codex is waiting on is left silent.
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
}

enum Event {
    Socket(io::Result<Option<Vec<u8>>>),
    Stdin(io::Result<Option<Vec<u8>>>),
    Probe(Probe),
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
/// dropping this future inside `select!` loses nothing.
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

async fn write_frame(wr: &mut OwnedWriteHalf, bytes: &[u8]) -> io::Result<()> {
    wr.write_all(bytes).await?;
    wr.flush().await
}

/// One line per state change; codex copies our stderr into its log.
fn stderr_line(msg: &str) {
    let _ = writeln!(io::stderr(), "neige-mcp-stdio-shim: {msg}");
}

#[cfg(test)]
mod budget_tests {
    //! #1699 D3 — the pure budget/backoff type.

    use std::time::{Duration, Instant};

    use super::{RECONNECT_BACKOFF_CAP, RECONNECT_BUDGET, ReconnectBudget};

    #[test]
    fn backoff_doubles_from_100ms_to_the_5s_cap() {
        let t0 = Instant::now();
        let mut budget = ReconnectBudget::new(t0, RECONNECT_BUDGET);
        let delays: Vec<u64> = (0..8)
            .map(|_| budget.next_delay(t0).expect("inside budget").as_millis() as u64)
            .collect();
        assert_eq!(delays, [100, 200, 400, 800, 1600, 3200, 5000, 5000]);
        assert_eq!(RECONNECT_BACKOFF_CAP, Duration::from_secs(5));
    }

    #[test]
    fn gives_up_once_the_deadline_is_reached() {
        let t0 = Instant::now();
        let mut budget = ReconnectBudget::new(t0, RECONNECT_BUDGET);
        assert!(budget.next_delay(t0 + Duration::from_secs(29)).is_some());
        assert_eq!(budget.next_delay(t0 + RECONNECT_BUDGET), None);
        assert_eq!(budget.next_delay(t0 + Duration::from_secs(40)), None);
    }

    #[test]
    fn delay_is_clipped_to_the_time_left() {
        let t0 = Instant::now();
        let mut budget = ReconnectBudget::new(t0, RECONNECT_BUDGET);
        let near_end = t0 + RECONNECT_BUDGET - Duration::from_millis(30);
        assert_eq!(budget.next_delay(near_end), Some(Duration::from_millis(30)));
    }

    #[test]
    fn reset_restarts_deadline_backoff_and_attempts() {
        let t0 = Instant::now();
        let mut budget = ReconnectBudget::new(t0, RECONNECT_BUDGET);
        for _ in 0..4 {
            budget.record_attempt();
            budget.next_delay(t0);
        }
        assert_eq!(budget.attempts(), 4);
        let t1 = t0 + Duration::from_secs(25);
        budget.reset(t1);
        assert_eq!(budget.attempts(), 0);
        assert_eq!(budget.elapsed(t1), Duration::ZERO);
        assert_eq!(budget.next_delay(t1), Some(Duration::from_millis(100)));
        // The deadline moved with the reset: 29 s after t1 is still inside.
        assert!(budget.next_delay(t1 + Duration::from_secs(29)).is_some());
        assert_eq!(budget.next_delay(t1 + RECONNECT_BUDGET), None);
    }
}
