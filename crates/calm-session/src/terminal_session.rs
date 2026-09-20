//! Pure, IO-free terminal-mode protocol state machine: each client frame / PTY chunk / child exit is fed in and
//! a list of [`Effect`]s comes out for the IO shell to enact. No tokio types, no OS resources, no `Arc`/`Mutex`.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use uuid::Uuid;

use crate::terminal_owner::OwnerLease;
pub use crate::terminal_owner::OwnerRegistry;

use crate::terminal_model::{ScrollbackLimit, TerminalModel};
use crate::{
    ClientCapabilities, ClientMsg, DaemonMsg, PROTOCOL_VERSION, ProtocolErrorCode, PtySize,
    RenderEncoding, RenderPatch, RenderSnapshot, Role,
};

/// How long `render_rev` must stay stable after at least one PTY chunk before [`RenderPlane`] reports the child input-ready; long enough to coalesce a multi-chunk prompt paint.
pub const CHILD_READY_QUIESCENT_MS: u64 = 100;

/// Side-effects emitted by the protocol layer for the IO shell to enact; the state machine never performs IO itself.
#[derive(Debug, PartialEq, Eq)]
pub enum Effect {
    /// Send a single [`DaemonMsg`] to the client whose frame produced this effect.
    SendToClient(DaemonMsg),
    /// Broadcast a [`DaemonMsg`] to every attached client.
    Broadcast(DaemonMsg),
    /// Resize the PTY master. The shell is free to ignore cols/rows == 0.
    ResizePty { cols: u16, rows: u16 },
    /// Write bytes to the PTY stdin; `input_seq` mirrors the [`ClientMsg::Input`] field, and the shell emits
    /// [`DaemonMsg::InputAck`] with it after a successful write when it is non-zero.
    WriteToPty { data: Vec<u8>, input_seq: u64 },
    /// Tear down the child process (SIGHUP the pgid, then SIGKILL fallback; the shell owns that policy).
    KillChild,
    /// Send a typed v2 protocol error to the client as a [`DaemonMsg::ProtocolError`] frame before closing.
    SendProtocolError {
        code: ProtocolErrorCode,
        message: String,
        expected_version: Option<u16>,
    },
    /// Drop the client connection after any preceding `SendProtocolError` has been flushed.
    CloseConnection,
    /// Owner registry transition from a successful `OwnerClaim` / `OwnerRelease`; a marker for observability, paired with `BroadcastOwnerChanged`.
    AssignOwner(Option<Uuid>),
    /// Tell the shell to broadcast a [`DaemonMsg::OwnerChanged`] with the current owner (or `None` after a release).
    BroadcastOwnerChanged(Option<Uuid>),
    /// Legacy: the client violated the protocol; new v2 paths emit `SendProtocolError` + `CloseConnection` instead.
    ProtocolViolation(&'static str),
    /// Update the daemon-side default fg/bg used to answer OSC 10/11 and, when the child has DECSET 1004 enabled,
    /// write `ESC[I` so a focus-aware TUI re-queries them.
    TerminalThemeUpdate { fg: (u8, u8, u8), bg: (u8, u8, u8) },
}

/// Captured protocol admission, rechecked by the IO shell at physical write.
#[derive(Clone, Copy, Debug)]
pub enum InputPermission {
    Owner(OwnerLease),
    Kernel,
    Denied,
}

/// Chunk-granular byte ring used to seed a fresh client's render snapshot; eviction drops whole chunks so
/// replay always starts on a chunk boundary and never slices a multi-byte escape sequence.
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

    /// Push one chunk; over budget, evict whole chunks from the front until we fit or one chunk remains.
    pub fn append(&mut self, bytes: Vec<u8>) {
        self.total_bytes += bytes.len();
        self.chunks.push_back(bytes);
        while self.total_bytes > self.max_bytes && self.chunks.len() > 1 {
            let dropped = self.chunks.pop_front().unwrap();
            self.total_bytes -= dropped.len();
        }
    }

    /// Concatenated copy of every chunk currently buffered.
    pub fn snapshot(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.total_bytes);
        for c in &self.chunks {
            out.extend_from_slice(c);
        }
        out
    }

    pub fn total_bytes(&self) -> usize {
        self.total_bytes
    }
}

