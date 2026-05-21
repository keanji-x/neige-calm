//! Pure, IO-free terminal-mode protocol state machine (v2).
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
//!   Owns whether the [`ClientMsg::ClientHello`] handshake has completed,
//!   tracks the client's role / resize epoch, and decides what to emit on
//!   each subsequent frame.
//! - [`OwnerRegistry`] — single instance per daemon. Tracks the current
//!   owner across all connected clients. Concurrent-safe access is the
//!   shell's responsibility (typically a `Mutex` wrapper).
//! - [`RenderPlane`] (PR-2) — single global instance per terminal-mode
//!   daemon. Owns the [`TerminalModel`] (VT-driven grid + scrollback) and
//!   the [`ByteRing`] transcript. Produces `Broadcast(RenderPatch)` on
//!   each PTY chunk, `Broadcast(RenderSnapshot)` on resize, and
//!   `Broadcast(TerminalExited)` on child exit. Maintains `pty_seq`
//!   independently of `render_rev` (the latter comes from the model).
//! - [`PtyBroadcaster`] — legacy single global instance per daemon.
//!   Replaced by [`RenderPlane`] for terminal-mode daemons in PR-2.
//!   Retained as the test fixture for the protocol state machine
//!   (`tests/v2_protocol.rs`); chat mode also still uses a parallel
//!   buffer (`EventBuffer` in `bin/daemon.rs`).
//! - [`ByteRing`] — chunk-granular ring of recent PTY output, sized in
//!   bytes. Drops whole chunks (never splits an escape sequence) from the
//!   front when over budget.
//!
//! ## Non-goals
//!
//! - No tokio types. No OS resources. No `Arc`/`Mutex`.
//! - `RenderPatch.data` remains raw PTY bytes (`encoding = Vt`) so
//!   xterm.js can drive its own grid; cell-grid diff encoding is a
//!   follow-up.

use std::collections::VecDeque;

use uuid::Uuid;

use crate::terminal_model::{ScrollbackLimit, TerminalModel};
use crate::{
    ClientMsg, DaemonMsg, PROTOCOL_VERSION, ProtocolErrorCode, PtySize, RenderEncoding,
    RenderPatch, RenderSnapshot, Role,
};

/// Side-effects emitted by the protocol layer for the IO shell to enact.
///
/// All variants are passive descriptions — the state machine never performs
/// IO itself. The shell receives a `Vec<Effect>` per pumped event and
/// translates each variant into a socket write, channel send, or syscall.
#[derive(Debug, PartialEq, Eq)]
pub enum Effect {
    /// Send a single [`DaemonMsg`] to the client whose frame produced this
    /// effect. Used for [`DaemonMsg::ServerHello`] after a successful
    /// handshake.
    SendToClient(DaemonMsg),
    /// Broadcast a [`DaemonMsg`] to every attached client. Used for live
    /// `RenderPatch` / `ResizeApplied` / `TerminalExited` / `OwnerChanged`.
    Broadcast(DaemonMsg),
    /// Resize the PTY master. The shell is free to ignore cols/rows == 0
    /// (the existing `apply_resize` keeps that guard).
    ResizePty { cols: u16, rows: u16 },
    /// Write bytes to the PTY stdin.
    WriteToPty(Vec<u8>),
    /// Tear down the child process (SIGHUP the pgid, then SIGKILL fallback;
    /// the shell still owns that policy).
    KillChild,
    /// Send a typed v2 protocol error to the client. Used in place of the
    /// legacy [`Self::ProtocolViolation`] whenever the state machine wants
    /// to deliver the error as a [`DaemonMsg::ProtocolError`] frame on the
    /// wire before closing.
    SendProtocolError {
        code: ProtocolErrorCode,
        message: String,
        expected_version: Option<u16>,
    },
    /// Drop the client connection after any preceding `SendProtocolError`
    /// has been flushed. Distinct from a generic "violation" so the shell
    /// can choose to send a graceful close frame first.
    CloseConnection,
    /// Daemon-level owner registry transition produced as a side-effect of
    /// a successful `OwnerClaim` / `OwnerRelease`. The shell broadcasts
    /// [`DaemonMsg::OwnerChanged`] derived from this; we emit both
    /// `AssignOwner` (registry update intent — purely a marker for
    /// observability and future hooks) and `BroadcastOwnerChanged`
    /// (broadcast intent) so the shell does not have to reach into the
    /// registry to figure out who's owner now.
    AssignOwner(Option<Uuid>),
    /// Tell the shell to broadcast a [`DaemonMsg::OwnerChanged`] with the
    /// current owner (or `None` after a release).
    BroadcastOwnerChanged(Option<Uuid>),
    /// Legacy: the client violated the protocol — typically by sending a
    /// non-`ClientHello` frame as the first message. Kept for the
    /// pre-existing tests but new v2 paths emit
    /// [`Self::SendProtocolError`] + [`Self::CloseConnection`] instead.
    ProtocolViolation(&'static str),
}

/// Chunk-granular byte ring used to seed a fresh client's render snapshot.
///
/// Each `append` pushes one whole chunk (typically one PTY read). When the
/// total goes over `max_bytes` we drop chunks from the front, never
/// splitting one — that way the replay always starts on a chunk boundary
/// and we never slice through a multi-byte escape sequence.
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

/// Daemon-level owner tracking. Single instance per daemon process.
///
/// All clients start as `Observer` unless they're the very first one to
/// attach with no current owner — in that case the first attach is
/// implicitly promoted to `Owner`. `OwnerClaim` is a hostile takeover and
/// always succeeds (transfers ownership unconditionally to the claimant).
pub struct OwnerRegistry {
    owner: Option<Uuid>,
}

impl OwnerRegistry {
    pub fn new() -> Self {
        Self { owner: None }
    }

