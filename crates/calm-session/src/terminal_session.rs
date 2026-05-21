//! Pure, IO-free terminal-mode protocol state machine.
//!
//! This module is the testable core of the per-client protocol that lives in
//! `src/bin/daemon.rs`. The daemon binary is the IO shell: it owns the PTY,
//! the Unix socket, the tokio runtime, and the broadcast channels. Each
//! [`ClientMsg`] it reads off a socket — and each PTY chunk / child-exit
//! event it observes — is fed into one of the types here, which decide what
//! to do and emit a list of [`Effect`]s for the shell to enact.
//!
//! Keeping the protocol layer free of tokio / sockets / fds lets us assert
//! on its transitions in plain unit tests instead of by forking the real
//! binary and polling timing-sensitive sockets.
//!
//! ## Layering
//!
//! - [`TerminalSessionState`] — one instance per attached client connection.
//!   Owns whether the first frame has been seen, decides what to emit on
//!   each subsequent client frame, and reads (but never writes) the shared
//!   [`ByteRing`] when generating the Hello replay.
//! - [`PtyBroadcaster`] — single global instance per daemon. Owns the
//!   [`ByteRing`] and produces the broadcast effects for raw PTY chunks
//!   and for `ChildExited`.
//! - [`ByteRing`] — chunk-granular ring of recent PTY output, sized in
//!   bytes. Drops whole chunks (never splits an escape sequence) from the
//!   front when over budget.
//!
//! ## Non-goals
//!
//! - No tokio types. No OS resources. No `Arc`/`Mutex`.
//! - No wire protocol changes. The shell layer is expected to produce the
//!   exact same bytes on the socket as the pre-refactor daemon did.

use std::collections::VecDeque;

use crate::{ClientMsg, DaemonMsg};

/// Side-effects emitted by the protocol layer for the IO shell to enact.
///
/// All variants are passive descriptions — the state machine never performs
/// IO itself. The shell receives a `Vec<Effect>` per pumped event and
/// translates each variant into a socket write, channel send, or syscall.
#[derive(Debug, PartialEq, Eq)]
pub enum Effect {
    /// Send a single [`DaemonMsg`] to the client whose frame produced this
    /// effect. Used for the initial `Hello { replay }` after Attach.
    SendToClient(DaemonMsg),
    /// Broadcast a [`DaemonMsg`] to every attached client. Used for live
    /// `Stdout` chunks and for the terminal `ChildExited` frame.
    Broadcast(DaemonMsg),
    /// Resize the PTY master. The shell is free to ignore cols/rows == 0
    /// (the existing `apply_resize` keeps that guard).
    ResizePty { cols: u16, rows: u16 },
    /// Write bytes to the PTY stdin.
    WriteToPty(Vec<u8>),
    /// Tear down the child process (SIGHUP the pgid, then SIGKILL fallback;
    /// the shell still owns that policy).
    KillChild,
    /// The client violated the protocol — typically by sending a non-Attach
    /// frame as the first message. The shell must drop the connection.
    ProtocolViolation(&'static str),
}

/// Chunk-granular byte ring used to seed a fresh client's Hello replay.
///
/// Each `append` pushes one whole chunk (typically one PTY read). When the
/// total goes over `max_bytes` we drop chunks from the front, never
/// splitting one — that way the replay always starts on a chunk boundary
/// and we never slice through a multi-byte escape sequence.
///
/// Migrated verbatim from the old `daemon.rs::ByteBuffer`. Behaviour is
/// preserved bit-for-bit: same drain policy, same `snapshot` semantics.
pub struct ByteRing {
    chunks: VecDeque<Vec<u8>>,
    total_bytes: usize,
    max_bytes: usize,
}

impl ByteRing {
    pub fn new(max_bytes: usize) -> Self {
        Self {
            chunks: VecDeque::new(),
            total_bytes: 0,
            max_bytes,
        }
    }

    /// Push one chunk. If the buffer is now over budget, evict whole chunks
    /// from the front until either we fit, or only one chunk remains.
    pub fn append(&mut self, bytes: Vec<u8>) {
        self.total_bytes += bytes.len();
        self.chunks.push_back(bytes);
        while self.total_bytes > self.max_bytes && self.chunks.len() > 1 {
            let dropped = self.chunks.pop_front().unwrap();
            self.total_bytes -= dropped.len();
        }
    }

    /// Concatenated copy of every chunk currently buffered. Only called on
    /// the attach path so a per-call `Vec` clone is fine.
    pub fn snapshot(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.total_bytes);
        for c in &self.chunks {
            out.extend_from_slice(c);
        }
        out
    }