/// Context the shell threads through `on_client_frame`.
#[derive(Debug, Clone)]
pub struct SessionContext<'a> {
    /// Terminal id the daemon was launched for, checked against `ClientHello.terminal_id`.
    pub terminal_id: &'a str,
    /// UUID that rolls on every daemon respawn, so a client knows whether the PTY is the same as its last attach.
    pub session_id: Uuid,
    /// Current PTY viewport; the state machine only reports it, never mutates the master.
    pub pty_size: PtySize,
    /// PTY byte sequence head (oldest still in history).
    pub pty_seq_head: u32,
    /// PTY byte sequence tail (most recent).
    pub pty_seq_tail: u32,
    /// Current render revision.
    pub render_rev: u32,
    /// Snapshot of `RenderPlane::child_ready_fired()` for `ServerHello.is_child_ready`; defaults to `false` (wait for ready) on call sites that don't track readiness.
    pub is_child_ready: bool,
    /// Default fg/bg the daemon currently advertises on OSC 10/11, used to suppress a redundant `TerminalThemeUpdate`;
    /// `None` means unknown, and the suppression falls through so a real toggle is never swallowed.
    pub current_default_fg: Option<(u8, u8, u8)>,
    pub current_default_bg: Option<(u8, u8, u8)>,
}

/// Single-client protocol state machine. One instance per accepted socket.
pub struct TerminalSessionState {
    /// True once a valid `ClientHello` has been processed.
    attached: bool,
    /// Client UUID from the successful `ClientHello`. `None` pre-handshake.
    client_id: Option<Uuid>,
    /// Role assigned by the `OwnerRegistry` at handshake time, mutated on successful `OwnerClaim` / `OwnerRelease`.
    role: Option<Role>,
    owner_lease: Option<OwnerLease>,
    /// Latest accepted resize epoch; frames with `epoch <= resize_epoch` are stale and silently dropped.
    resize_epoch: u32,
    /// Last `render_rev` the client acknowledged; not used for back-pressure decisions yet.
    last_render_acked_rev: Option<u32>,
    /// Capabilities the client advertised in its `ClientHello`, cached for post-handshake gating. `None` pre-handshake.
    capabilities: Option<ClientCapabilities>,
}

impl TerminalSessionState {
    pub fn new() -> Self {
        Self {
            attached: false,
            client_id: None,
            role: None,
            owner_lease: None,
            resize_epoch: 0,
            last_render_acked_rev: None,
            capabilities: None,
        }
    }

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

    pub fn input_permission(&self) -> InputPermission {
        if let Some(lease) = self.owner_lease {
            InputPermission::Owner(lease)
        } else if self
            .capabilities
            .as_ref()
            .is_some_and(|cap| cap.kernel_originated_input)
        {
            InputPermission::Kernel
        } else {
            InputPermission::Denied
        }
    }

    /// Release only this connection's lease, including on transport teardown.
    /// A stale connection cannot release a subsequent same-ID reconnect.
    pub fn release_owner(&mut self, registry: &mut OwnerRegistry) -> bool {
        let released = self
            .owner_lease
            .take()
            .is_some_and(|lease| registry.release(lease));
        if self.attached {
            self.role = Some(Role::Observer);
        }
        released
    }

    /// Translate one incoming client frame into a list of side-effects. The first frame MUST be `ClientHello`.
    /// A handshake is read-only with respect to PTY geometry: a remount must not reshape the shared model before its
    /// recovery snapshot is built; owners resize explicitly with `ResizeCommit` after attachment.
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

        // Role is a cached UI value; only the current server-issued lease
        // authorizes owner effects. Client IDs may be reused on reconnect.
        if self.owner_lease.is_some() && self.owner_lease != registry.lease() {
            self.owner_lease = None;
            self.role = Some(Role::Observer);
        }