    /// Called when a new client completes its handshake. Returns the role
    /// the client should be assigned. Honors `role_hint = Some(Owner)`
    /// only when no current owner exists; otherwise the request implicitly
    /// degrades to Observer (the client can still claim via
    /// `ClientMsg::OwnerClaim`).
    pub fn on_attach(&mut self, client_id: Uuid, role_hint: Option<Role>) -> Role {
        if self.owner.is_none() {
            // First attach (or first after a release): become Owner unless
            // the client explicitly asked to stay an Observer.
            let want_observer = matches!(role_hint, Some(Role::Observer));
            if want_observer {
                Role::Observer
            } else {
                self.owner = Some(client_id);
                Role::Owner
            }
        } else {
            // Already an owner; new client is an observer.
            Role::Observer
        }
    }

    /// Observer (or anyone) sent `OwnerClaim`. Always transfers ownership.
    /// Returns `true` if ownership actually changed (used by the shell to
    /// decide whether to broadcast `OwnerChanged`).
    pub fn on_claim(&mut self, client_id: Uuid) -> bool {
        let changed = self.owner != Some(client_id);
        self.owner = Some(client_id);
        changed
    }

    /// Owner sent `OwnerRelease` (or disconnected). Returns `true` if it
    /// actually held ownership.
    pub fn on_release(&mut self, client_id: Uuid) -> bool {
        if self.owner == Some(client_id) {
            self.owner = None;
            true
        } else {
            false
        }
    }

    pub fn current_owner(&self) -> Option<Uuid> {
        self.owner
    }
}

impl Default for OwnerRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Context the shell threads through `on_client_frame`. Keeping this in one
/// struct (rather than a long function-argument list) means new daemon
/// metadata doesn't bloat every call site.
#[derive(Debug, Clone)]
pub struct SessionContext<'a> {
    /// Terminal id the daemon was launched for. Used for the
    /// `ClientHello.terminal_id` mismatch check.
    pub terminal_id: &'a str,
    /// UUID that rolls on every daemon respawn. Sent back in
    /// `ServerHello.session_id` so a client knows whether the underlying
    /// PTY is the same as its last attach or a fresh one.
    pub session_id: Uuid,
    /// Current PTY viewport. The state machine doesn't mutate the master;
    /// it only reports what's there back to the client.
    pub pty_size: PtySize,
    /// PTY byte sequence head (oldest still in history).
    pub pty_seq_head: u32,
    /// PTY byte sequence tail (most recent). The shell increments this as
    /// chunks land via `PtyBroadcaster::on_pty_chunk`.
    pub pty_seq_tail: u32,
    /// Current render revision (mirrors `pty_seq_tail` in this PR).
    pub render_rev: u32,
}

