//! Pure, IO-free terminal-mode protocol state machine (v2).
//!
//! This module is the testable core of the per-client protocol that runs in
//! calm-server's terminal renderer. The renderer's IO shell owns the PTY
//! attachment, WebSocket bridge, tokio tasks, and broadcast channels. Each
//! [`ClientMsg`] it reads off a client connection — and each PTY chunk /
//! child-exit event it observes — is fed into one of the types here, which
//! decide what to do and emit a list of [`Effect`]s for the shell to enact.
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
//!   (`tests/v2_protocol.rs`).
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
use std::time::{Duration, Instant};

use uuid::Uuid;

use crate::terminal_model::{ScrollbackLimit, TerminalModel};
use crate::{
    ClientCapabilities, ClientMsg, DaemonMsg, PROTOCOL_VERSION, ProtocolErrorCode, PtySize,
    RenderEncoding, RenderPatch, RenderSnapshot, Role,
};

/// How long `render_rev` must remain stable (no further bumps) after at
/// least one PTY chunk has been observed before [`RenderPlane`] reports
/// the child as input-ready. Tuned for typical shell prompts which paint
/// PS1 in a single CSI burst and then idle; agent CLIs (Claude / codex /
/// gemini) also tend to render their startup banner in one go then wait
/// for input. 100ms is long enough to coalesce a multi-chunk paint
/// without making the kernel wait noticeably before injecting stdin.
pub const CHILD_READY_QUIESCENT_MS: u64 = 100;

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
    ///
    /// `input_seq` mirrors the [`ClientMsg::Input`] field that produced
    /// this effect. The shell uses it to drive a per-write ack: after
    /// the PTY master write returns successfully, the shell emits a
    /// [`DaemonMsg::InputAck`] back to the originating connection
    /// carrying this seq. `input_seq == 0` means the client did not
    /// request an ack — the shell still performs the write but does not
    /// emit any ack frame. See [`ClientMsg::Input`] and
    /// [`DaemonMsg::InputAck`] for the wire-level contract.
    WriteToPty { data: Vec<u8>, input_seq: u64 },
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
    /// Update the daemon-side default fg/bg used to answer OSC 10/11
    /// color queries, and nudge a focus-aware TUI to re-query (#177,
    /// refined by #305). The shell calls `RenderPlane::set_default_colors`
    /// and, when the child has DECSET 1004 enabled, writes `ESC[I` to
    /// the PTY; crossterm-based TUIs (codex, claude-tui) re-emit
    /// `OSC 10;? + OSC 11;?` on `FocusGained`, and the daemon's vte
    /// parser synthesizes the solicited reply from the just-updated
    /// defaults.
    TerminalThemeUpdate { fg: (u8, u8, u8), bg: (u8, u8, u8) },
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
    /// Snapshot of `RenderPlane::child_ready_fired()` captured at the
    /// moment the daemon built this context. Threaded into
    /// `DaemonMsg::ServerHello.is_child_ready` so a late-joining client
    /// (e.g. the kernel's transient input-injection connection) knows
    /// whether the one-shot `ChildReady` broadcast has already fired.
    ///
    /// Defaults to `false` (the safe "wait for ready" assumption) on
    /// call sites that don't track child-readiness — notably the legacy
    /// [`PtyBroadcaster`]-backed unit tests in `tests/v2_protocol.rs`.
    pub is_child_ready: bool,
    /// Default foreground/background colors the daemon currently
    /// advertises on OSC 10/11 (mirrors `RenderPlane::default_fg/_bg`).
    /// Used by the `TerminalThemeUpdate` handler to suppress a redundant
    /// theme update whose colors already match what the daemon is
    /// serving (the New-terminal mount case — fix A for the OSC-echo
    /// bug). `None` on call sites that pre-date theming (the legacy
    /// `PtyBroadcaster` unit-test fixture), which makes the equality
    /// check below fall through to "treat as a real change" — i.e. the
    /// suppression is opt-in and never silently swallows a toggle when
    /// the daemon's current colors are unknown.
    pub current_default_fg: Option<(u8, u8, u8)>,
    pub current_default_bg: Option<(u8, u8, u8)>,
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
    /// Capabilities the client advertised in its `ClientHello`. Cached
    /// here so post-handshake frame handlers can branch on flags like
    /// `kernel_originated_input` without rethreading the original hello.
    /// `None` pre-handshake.
    capabilities: Option<ClientCapabilities>,
}