        match msg {
            ClientMsg::Input { data, input_seq } => {
                // Input is authorized by owner role OR the `kernel_originated_input` capability; the latter is NOT extended to ResizeCommit / Kill on purpose.
                let kernel_input = self
                    .capabilities
                    .as_ref()
                    .map(|c| c.kernel_originated_input)
                    .unwrap_or(false);
                if self.role == Some(Role::Owner) || kernel_input {
                    vec![Effect::WriteToPty { data, input_seq }]
                } else {
                    vec![not_owner_error(INPUT_REQUIRES_OWNER_ROLE)]
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
                let Some(lease) = registry.claim(cid) else {
                    return vec![Effect::SendProtocolError {
                        code: ProtocolErrorCode::BadSequence,
                        message: "terminal owner lease generation exhausted".into(),
                        expected_version: None,
                    }];
                };
                self.owner_lease = Some(lease);
                self.role = Some(Role::Owner);
                vec![
                    Effect::AssignOwner(Some(cid)),
                    Effect::BroadcastOwnerChanged(Some(cid)),
                ]
            }
            ClientMsg::OwnerRelease => {
                if self.release_owner(registry) {
                    vec![
                        Effect::AssignOwner(None),
                        Effect::BroadcastOwnerChanged(None),
                    ]
                } else {
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
            // A second `ClientHello` on the same connection is a protocol violation.
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
            ClientMsg::TerminalThemeUpdate { fg, bg } => {
                // Drop a theme update whose colors already match: the browser re-POSTs the host theme on EVERY mount, and the
                // daemon was spawned with that exact theme. Suppressed before the role gate so an observer's benign mount-time
                // re-POST does not surface as NotOwner; the unchanged path emits no effect, so no authorization is bypassed.
                let unchanged =
                    ctx.current_default_fg == Some(fg) && ctx.current_default_bg == Some(bg);
                if unchanged {
                    return vec![];
                }

                // Same authorization shape as `Input`: this flips the advertised OSC 10/11 colors and may write `ESC[I` to the PTY,
                // so an observer MUST NOT rewrite another user's terminal colors through a forged WS frame.
                let kernel_input = self
                    .capabilities
                    .as_ref()
                    .map(|c| c.kernel_originated_input)
                    .unwrap_or(false);
                if self.role != Some(Role::Owner) && !kernel_input {
                    return vec![not_owner_error(
                        "TerminalThemeUpdate requires owner role or kernel_originated_input capability",
                    )];
                }
                vec![Effect::TerminalThemeUpdate { fg, bg }]
            }
            // Question-answer frames are consumed by higher-level agent plumbing; no terminal-session side effect.
            ClientMsg::AnswerQuestion { .. } => vec![],
        }
    }

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

                let role = registry.on_attach(client_id, role_hint);
                self.attached = true;
                self.client_id = Some(client_id);
                self.role = Some(role);
                self.owner_lease = if role == Role::Owner {
                    registry.lease()
                } else {
                    None
                };
                self.capabilities = Some(capabilities.clone());

                // The snapshot's `data` is the ring's full content (raw PTY bytes).
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
                    is_child_ready: ctx.is_child_ready,
                };

                // `desired_size` must not mutate the shared PTY/model: a page remount is a reconnect, not a resize intent, and
                // resizing here destroyed content before ServerHello could restore it.
                let _ = desired_size;
                vec![Effect::SendToClient(server_hello)]
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

// Alias so `process_hello`'s match doesn't shadow `None`/`All`/`Lines`.
use crate::InitialScrollback as InitialScrollbackEcho;

/// Kernel clients match on this message to tell an input refusal apart from an ownership-claim refusal (both use `NotOwner`).
pub const INPUT_REQUIRES_OWNER_ROLE: &str =
    "Input requires owner role or kernel_originated_input capability";

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

/// PTY-byte plane: owns the [`ByteRing`] and produces the broadcast effects for raw PTY chunks and child exit. Legacy; retained as the protocol test fixture.
pub struct PtyBroadcaster {
    buffer: ByteRing,
    /// Monotonic per-chunk counter (chunk-granularity, not byte-granularity).
    pty_seq: u32,
    /// Monotonic render revision; here every PTY chunk also bumps it by 1.
    render_rev: u32,
    /// Oldest sequence number still represented in the `ByteRing`; bumped when a chunk is evicted.
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

    /// One PTY chunk arrived: append to the replay ring, bump `pty_seq` + `render_rev`, and broadcast a `RenderPatch`.
    pub fn on_pty_chunk(&mut self, bytes: Vec<u8>) -> Vec<Effect> {
        let prev_render_rev = self.render_rev;
        // Manual append so an evicted chunk also moves the seq-head forward.
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

    /// Child exited; broadcast `TerminalExited` carrying the final cursors so clients can confirm they missed no output.
    pub fn on_child_exit(&mut self, code: Option<i32>) -> Vec<Effect> {
        vec![Effect::Broadcast(DaemonMsg::TerminalExited {
            code,
            pty_seq: self.pty_seq,
            render_rev: self.render_rev,
        })]
    }

    /// Read-only handle on the ring, snapshotted into `ServerHello.snapshot.data` at handshake.
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

/// An optional read-only client projection installed before output is ingested.
/// The render plane remains the sole source of terminal protocol replies.
pub trait RenderObserver: Send + Sync {
    fn unavailable(&mut self, reason: &str);
    fn output(&mut self, bytes: &[u8]);
    fn resize(&mut self, cols: u16, rows: u16);
    fn colors(&mut self, fg: Option<(u8, u8, u8)>, bg: Option<(u8, u8, u8)>);
}

pub struct RenderPlane {
    observer: Option<Box<dyn RenderObserver>>,
    model: TerminalModel,
    transcript: ByteRing,
    pty_seq: u32,
    /// Latest viewport (cols, rows) the daemon believes the PTY is at.
    cols: u16,
    rows: u16,
    /// Previous `render_rev` emitted, so each `RenderPatch.prev_render_rev` is correctly chained.
    last_emitted_render_rev: u32,
    /// Instant of the most recent `render_rev` increase; `None` until the first PTY chunk. Drives `detect_ready`.
    last_rev_change_at: Option<Instant>,
    /// `true` once `detect_ready` has fired `ChildReady`; one-shot per session.
    child_ready_fired: bool,
    /// Injectable clock; every wall-clock "now" read in `RenderPlane` MUST route through this or a mock clock diverges from real time.
    now: Box<dyn Fn() -> Instant + Send + Sync>,
}

impl RenderPlane {
    pub fn invalidate_observation(&mut self, reason: &str) {
        if let Some(observer) = &mut self.observer {
            observer.unavailable(reason);
        }
    }
    /// Install only on a fresh plane; an observer cannot reconstruct missed bytes.
    pub fn install_observer(&mut self, observer: Box<dyn RenderObserver>) {
        assert!(
            self.pty_seq == 0 && self.observer.is_none(),
            "observer must be installed before output"
        );
        self.observer = Some(observer);
    }

    /// Production constructor: wires the clock to [`Instant::now`].
    pub fn new(
        cols: u16,
        rows: u16,
        transcript_max_bytes: usize,
        scrollback_max_lines: usize,
    ) -> Self {
        Self::with_clock(
            cols,
            rows,
            transcript_max_bytes,
            scrollback_max_lines,
            Box::new(Instant::now),
        )
    }

    /// Same as [`Self::new`] but pre-seeds the model's OSC 10/11 reply colors, so a child's startup probe gets an authoritative answer before the first PTY chunk.
    pub fn with_colors(
        cols: u16,
        rows: u16,
        transcript_max_bytes: usize,
        scrollback_max_lines: usize,
        default_fg: Option<(u8, u8, u8)>,
        default_bg: Option<(u8, u8, u8)>,
    ) -> Self {
        let mut rp = Self::new(cols, rows, transcript_max_bytes, scrollback_max_lines);
        rp.model.set_default_colors(default_fg, default_bg);
        rp
    }

    /// Replace the default fg/bg the model advertises on OSC 10/11 query.
    pub fn set_default_colors(&mut self, fg: Option<(u8, u8, u8)>, bg: Option<(u8, u8, u8)>) {
        if let Some(observer) = &mut self.observer {
            observer.colors(fg, bg);
        }
        self.model.set_default_colors(fg, bg);
    }

    /// Current default foreground the model advertises on OSC 10; lets the session state drop a redundant `TerminalThemeUpdate`.
    pub fn default_fg(&self) -> Option<(u8, u8, u8)> {
        self.model.default_fg()
    }

    /// Current default background the model advertises on OSC 11.
    pub fn default_bg(&self) -> Option<(u8, u8, u8)> {
        self.model.default_bg()
    }

    /// Whether the child has enabled DECSET 1004; gates the mid-session `ESC[I` write on theme toggle, since a shell's
    /// line editor never enables 1004 and a stray `ESC[I` would land in its line buffer.
    pub fn focus_event_tracking(&self) -> bool {
        self.model.focus_event_tracking()
    }

    /// Constructor with an injected clock (test use). Every wall-clock read inside `RenderPlane` MUST go through
    /// `self.now`, or the virtual and real time bases diverge and the ready detector fires instantly or never.
    pub fn with_clock(
        cols: u16,
        rows: u16,
        transcript_max_bytes: usize,
        scrollback_max_lines: usize,
        now: Box<dyn Fn() -> Instant + Send + Sync>,
    ) -> Self {
        Self {
            observer: None,
            model: TerminalModel::new(cols, rows, scrollback_max_lines),
            transcript: ByteRing::new(transcript_max_bytes),
            pty_seq: 0,
            cols,
            rows,
            last_emitted_render_rev: 0,
            last_rev_change_at: None,
            child_ready_fired: false,
            now,
        }
    }

    /// One PTY chunk arrived: feed the model, append to the transcript, broadcast a `RenderPatch`, and reset the
    /// `ChildReady` quiescent timer whenever the model's `rev()` actually bumped.
    pub fn on_pty_chunk(&mut self, bytes: Vec<u8>) -> Vec<Effect> {
        let prev_rev = self.model.rev();

        if let Some(observer) = &mut self.observer {
            observer.output(&bytes);
        }
        self.model.feed(&bytes);

        self.transcript.append(bytes.clone());

        self.pty_seq = self.pty_seq.saturating_add(1);
        let new_rev = self.model.rev();
        let prev = self.last_emitted_render_rev;
        self.last_emitted_render_rev = new_rev;

        // A no-op chunk leaves the timer alone: a child echoing nothing visible IS idle.
        if new_rev != prev_rev {
            self.last_rev_change_at = Some((self.now)());
        }

        let mut effects: Vec<Effect> = Vec::with_capacity(2);
        effects.push(Effect::Broadcast(DaemonMsg::RenderPatch(RenderPatch {
            render_rev: new_rev,
            prev_render_rev: prev,
            pty_seq: self.pty_seq,
            encoding: RenderEncoding::Vt,
            data: bytes,
        })));
        // Route the model's OSC reply bytes back to the PTY master; `input_seq: 0` because the daemon wants no ack for its own writes.
        let replies = self.model.take_pending_osc_replies();
        if !replies.is_empty() {
            effects.push(Effect::WriteToPty {
                data: replies,
                input_seq: 0,
            });
        }
        effects
    }

    /// Poll for the one-shot `ChildReady` signal: `Some` the first time the quiescent window has elapsed since the last
    /// `render_rev` change AND at least one PTY chunk was observed. Poll-based to avoid a deadline task per chunk racing the timer reset.
    pub fn detect_ready(&mut self) -> Option<Effect> {
        if self.child_ready_fired {
            return None;
        }
        let last = self.last_rev_change_at?;
        if (self.now)().duration_since(last) >= Duration::from_millis(CHILD_READY_QUIESCENT_MS) {
            self.child_ready_fired = true;
            return Some(Effect::Broadcast(DaemonMsg::ChildReady {
                pty_seq: self.pty_seq,
                render_rev: self.model.rev(),
            }));
        }
        None
    }

    /// Whether `ChildReady` has already fired; for acceptance tests, production should call [`Self::detect_ready`].
    pub fn child_ready_fired(&self) -> bool {
        self.child_ready_fired
    }

    /// Child exited; broadcast `TerminalExited` carrying current cursors.
    pub fn on_child_exit(&mut self, code: Option<i32>) -> Vec<Effect> {
        vec![Effect::Broadcast(DaemonMsg::TerminalExited {
            code,
            pty_seq: self.pty_seq,
            render_rev: self.model.rev(),
        })]
    }

    /// PTY (and model) was resized; broadcasts a fresh `RenderSnapshot` so clients repaint instead of accumulating mis-sized patches.
    pub fn on_resize(&mut self, cols: u16, rows: u16) -> Vec<Effect> {
        self.cols = cols;
        self.rows = rows;
        if let Some(observer) = &mut self.observer {
            observer.resize(cols, rows);
        }
        self.model.resize(cols, rows);
        let snap = self.build_snapshot(cols, rows, ScrollbackLimit::None);
        self.last_emitted_render_rev = snap.render_rev;
        vec![Effect::Broadcast(DaemonMsg::RenderSnapshot(snap))]
    }

    /// Build a snapshot bound to the client's desired geometry.
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
        // The transcript ring has no per-chunk seq tracking; history-gap detection is always "full snapshot", so surface 0 for schema compatibility.
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

    /// Read-only handle on the transcript ring; new code goes through `build_snapshot`.
    pub fn transcript(&self) -> &ByteRing {
        &self.transcript
    }
}