/// Single-client protocol state machine. One instance per accepted socket.
///
/// Tracks whether the [`ClientMsg::ClientHello`] handshake has completed,
/// the role assigned by the [`OwnerRegistry`], and the latest committed
/// resize epoch. Every [`ClientMsg`] is run through [`on_client_frame`]
/// which returns the side-effects the shell needs to enact.
///
/// [`on_client_frame`]: TerminalSessionState::on_client_frame
pub struct TerminalSessionState {
    /// True once a valid `ClientHello` has been processed.
    attached: bool,
    /// Client UUID from the successful `ClientHello`. `None` pre-handshake.
    client_id: Option<Uuid>,
    /// Role assigned by the `OwnerRegistry` at handshake time, mutated on
    /// successful `OwnerClaim` / `OwnerRelease` from this connection.
    role: Option<Role>,
    /// Latest accepted resize epoch from the owner. Frames with `epoch <=
    /// resize_epoch` are stale and silently dropped.
    resize_epoch: u32,
    /// Last `render_rev` the client acknowledged. Plumbed in but not used
    /// for back-pressure decisions in this PR (a follow-up wires it into
    /// `Backpressure` emission).
    last_render_acked_rev: Option<u32>,
}

impl TerminalSessionState {
    pub fn new() -> Self {
        Self {
            attached: false,
            client_id: None,
            role: None,
            resize_epoch: 0,
            last_render_acked_rev: None,
        }
    }

    /// True once we have seen the initial `ClientHello`.
    pub fn is_attached(&self) -> bool {
        self.attached
    }

    pub fn role(&self) -> Option<Role> {
        self.role
    }

    pub fn client_id(&self) -> Option<Uuid> {
        self.client_id
    }

    pub fn resize_epoch(&self) -> u32 {
        self.resize_epoch
    }

    pub fn last_render_acked_rev(&self) -> Option<u32> {
        self.last_render_acked_rev
    }

    /// Translate one incoming client frame into a list of side-effects.
    ///
    /// The first frame on a connection MUST be [`ClientMsg::ClientHello`];
    /// anything else yields a typed
    /// [`Effect::SendProtocolError`]+[`Effect::CloseConnection`] pair and
    /// the shell must close the socket.
    ///
    /// Handshake checks (in order):
    /// 1. `protocol_version == PROTOCOL_VERSION` else `UnsupportedVersion`.
    /// 2. `terminal_id == ctx.terminal_id` else `BadHandshake`.
    /// 3. `capabilities.render_encodings` contains `Vt` else
    ///    `UnsupportedEncoding`.
    ///
    /// On success: register the client in `registry`, capture the
    /// returned `Role`, and emit `ResizePty` (so the daemon picks up the
    /// client's `desired_size`) + `ServerHello` with the current snapshot.
    ///
    /// Post-handshake routing:
    /// - `Input(b)` → owner: `WriteToPty(b)`; observer: `NotOwner` error.
    /// - `ResizeCommit{epoch,..}` → owner: bump epoch if `>` current,
    ///   emit `ResizePty` + broadcast `ResizeApplied`; stale epoch is a
    ///   silent no-op. Observer → `NotOwner`.
    /// - `OwnerClaim` → registry takeover; on actual change emit
    ///   `AssignOwner` + `BroadcastOwnerChanged` and bump this state's
    ///   role to `Owner`.
    /// - `OwnerRelease` → registry clear; mirror role to Observer.
    /// - `RenderAck` → update `last_render_acked_rev` (no other effect).
    /// - `Kill` → owner: `KillChild`; observer: `NotOwner`.
    /// - chat-mode frames → silent no-op (terminal mode ignores them).
    pub fn on_client_frame(
        &mut self,
        msg: ClientMsg,
        buffer: &ByteRing,
        registry: &mut OwnerRegistry,
        ctx: &SessionContext<'_>,
    ) -> Vec<Effect> {
        if !self.attached {
            return self.process_hello(msg, buffer, registry, ctx);
        }

        match msg {
            ClientMsg::Input(b) => {
                if self.role == Some(Role::Owner) {
                    vec![Effect::WriteToPty(b)]
                } else {
                    vec![not_owner_error("Input requires owner role")]
                }
            }
            ClientMsg::ResizeCommit { epoch, cols, rows } => {
                if self.role != Some(Role::Owner) {
                    return vec![not_owner_error("ResizeCommit requires owner role")];
                }
                if epoch <= self.resize_epoch {
                    // Stale — a newer resize has already been accepted.
                    return vec![];
                }
                self.resize_epoch = epoch;
                vec![
                    Effect::ResizePty { cols, rows },
                    Effect::Broadcast(DaemonMsg::ResizeApplied {
                        epoch,
                        pty_seq: ctx.pty_seq_tail,
                        render_rev: ctx.render_rev,
                        cols,
                        rows,
                    }),
                ]
            }
            ClientMsg::OwnerClaim => {
                let Some(cid) = self.client_id else {
                    // Shouldn't happen post-handshake, but bail cleanly.
                    return vec![];
                };
                let changed = registry.on_claim(cid);
                self.role = Some(Role::Owner);
                if changed {
                    vec![
                        Effect::AssignOwner(Some(cid)),
                        Effect::BroadcastOwnerChanged(Some(cid)),
                    ]
                } else {
                    vec![]
                }
            }
            ClientMsg::OwnerRelease => {
                let Some(cid) = self.client_id else {
                    return vec![];
                };
                let changed = registry.on_release(cid);
                if changed {
                    self.role = Some(Role::Observer);
                    vec![
                        Effect::AssignOwner(None),
                        Effect::BroadcastOwnerChanged(None),
                    ]
                } else {
                    // Wasn't the owner; ignore.
                    vec![]
                }
            }
            ClientMsg::RenderAck {
                render_rev,
                pty_seq: _,
            } => {
                self.last_render_acked_rev = Some(render_rev);
                vec![]
            }
            ClientMsg::Kill => {
                if self.role == Some(Role::Owner) {
                    vec![Effect::KillChild]
                } else {
                    vec![not_owner_error("Kill requires owner role")]
                }
            }
            // A second `ClientHello` on the same connection is a protocol
            // violation; the spec is "one hello per connection".
            ClientMsg::ClientHello { .. } => {
                vec![
                    Effect::SendProtocolError {
                        code: ProtocolErrorCode::BadHandshake,
                        message: "ClientHello already received".to_string(),
                        expected_version: Some(PROTOCOL_VERSION),
                    },
                    Effect::CloseConnection,
                ]
            }
            // Chat-mode frames received in terminal mode are silently
            // dropped (parity with v1 behaviour).
            ClientMsg::ChatUserMessage { .. }
            | ClientMsg::ChatStop
            | ClientMsg::AnswerQuestion { .. } => vec![],
        }
    }