impl TerminalSessionState {
    pub fn new() -> Self {
        Self {
            attached: false,
            client_id: None,
            role: None,
            resize_epoch: 0,
            last_render_acked_rev: None,
            capabilities: None,
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
    /// returned `Role`, and emit `ServerHello` with the current snapshot.
    /// Owner handshakes also emit `ResizePty` so the daemon picks up the
    /// owner's `desired_size`.
    ///
    /// Post-handshake routing:
    /// - `Input{ data, input_seq }` → owner / kernel-input observer:
    ///   `WriteToPty { data, input_seq }`; observer without kernel-input:
    ///   `NotOwner` error. The seq is forwarded verbatim; the shell uses
    ///   it post-write to emit `DaemonMsg::InputAck` to the originating
    ///   connection (when seq > 0). The state machine never validates or
    ///   tracks ordering.
    /// - `ResizeCommit{epoch,..}` → owner: bump epoch if `>` current,
    ///   emit `ResizePty` + broadcast `ResizeApplied`; stale epoch is a
    ///   silent no-op. Observer → `NotOwner`.
    /// - `OwnerClaim` → registry takeover; on actual change emit
    ///   `AssignOwner` + `BroadcastOwnerChanged` and bump this state's
    ///   role to `Owner`.
    /// - `OwnerRelease` → registry clear; mirror role to Observer.
    /// - `RenderAck` → update `last_render_acked_rev` (no other effect).
    /// - `Kill` → owner: `KillChild`; observer: `NotOwner`.
    /// - unknown / forward-compatibility variants → silent no-op.
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
            ClientMsg::Input { data, input_seq } => {
                // Two paths can authorize input:
                // (1) Owner role — the default.
                // (2) kernel_originated_input capability — only set by
                //     the kernel's own DaemonClient over the
                //     kernel-private unix socket (see field docs in
                //     `crate::ClientCapabilities`). NOT extended to
                //     ResizeCommit / Kill on purpose; the kernel relays
                //     input but is not the source of truth for viewport
                //     / lifecycle.
                let kernel_input = self
                    .capabilities
                    .as_ref()
                    .map(|c| c.kernel_originated_input)
                    .unwrap_or(false);
                if self.role == Some(Role::Owner) || kernel_input {
                    // `input_seq` is forwarded into the effect verbatim;
                    // the shell will emit `DaemonMsg::InputAck` after
                    // the actual PTY write completes when seq > 0. seq
                    // == 0 means "no ack requested" — the browser path's
                    // wire default.
                    vec![Effect::WriteToPty { data, input_seq }]
                } else {
                    vec![not_owner_error(
                        "Input requires owner role or kernel_originated_input capability",
                    )]
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
            ClientMsg::TerminalThemeUpdate { fg, bg } => {
                // Fix A — drop the redundant mount-time theme update.
                //
                // `web/src/XtermView.tsx`'s theme effect fires on EVERY
                // mount (deliberately, so a real toggle while the WS was
                // down still reaches the daemon), so a freshly-opened
                // New-terminal card always POSTs a `TerminalThemeUpdate`
                // carrying the host's current theme. But the daemon was
                // spawned with that exact theme via `--terminal-fg/-bg`
                // (see daemon.rs `with_colors`), so this first update is
                // a no-op color-wise. Pre-#305 the daemon still wrote
                // a synthetic `OSC 10/11 + focus-in` blob; a shell at
                // its prompt runs ZLE/readline in raw mode (ECHO off,
                // ICANON off) and treated the injected bytes as INPUT,
                // redrawing them as `^[]10;rgb:…` glyphs (#295).
                //
                // Suppress this no-op before the role gate (#359) so an
                // observer's benign #177 mount-time re-POST does not
                // surface as a NotOwner protocol error. This is safe:
                // the unchanged path emits no effect, writes nothing to
                // the PTY, and changes no state, so no authorization is
                // bypassed. A genuine toggle (colors actually differ)
                // still flows through to the authorization check below.
                // We only suppress when `current_default_*` is known
                // (`Some`); an unknown current color (legacy fixtures)
                // falls through to the original always-emit behaviour,
                // so we never swallow a real change.
                let unchanged =
                    ctx.current_default_fg == Some(fg) && ctx.current_default_bg == Some(bg);
                if unchanged {
                    return vec![];
                }

                // Same authorization shape as `Input`: owner OR
                // kernel-input observer. This flips the daemon's
                // advertised OSC 10/11 colors and (under DECSET 1004)
                // writes `ESC[I` to the PTY, so we MUST NOT let an
                // observer rewrite another user's terminal colors
                // through a forged WS frame.
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
            // Question-answer frames are consumed by higher-level agent
            // plumbing and have no terminal-session side effect.
            ClientMsg::AnswerQuestion { .. } => vec![],
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
                // Cache capabilities for post-handshake gating
                // (kernel_originated_input on Input frames, etc.).
                self.capabilities = Some(capabilities.clone());

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
                    is_child_ready: ctx.is_child_ready,
                };

                let mut effects = Vec::new();
                if role == Role::Owner {
                    // PTY size is owner-driven, matching ResizeCommit.
                    // Observers learn the owner's size through ServerHello
                    // and snapshots; they never reshape the shared PTY.
                    effects.push(Effect::ResizePty {
                        cols: desired_size.cols,
                        rows: desired_size.rows,
                    });
                }
                effects.push(Effect::SendToClient(server_hello));
                effects
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
/// [`PtyBroadcaster`] for terminal-mode daemons in PR-2; the existing
/// protocol unit tests keep using `PtyBroadcaster`.
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
    /// Wall-clock instant of the most recent `render_rev` increase.
    /// `None` until the first PTY chunk has been observed; reset on
    /// every subsequent chunk that bumps the model's `rev()`. Drives
    /// the [`detect_ready`] quiescent-window check.
    ///
    /// [`detect_ready`]: RenderPlane::detect_ready
    last_rev_change_at: Option<Instant>,
    /// `true` once [`detect_ready`] has fired `ChildReady`; suppresses
    /// duplicate emissions. One-shot per session by design (the kernel
    /// only needs the first ready signal — subsequent quiescent windows
    /// are normal shell idle, not interesting).
    ///
    /// [`detect_ready`]: RenderPlane::detect_ready
    child_ready_fired: bool,
    /// Injectable clock used everywhere RenderPlane needs "now". Production
    /// goes through [`Self::new`], which wires this to `Instant::now`; tests
    /// use [`Self::with_clock`] to provide an `AtomicU64`-backed mock so
    /// the quiescent-window detector can be exercised without wall-clock
    /// sleeps. See [`Self::with_clock`] for the contract on adding new
    /// wall-clock call sites.
    now: Box<dyn Fn() -> Instant + Send + Sync>,
}

impl RenderPlane {
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

    /// Same as [`Self::new`] but pre-seeds the model's OSC 10/11 reply
    /// colors. The daemon passes the host browser's theme RGB here on
    /// spawn so codex's startup probe gets an authoritative answer
    /// before the first PTY chunk lands. See
    /// [`crate::TerminalTheme`] / `--terminal-fg` / `--terminal-bg`.
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

    /// Replace the default fg/bg the model advertises on OSC 10/11
    /// query. Drives the mid-session theme-toggle path (#177): the
    /// session-frame handler updates the model, then writes a synthetic
    /// OSC reply to the PTY master.
    pub fn set_default_colors(&mut self, fg: Option<(u8, u8, u8)>, bg: Option<(u8, u8, u8)>) {
        self.model.set_default_colors(fg, bg);
    }

    /// Current default foreground the model advertises on an OSC 10
    /// query. Read by the daemon when building `SessionContext` so the
    /// session state machine can drop a redundant `TerminalThemeUpdate`
    /// whose colors already match (the New-terminal mount case — see
    /// `TerminalSessionState::on_client_frame`).
    pub fn default_fg(&self) -> Option<(u8, u8, u8)> {
        self.model.default_fg()
    }

    /// Current default background the model advertises on an OSC 11
    /// query. See [`Self::default_fg`].
    pub fn default_bg(&self) -> Option<(u8, u8, u8)> {
        self.model.default_bg()
    }

    /// Whether the PTY child has enabled DECSET 1004 (focus event
    /// reporting). The daemon reads this to gate the mid-session
    /// `ESC[I` write on theme toggle: only a focus-aware TUI (codex
    /// opts in on startup) will treat it as `FocusGained` and
    /// re-query OSC 10/11; a shell's line editor sits in raw mode but
    /// never enables 1004, so a stray `ESC[I` would land in its line
    /// buffer. See `daemon.rs` `Effect::TerminalThemeUpdate`.
    pub fn focus_event_tracking(&self) -> bool {
        self.model.focus_event_tracking()
    }

    /// Constructor with an injected clock (test use). Production goes
    /// through [`Self::new`].
    ///
    /// # Time injection
    ///
    /// Production code path uses [`Self::new`], which wires `now` to
    /// [`Instant::now`]. Tests use this constructor with a mock clock
    /// (typically an `Arc<AtomicU64>` of "virtual milliseconds since
    /// base") so the quiescent-window detector can be driven without
    /// real `tokio::time::sleep`.
    ///
    /// **Contract for future maintainers:** every wall-clock "now"
    /// read inside `RenderPlane` MUST route through `self.now` —
    /// including the `Instant::elapsed()` comparison inside
    /// [`Self::detect_ready`], which is really `Instant::now() -
    /// last`. If a mock clock writes a virtual instant via `self.now`
    /// but a comparison elsewhere reads `Instant::now()` directly, the
    /// virtual and real time bases diverge and the detector either
    /// fires instantly (real now ≫ virtual base) or never (real now
    /// drifts past virtual deadline). Any new wall-clock call site you
    /// add to this type must go through `self.now` or the test suite's
    /// virtual-time guarantee is broken.
    pub fn with_clock(
        cols: u16,
        rows: u16,
        transcript_max_bytes: usize,
        scrollback_max_lines: usize,
        now: Box<dyn Fn() -> Instant + Send + Sync>,
    ) -> Self {
        Self {
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

    /// One PTY chunk arrived. Feed the model (which updates the grid +
    /// bumps `rev` if anything visible changed), append to transcript,
    /// and emit a `Broadcast(RenderPatch{ encoding: Vt, data: raw bytes,
    /// render_rev: model.rev(), prev_render_rev: previous,
    /// pty_seq: bumped })`.
    ///
    /// Also bookkeeps [`Self::last_rev_change_at`] for the `ChildReady`
    /// quiescent-window detector — whenever the model's `rev()` actually
    /// bumps, the timer resets to "now", which keeps
    /// [`Self::detect_ready`] from firing while the prompt is still
    /// being painted.
    pub fn on_pty_chunk(&mut self, bytes: Vec<u8>) -> Vec<Effect> {
        let prev_rev = self.model.rev();

        // 1. Feed model. `rev()` may or may not bump.
        self.model.feed(&bytes);

        // 2. Transcript bookkeeping (mirrors v1 `ByteRing::append`).
        self.transcript.append(bytes.clone());

        // 3. Cursors.
        self.pty_seq = self.pty_seq.saturating_add(1);
        let new_rev = self.model.rev();
        let prev = self.last_emitted_render_rev;
        self.last_emitted_render_rev = new_rev;

        // 4. ChildReady quiescent timer: reset whenever the model's
        //    `rev()` actually bumped. `detect_ready` then fires once
        //    the timer has been idle for `CHILD_READY_QUIESCENT_MS`.
        //    A no-op chunk (pure C0/SGR that flips back to the same
        //    state) leaves the timer alone — which is what we want:
        //    a child that's silently echoing nothing visible IS idle.
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
        // 5. Drain OSC 10/11 reply bytes the model produced this feed
        //    (codex's startup probe lands inside the very first chunk).
        //    Routed back to the PTY master via Effect::WriteToPty so
        //    crossterm's stdin event queue sees the answer.
        //    `input_seq: 0` — the daemon doesn't want an ack for its
        //    own synthesized writes; this is fire-and-forget.
        let replies = self.model.take_pending_osc_replies();
        if !replies.is_empty() {
            effects.push(Effect::WriteToPty {
                data: replies,
                input_seq: 0,
            });
        }
        effects
    }

    /// Poll for the one-shot `ChildReady` signal. Returns `Some(Effect)`
    /// the *first* time the quiescent window has elapsed since the last
    /// `render_rev` change AND at least one PTY chunk has been observed;
    /// returns `None` on every subsequent call (or before the window
    /// elapses).
    ///
    /// Driver: the daemon shell calls this on a 50ms `tokio::time::interval`
    /// in terminal mode. Chat mode never calls it. The poll-based shape
    /// avoids spawning a deadline task per chunk (which would race the
    /// next chunk's reset of `last_rev_change_at`).
    ///
    /// Returns `Effect::Broadcast(DaemonMsg::ChildReady { ... })` carrying
    /// the snapshot of `pty_seq` and `render_rev` at the moment of
    /// detection — the client can correlate against its own cursors to
    /// know exactly what state the child reached "ready" in.
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

    /// Whether `ChildReady` has already been fired this session.
    /// Production code shouldn't branch on this — call
    /// [`Self::detect_ready`] and act on the returned `Option`; the
    /// accessor is here for acceptance tests that want to assert the
    /// one-shot state machine without polling the channel.
    pub fn child_ready_fired(&self) -> bool {
        self.child_ready_fired
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