    /// Sum of every buffered chunk's length. Mostly for tests / metrics.
    pub fn total_bytes(&self) -> usize {
        self.total_bytes
    }
}

/// Single-client protocol state machine. One instance per accepted socket.
///
/// Tracks whether the first `Attach` frame has been observed. Every
/// subsequent [`ClientMsg`] is run through [`on_client_frame`] which
/// returns the side-effects the shell needs to enact.
///
/// [`on_client_frame`]: TerminalSessionState::on_client_frame
pub struct TerminalSessionState {
    attached: bool,
}

impl TerminalSessionState {
    pub fn new() -> Self {
        Self { attached: false }
    }

    /// True once we have seen the initial `Attach` frame.
    pub fn is_attached(&self) -> bool {
        self.attached
    }

    /// Translate one incoming client frame into a list of side-effects.
    ///
    /// The first frame on a connection MUST be [`ClientMsg::Attach`]; anything
    /// else yields a single [`Effect::ProtocolViolation`] and the shell must
    /// close the connection (matching the pre-refactor `anyhow::bail!` path
    /// in `handle_client`).
    ///
    /// After attach:
    /// - `Stdin(b)`   → `WriteToPty(b)`
    /// - `Resize`     → `ResizePty`
    /// - `Kill`       → `KillChild`
    /// - re-`Attach`  → no-op (matches "ignore re-attach on live connection")
    /// - chat-mode frames in terminal mode → no-op (silently ignored, just
    ///   like the original `tracing::debug!` arm).
    ///
    /// The shared [`ByteRing`] is read by reference only — the state machine
    /// never mutates it. The shell is responsible for keeping the ring
    /// populated via [`PtyBroadcaster`].
    pub fn on_client_frame(&mut self, msg: ClientMsg, buffer: &ByteRing) -> Vec<Effect> {
        if !self.attached {
            return match msg {
                ClientMsg::Attach { cols, rows } => {
                    self.attached = true;
                    let replay = buffer.snapshot();
                    vec![
                        Effect::ResizePty { cols, rows },
                        Effect::SendToClient(DaemonMsg::Hello { replay }),
                    ]
                }
                _ => vec![Effect::ProtocolViolation(
                    "expected Attach as first message",
                )],
            };
        }

        match msg {
            ClientMsg::Stdin(b) => vec![Effect::WriteToPty(b)],
            ClientMsg::Resize { cols, rows } => vec![Effect::ResizePty { cols, rows }],
            ClientMsg::Kill => vec![Effect::KillChild],
            // Re-attach on a live connection is intentionally a no-op
            // (matches the pre-refactor behaviour at daemon.rs:728).
            ClientMsg::Attach { .. } => vec![],
            // Chat-mode frames received in terminal mode are silently
            // dropped (matches daemon.rs:735-741).
            ClientMsg::ChatUserMessage { .. }
            | ClientMsg::ChatStop
            | ClientMsg::AnswerQuestion { .. } => vec![],
        }
    }
}

impl Default for TerminalSessionState {
    fn default() -> Self {
        Self::new()
    }
}

/// PTY-byte plane: owns the [`ByteRing`] and produces the broadcast
/// effects for raw PTY chunks and for the child's exit code.
///
/// One instance per daemon, shared between the PTY-reader thread and the
/// child-waiter task in the shell layer.
pub struct PtyBroadcaster {
    buffer: ByteRing,
}

impl PtyBroadcaster {
    pub fn new(max_bytes: usize) -> Self {
        Self {
            buffer: ByteRing::new(max_bytes),
        }
    }

    /// One PTY chunk arrived. Append to the replay ring and emit a
    /// `Broadcast(Stdout(bytes))` for every attached client.
    pub fn on_pty_chunk(&mut self, bytes: Vec<u8>) -> Vec<Effect> {
        self.buffer.append(bytes.clone());
        vec![Effect::Broadcast(DaemonMsg::Stdout(bytes))]
    }

    /// Child exited with the given exit code. Emit a `ChildExited` to every
    /// client; the shell then begins its 200ms grace period before tearing
    /// down the socket.
    pub fn on_child_exit(&mut self, code: Option<i32>) -> Vec<Effect> {
        vec![Effect::Broadcast(DaemonMsg::ChildExited { code })]
    }

    /// Read-only handle on the ring. The shell hands this to
    /// [`TerminalSessionState::on_client_frame`] when serving an Attach so
    /// the state machine can snapshot it into `Hello { replay }`.
    pub fn buffer(&self) -> &ByteRing {
        &self.buffer
    }
}