    /// Process the very first frame on a connection. Splits out of
    /// `on_client_frame` only for readability.
    fn process_hello(
        &mut self,
        msg: ClientMsg,
        buffer: &ByteRing,
        registry: &mut OwnerRegistry,
        ctx: &SessionContext<'_>,
    ) -> Vec<Effect> {
        match msg {
            ClientMsg::ClientHello {
                protocol_version,
                terminal_id,
                client_id,
                desired_size,
                cell_size: _,
                initial_scrollback,
                resume_from: _,
                role_hint,
                capabilities,
            } => {
                // 1. Version match — must be exactly PROTOCOL_VERSION.
                if protocol_version != PROTOCOL_VERSION {
                    return vec![
                        Effect::SendProtocolError {
                            code: ProtocolErrorCode::UnsupportedVersion,
                            message: format!(
                                "protocol_version {protocol_version} != {PROTOCOL_VERSION}"
                            ),
                            expected_version: Some(PROTOCOL_VERSION),
                        },
                        Effect::CloseConnection,
                    ];
                }
                // 2. Terminal id match.
                if terminal_id != ctx.terminal_id {
                    return vec![
                        Effect::SendProtocolError {
                            code: ProtocolErrorCode::BadHandshake,
                            message: format!(
                                "terminal_id mismatch: client {terminal_id:?} vs daemon {:?}",
                                ctx.terminal_id
                            ),
                            expected_version: Some(PROTOCOL_VERSION),
                        },
                        Effect::CloseConnection,
                    ];
                }
                // 3. Capability intersection (must include Vt).
                if !capabilities.render_encodings.contains(&RenderEncoding::Vt) {
                    return vec![
                        Effect::SendProtocolError {
                            code: ProtocolErrorCode::UnsupportedEncoding,
                            message: "client capabilities do not include Vt".to_string(),
                            expected_version: Some(PROTOCOL_VERSION),
                        },
                        Effect::CloseConnection,
                    ];
                }

                // Handshake passed — register, build snapshot, reply.
                let role = registry.on_attach(client_id, role_hint);
                self.attached = true;
                self.client_id = Some(client_id);
                self.role = Some(role);

                // In this PR the render plane is byte-passthrough: the
                // snapshot's `data` is the ring's full content (raw PTY
                // bytes). PR-2 will replace this with a VT-model render.
                let snapshot_bytes = buffer.snapshot();
                let scrollback = match initial_scrollback {
                    InitialScrollbackEcho::None => None,
                    InitialScrollbackEcho::All => Some(snapshot_bytes.clone()),
                    InitialScrollbackEcho::Lines(_) => Some(snapshot_bytes.clone()),
                };
                let snapshot = RenderSnapshot {
                    render_rev: ctx.render_rev,
                    pty_seq: ctx.pty_seq_tail,
                    cols: ctx.pty_size.cols,
                    rows: ctx.pty_size.rows,
                    encoding: RenderEncoding::Vt,
                    data: snapshot_bytes,
                    scrollback,
                };

                let server_hello = DaemonMsg::ServerHello {
                    protocol_version: PROTOCOL_VERSION,
                    terminal_id: terminal_id.clone(),
                    session_id: ctx.session_id,
                    client_role: role,
                    owner_client_id: registry.current_owner(),
                    pty_size: ctx.pty_size,
                    pty_seq_head: ctx.pty_seq_head,
                    pty_seq_tail: ctx.pty_seq_tail,
                    render_rev: ctx.render_rev,
                    snapshot,
                    history_gap: None,
                };

                // We also need to push the requested PTY size through to
                // the master so the child sees the client's actual viewport.
                vec![
                    Effect::ResizePty {
                        cols: desired_size.cols,
                        rows: desired_size.rows,
                    },
                    Effect::SendToClient(server_hello),
                ]
            }
            _ => vec![
                Effect::SendProtocolError {
                    code: ProtocolErrorCode::BadHandshake,
                    message: "expected ClientHello as first message".to_string(),
                    expected_version: Some(PROTOCOL_VERSION),
                },
                Effect::CloseConnection,
            ],
        }
    }
}

// Internal alias so `process_hello`'s match doesn't have to re-import
// `crate::InitialScrollback` (its `None`/`All`/`Lines` are
// indistinguishable from the bare scope below otherwise).
use crate::InitialScrollback as InitialScrollbackEcho;

fn not_owner_error(message: &str) -> Effect {
    Effect::SendProtocolError {
        code: ProtocolErrorCode::NotOwner,
        message: message.to_string(),
        expected_version: None,
    }
}

impl Default for TerminalSessionState {
    fn default() -> Self {
        Self::new()
    }
}

/// PTY-byte plane: owns the [`ByteRing`] and produces the broadcast
/// effects for raw PTY chunks and for the child's exit code. Maintains the
/// `pty_seq` and (in this PR identically) the `render_rev` counters.
///
/// One instance per daemon, shared between the PTY-reader thread and the
/// child-waiter task in the shell layer.
pub struct PtyBroadcaster {
    buffer: ByteRing,
    /// Monotonic per-chunk counter. Bumped once per `on_pty_chunk` call —
    /// chunk-granularity, not byte-granularity. PR-2 may switch to
    /// byte-granularity if/when the VT model produces per-glyph patches.
    pty_seq: u32,
    /// Monotonic render revision. In this PR every PTY chunk also bumps
    /// the render rev by 1 — render plane and PTY plane are pinned together
    /// until the VT model lands.
    render_rev: u32,
    /// PTY history low-water mark. We never evict the seq below this — it
    /// indicates the oldest sequence number still represented in the
    /// `ByteRing`. Bumped when a chunk is evicted.
    pty_seq_head: u32,
}

impl PtyBroadcaster {
    pub fn new(max_bytes: usize) -> Self {
        Self {
            buffer: ByteRing::new(max_bytes),
            pty_seq: 0,
            render_rev: 0,
            pty_seq_head: 0,
        }
    }

    /// One PTY chunk arrived. Append to the replay ring (evicting old
    /// chunks as needed, bumping `pty_seq_head`), bump `pty_seq` +
    /// `render_rev`, and emit a `Broadcast(RenderPatch{..})` for every
    /// attached client.
    pub fn on_pty_chunk(&mut self, bytes: Vec<u8>) -> Vec<Effect> {
        let prev_render_rev = self.render_rev;
        // Manual append-with-eviction-tracking: a chunk drop here also
        // moves the seq-head forward (each evicted chunk == one earlier
        // seq increment that's no longer in history).
        let chunk_len = bytes.len();
        self.buffer.total_bytes += chunk_len;
        self.buffer.chunks.push_back(bytes.clone());
        while self.buffer.total_bytes > self.buffer.max_bytes && self.buffer.chunks.len() > 1 {
            let dropped = self.buffer.chunks.pop_front().unwrap();
            self.buffer.total_bytes -= dropped.len();
            self.pty_seq_head = self.pty_seq_head.saturating_add(1);
        }

        self.pty_seq = self.pty_seq.saturating_add(1);
        self.render_rev = self.render_rev.saturating_add(1);

        vec![Effect::Broadcast(DaemonMsg::RenderPatch(RenderPatch {
            render_rev: self.render_rev,
            prev_render_rev,
            pty_seq: self.pty_seq,
            encoding: RenderEncoding::Vt,
            data: bytes,
        }))]
    }

    /// Child exited with the given exit code. Emit a `TerminalExited` to
    /// every client carrying the final `pty_seq` / `render_rev` so the
    /// client can confirm it didn't miss any output.
    pub fn on_child_exit(&mut self, code: Option<i32>) -> Vec<Effect> {
        vec![Effect::Broadcast(DaemonMsg::TerminalExited {
            code,
            pty_seq: self.pty_seq,
            render_rev: self.render_rev,
        })]
    }

    /// Read-only handle on the ring. The shell hands this to
    /// [`TerminalSessionState::on_client_frame`] when serving a handshake
    /// so the state machine can snapshot it into `ServerHello.snapshot.data`.
    pub fn buffer(&self) -> &ByteRing {
        &self.buffer
    }

    pub fn pty_seq(&self) -> u32 {
        self.pty_seq
    }

    pub fn pty_seq_head(&self) -> u32 {
        self.pty_seq_head
    }

    pub fn render_rev(&self) -> u32 {
        self.render_rev
    }
}

// ---- Render plane (PR-2) ------------------------------------------------

/// Server-side render plane: owns the [`TerminalModel`] (VT-driven grid +
/// scrollback) plus the byte-passthrough transcript ring. Replaces
/// [`PtyBroadcaster`] for terminal-mode daemons in PR-2; chat mode and
/// the existing protocol unit tests keep using `PtyBroadcaster`.
///
/// ## `pty_seq` vs `render_rev` (PR-2 divergence)
///
/// - `pty_seq` is bumped **once per PTY chunk**. It tracks bytes
///   delivered, regardless of whether they changed anything visible.
/// - `render_rev` comes from `TerminalModel::rev()`. It only bumps when
///   the grid / cursor / SGR actually changed.
///
/// Consequences:
/// - A no-op chunk (e.g. pure SGR toggle that flips back, or DECSET that
///   we treat as noop) bumps `pty_seq` but may leave `render_rev`
///   unchanged.
/// - A `resize` bumps `render_rev` (the model considers any geometry
///   change a state change) but doesn't touch `pty_seq`.
///
/// Each emitted `RenderPatch` carries both cursors; clients can resync
/// against whichever is more useful.
pub struct RenderPlane {
    model: TerminalModel,
    transcript: ByteRing,
    pty_seq: u32,
    /// Latest viewport (cols, rows) the daemon believes the PTY is at.
    /// Updated by `on_resize`; surfaced to clients in `RenderSnapshot`
    /// when their `desired_size` is `None`-equivalent.
    cols: u16,
    rows: u16,
    /// Backstop: tracks the previous `render_rev` we emitted so each
    /// `RenderPatch.prev_render_rev` is correctly chained.
    last_emitted_render_rev: u32,
}

impl RenderPlane {
    pub fn new(
        cols: u16,
        rows: u16,
        transcript_max_bytes: usize,
        scrollback_max_lines: usize,
    ) -> Self {
        Self {
            model: TerminalModel::new(cols, rows, scrollback_max_lines),
            transcript: ByteRing::new(transcript_max_bytes),
            pty_seq: 0,
            cols,
            rows,
            last_emitted_render_rev: 0,
        }
    }

    /// One PTY chunk arrived. Feed the model (which updates the grid +
    /// bumps `rev` if anything visible changed), append to transcript,
    /// and emit a `Broadcast(RenderPatch{ encoding: Vt, data: raw bytes,
    /// render_rev: model.rev(), prev_render_rev: previous,
    /// pty_seq: bumped })`.
    pub fn on_pty_chunk(&mut self, bytes: Vec<u8>) -> Vec<Effect> {
        // 1. Feed model. `rev()` may or may not bump.
        self.model.feed(&bytes);

        // 2. Transcript bookkeeping (mirrors v1 `ByteRing::append`).
        self.transcript.append(bytes.clone());

        // 3. Cursors.
        self.pty_seq = self.pty_seq.saturating_add(1);
        let new_rev = self.model.rev();
        let prev = self.last_emitted_render_rev;
        self.last_emitted_render_rev = new_rev;

        vec![Effect::Broadcast(DaemonMsg::RenderPatch(RenderPatch {
            render_rev: new_rev,
            prev_render_rev: prev,
            pty_seq: self.pty_seq,
            encoding: RenderEncoding::Vt,
            data: bytes,
        }))]
    }

    /// Child exited. Emit `TerminalExited` carrying current cursors so
    /// the client can confirm it didn't drop output between the last
    /// patch and the exit.
    pub fn on_child_exit(&mut self, code: Option<i32>) -> Vec<Effect> {
        vec![Effect::Broadcast(DaemonMsg::TerminalExited {
            code,
            pty_seq: self.pty_seq,
            render_rev: self.model.rev(),
        })]
    }

    /// PTY (and model) was resized. Updates internal cols/rows and feeds
    /// the model so the grid re-shapes (bumping `rev`). Emits a fresh
    /// `RenderSnapshot` broadcast: clients use it to repaint at the new
    /// geometry instead of accumulating mis-sized patches.
    pub fn on_resize(&mut self, cols: u16, rows: u16) -> Vec<Effect> {
        self.cols = cols;
        self.rows = rows;
        self.model.resize(cols, rows);
        let snap = self.build_snapshot(cols, rows, ScrollbackLimit::None);
        self.last_emitted_render_rev = snap.render_rev;
        vec![Effect::Broadcast(DaemonMsg::RenderSnapshot(snap))]
    }

    /// Build a snapshot bound to the client's desired geometry. Called
    /// at `ClientHello` time and whenever the daemon decides to issue a
    /// hard resync (e.g. broadcast Lagged → `SnapshotRequired`).
    pub fn build_snapshot(
        &self,
        target_cols: u16,
        target_rows: u16,
        scrollback: ScrollbackLimit,
    ) -> RenderSnapshot {
        let data = self.model.snapshot_vt(target_cols, target_rows);
        let scrollback_bytes = match scrollback {
            ScrollbackLimit::None => None,
            other => {
                let bytes = self.model.scrollback_vt(other);
                if bytes.is_empty() { None } else { Some(bytes) }
            }
        };
        RenderSnapshot {
            render_rev: self.model.rev(),
            pty_seq: self.pty_seq,
            cols: target_cols,
            rows: target_rows,
            encoding: RenderEncoding::Vt,
            data,
            scrollback: scrollback_bytes,
        }
    }

    pub fn pty_seq(&self) -> u32 {
        self.pty_seq
    }

    pub fn pty_seq_head(&self) -> u32 {
        // Transcript ring runs without per-chunk seq tracking in PR-2;
        // history-gap detection on the wire side stays "always full snapshot"
        // (`HistoryGap::requires_snapshot = true`). Surface 0 here — the
        // wire field is still populated in `ServerHello.pty_seq_head` for
        // schema compatibility.
        0
    }

    pub fn render_rev(&self) -> u32 {
        self.model.rev()
    }

    pub fn current_size(&self) -> PtySize {
        PtySize {
            cols: self.cols,
            rows: self.rows,
            pixel_width: None,
            pixel_height: None,
        }
    }

    /// Read-only handle on the transcript ring (for parity with
    /// [`PtyBroadcaster::buffer`]; legacy callers that still want raw
    /// bytes can use this. New code goes through `build_snapshot`.).
    pub fn transcript(&self) -> &ByteRing {
        &self.transcript
    }
}
