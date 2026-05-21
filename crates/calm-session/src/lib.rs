//! Wire protocol + framing helpers shared between the daemon and its clients.
//!
//! As of v2 (issue #44) a client (neige-server, or the standalone test CLI)
//! opens the daemon's Unix socket and sends [`ClientMsg::ClientHello`] as the
//! first frame, carrying its `protocol_version`, `terminal_id`, `client_id`,
//! desired viewport / cell metrics, optional resume cursor, role hint, and
//! capability set. The daemon validates the handshake, assigns owner/observer
//! role via a daemon-level `OwnerRegistry`, and replies with
//! [`DaemonMsg::ServerHello`] including an initial [`RenderSnapshot`]. From
//! there it's a duplex stream of [`ClientMsg::Input`] / [`ResizeCommit`] /
//! ownership / ack frames upstream and [`DaemonMsg::RenderPatch`] /
//! [`ResizeApplied`] / [`TerminalExited`] frames downstream.
//!
//! As of PR-2 the daemon runs a server-side VT model
//! (`calm-session::terminal_model::TerminalModel`):
//! - `RenderSnapshot.data` is the model's serialized ANSI representation
//!   of the visible viewport, bound to the client's `desired_size`.
//! - `RenderPatch.data` is the raw PTY chunk that triggered the rev
//!   bump (still `encoding = Vt`); xterm.js applies it client-side.
//!
//! Cell-grid diff encoding is a follow-up; not in this PR.
//!
//! Framing: `[magic (4) = b"NEIG"] [version (u16 BE) = 2] [length (u32 BE)]
//! [payload (bincode)]`.
//!
//! Magic + version were added in issue #45 so a daemon binary built against
//! an incompatible `ClientMsg`/`DaemonMsg` enum (variant reorder, new variant
//! inserted, etc.) fails fast at the read site with a typed [`FrameError`]
//! instead of silently misinterpreting the bincode discriminants that follow.

pub mod stream_json;
pub mod terminal_model;
pub mod terminal_session;

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use ts_rs::TS;
use uuid::Uuid;

/// Cap on a single frame. Anything larger is either a bug or hostile.
pub const MAX_FRAME: usize = 16 * 1024 * 1024;

/// Four-byte sentinel at the head of every frame. Lets the reader reject
/// random bytes / wrong protocol on a connected socket before attempting a
/// bincode decode that would otherwise succeed-with-garbage.
pub const FRAME_MAGIC: [u8; 4] = *b"NEIG";

/// Bumped whenever the on-wire payload format changes incompatibly (enum
/// variant reorder, payload shape change, ...). A reader seeing an
/// unexpected version closes the connection cleanly via
/// [`FrameError::UnsupportedFrameVersion`] rather than parsing the bytes
/// against the wrong schema.
///
/// v2: `ClientMsg` / `DaemonMsg` terminal variants completely replaced (no
/// v1 compatibility); chat variants unchanged. See issue #44.
pub const FRAME_VERSION: u16 = 2;

/// Application-layer protocol version carried in [`ClientMsg::ClientHello`]
/// and [`DaemonMsg::ServerHello`]. Distinct from [`FRAME_VERSION`] because
/// the wire envelope and the payload schema can move independently; today
/// they happen to be in lockstep at 2/2.
pub const PROTOCOL_VERSION: u16 = 2;

/// Typed errors from the framing layer. The kernel↔daemon WS bridge in
/// `calm-server` matches on [`FrameError::BadMagic`] /
/// [`FrameError::UnsupportedFrameVersion`] to log + close the connection on
/// version skew (see `crates/calm-server/src/ws/terminal.rs`).
#[derive(thiserror::Error, Debug)]
pub enum FrameError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("bincode encode: {0}")]
    Encode(#[from] bincode::error::EncodeError),
    #[error("bincode decode: {0}")]
    Decode(#[from] bincode::error::DecodeError),
    #[error("bad frame magic: got {got:?}, expected {expected:?}")]
    BadMagic { got: [u8; 4], expected: [u8; 4] },
    #[error("unsupported frame version: got {got}, supported {supported}")]
    UnsupportedFrameVersion { got: u16, supported: u16 },
    #[error("frame too large: {len} > {max}")]
    Oversize { len: u32, max: u32 },
}

// ---- v2 protocol value types -------------------------------------------

/// Per-connection role assigned by the daemon's `OwnerRegistry`. The first
/// successful handshake on a freshly-spawned daemon becomes the
/// [`Role::Owner`]; subsequent clients default to [`Role::Observer`] and can
/// promote themselves with [`ClientMsg::OwnerClaim`] (hostile takeover —
/// the daemon never negotiates).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "web/src/api/generated-terminal.ts")]
pub enum Role {
    Owner,
    Observer,
}

/// Render-plane payload encoding. Today the daemon only ever advertises
/// [`RenderEncoding::Vt`] (raw escape-sequence bytes); the enum exists so
/// later additions (cell-grid diffs, sixel images, ...) don't require a
/// fresh `FRAME_VERSION` bump.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "web/src/api/generated-terminal.ts")]
pub enum RenderEncoding {
    Vt,
}

/// How much pre-attach history the client wants in the
/// [`DaemonMsg::ServerHello`] snapshot. `None` = just the current viewport;
/// `All` = everything the daemon still has; `Lines(n)` = up to n lines of
/// scrollback (whole-chunk granularity in this PR, may tighten to
/// line-granular when the VT model lands).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "web/src/api/generated-terminal.ts")]
pub enum InitialScrollback {
    None,
    All,
    Lines(u32),
}

/// PTY viewport dimensions plus an optional pixel-size hint. The pixel
/// fields are only consulted by programs that draw inline images (sixel /
/// kitty graphics); most clients leave them `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "web/src/api/generated-terminal.ts")]
pub struct PtySize {
    pub cols: u16,
    pub rows: u16,
    pub pixel_width: Option<u16>,
    pub pixel_height: Option<u16>,
}

/// Single cell's pixel footprint as the client measured it. Sent only when
/// it materially differs from the daemon-side default and the client wants
/// pixel-accurate image alignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "web/src/api/generated-terminal.ts")]
pub struct CellSize {
    pub width: u16,
    pub height: u16,
}

/// Reconnect cursor — the latest `render_rev` and/or `pty_seq` the client
/// already has. The daemon decides whether it can replay a delta from there
/// or must send a fresh snapshot (in which case a
/// [`HistoryGap`] is included in `ServerHello`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "web/src/api/generated-terminal.ts")]
pub struct ResumeFrom {
    pub render_rev: Option<u32>,
    pub pty_seq: Option<u32>,
}

/// What the client can decode / display. The daemon validates the
/// intersection during handshake (e.g. no `Vt` in `render_encodings` →
/// [`ProtocolErrorCode::UnsupportedEncoding`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "web/src/api/generated-terminal.ts")]
pub struct ClientCapabilities {
    pub render_encodings: Vec<RenderEncoding>,
    pub supports_scrollback: bool,
    pub supports_sixel: bool,
    pub supports_images: bool,
    /// When true, this client is trusted to send [`ClientMsg::Input`]
    /// frames even when it is not the owner. Intended ONLY for
    /// kernel-originated clients connecting over a kernel-private unix
    /// domain socket (e.g. the task-dispatch platform's
    /// `DaemonClient::inject_stdin`). MUST be `false` for any client
    /// whose connection traverses an untrusted / network surface.
    ///
    /// Scope: only [`ClientMsg::Input`] is relaxed. [`ClientMsg::ResizeCommit`]
    /// and [`ClientMsg::Kill`] continue to require owner role even when
    /// this flag is set — the kernel relays input on behalf of an agent
    /// but is not itself the source of truth for viewport / lifecycle.
    ///
    /// Wire default is `false`; older peers that don't serialize this
    /// field decode as `false` thanks to `#[serde(default)]`. Any
    /// future relay surface that proxies a `ClientHello` over an
    /// untrusted hop (e.g. ws / network) MUST zero this field before
    /// forwarding to the daemon — see
    /// `crates/calm-server/src/ws/terminal.rs` for the existing
    /// kernel-side proxy that already forwards the field intact over
    /// the kernel-private socket.
    #[serde(default)]
    pub kernel_originated_input: bool,
}

/// Self-contained snapshot of the current render state. Sent inside
/// [`DaemonMsg::ServerHello`] and as a standalone frame when the daemon
/// decides the client needs a hard resync (typically because the requested
/// resume cursor fell off the history window).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "web/src/api/generated-terminal.ts")]
pub struct RenderSnapshot {
    pub render_rev: u32,
    pub pty_seq: u32,
    pub cols: u16,
    pub rows: u16,
    pub encoding: RenderEncoding,
    pub data: Vec<u8>,
    pub scrollback: Option<Vec<u8>>,
}

/// Incremental render-plane update. `prev_render_rev` lets the client
/// detect a gap (its last-known `render_rev` doesn't match) and request a
/// fresh [`RenderSnapshot`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "web/src/api/generated-terminal.ts")]
pub struct RenderPatch {
    pub render_rev: u32,
    pub prev_render_rev: u32,
    pub pty_seq: u32,
    pub encoding: RenderEncoding,
    pub data: Vec<u8>,
}

/// Communicates to the client that its requested resume cursor was older
/// than what the daemon still has buffered. `requires_snapshot` is always
/// `true` in this PR (we always re-send the snapshot rather than a partial
/// catch-up).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "web/src/api/generated-terminal.ts")]
pub struct HistoryGap {
    pub requested_render_rev: Option<u32>,
    pub requested_pty_seq: Option<u32>,
    pub earliest_render_rev: u32,
    pub earliest_pty_seq: u32,
    pub requires_snapshot: bool,
}

/// Daemon-side back-pressure policy hint. The first wave only encodes the
/// shape; nothing in this PR ever sends a `Backpressure` frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "web/src/api/generated-terminal.ts")]
pub enum BackpressurePolicy {
    LatestOnly,
    SnapshotRequired,
    Close,
}

/// Typed codes for [`DaemonMsg::ProtocolError`]. Distinct from a free-form
/// string so the client can branch on the error class (e.g. show
/// "upgrade required" for `UnsupportedVersion`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "web/src/api/generated-terminal.ts")]
pub enum ProtocolErrorCode {
    UnsupportedVersion,
    NotOwner,
    BadSequence,
    SnapshotMissing,
    UnsupportedEncoding,
    BadHandshake,
}

// ---- v2 ClientMsg / DaemonMsg ------------------------------------------

// NOTE (D7 / issue #5): the kernel's `Event` enum in `calm-server` drives
// its TS counterpart via `ts-rs` (see `web/src/api/generated-events.ts`).
// As of PR-3 of #44, `ClientMsg` / `DaemonMsg` + all helper types here
// follow the same pattern: `#[derive(TS)]` + `#[ts(export, export_to =
// "../../web/src/api/generated-terminal.ts")]`, regenerated by `cargo test
// export_bindings_` (driven by `npm run gen:api`). The hand-mirror
// `web/src/api/terminal-v2-handmirror.ts` has been retired in favor of
// the generated file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "web/src/api/generated-terminal.ts")]
pub enum ClientMsg {
    /// First frame on every connection. Carries the application protocol
    /// version, terminal identity, viewport / cell metrics, optional
    /// resume cursor, role hint, and capability set. The daemon validates
    /// (version match, terminal id match, encoding intersection non-empty)
    /// and responds with [`DaemonMsg::ServerHello`] or
    /// [`DaemonMsg::ProtocolError`].
    ClientHello {
        protocol_version: u16,
        terminal_id: String,
        #[ts(type = "string")]
        client_id: Uuid,
        desired_size: PtySize,
        cell_size: Option<CellSize>,
        initial_scrollback: InitialScrollback,
        resume_from: Option<ResumeFrom>,
        role_hint: Option<Role>,
        capabilities: ClientCapabilities,
    },
    /// Raw bytes from the client keyboard → PTY stdin. Owner-only;
    /// observers receive [`ProtocolErrorCode::NotOwner`].
    Input(Vec<u8>),
    /// Owner-driven viewport change. `epoch` is monotonic per-session and
    /// lets the daemon ignore stale resizes that arrive after a newer one
    /// has already been applied.
    ResizeCommit { epoch: u32, cols: u16, rows: u16 },
    /// Observer asking to be promoted to owner. Hostile takeover — the
    /// daemon transfers ownership immediately (no negotiation, no consent
    /// from the current owner) and broadcasts
    /// [`DaemonMsg::OwnerChanged`] to all connected clients.
    OwnerClaim,
    /// Owner relinquishing ownership. Subsequent input is rejected with
    /// [`ProtocolErrorCode::NotOwner`] until someone else claims.
    OwnerRelease,
    /// Client acknowledging it has rendered up through `render_rev`. Used
    /// (in a later PR) to decide back-pressure policy.
    RenderAck {
        render_rev: u32,
        pty_seq: Option<u32>,
    },
    /// Ask the daemon to terminate the child (SIGHUP). Owner-only in
    /// terminal mode; in chat mode this still routes through the
    /// chat-specific handler (closes the runner stdin so the SDK loop
    /// exits cleanly).
    Kill,
    /// Chat-mode user message. The daemon serializes this onto the Node
    /// runner's stdin as `{"kind":"user_message","content":"..."}`. The
    /// runner feeds it into `@anthropic-ai/claude-agent-sdk`'s `query()`.
    /// Ignored in terminal mode.
    ChatUserMessage { content: String },
    /// Interrupt an in-flight chat turn. Daemon writes
    /// `{"kind":"stop"}` to the runner's stdin; the runner calls the SDK
    /// interrupt API. Ignored in terminal mode.
    ChatStop,
    /// Resolve an `AskUserQuestion` posed by the SDK's `canUseTool`
    /// callback. Bridges WS frontend → daemon → runner stdin so the
    /// runner-side `canUseTool` promise can resolve and the agent loop
    /// proceeds. Daemon writes
    /// `{"kind":"answer_question","question_id":"<uuid>","answers": {...}}`
    /// to the runner. Ignored in terminal mode.
    AnswerQuestion {
        #[ts(type = "string")]
        question_id: Uuid,
        answers: HashMap<String, String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "web/src/api/generated-terminal.ts")]
pub enum DaemonMsg {
    /// Successful handshake response. Tells the client the daemon's
    /// negotiated protocol version, the session id (rolls on each daemon
    /// respawn), the role this client got assigned, the current owner (if
    /// any), and the PTY/render head/tail cursors. The `snapshot` field
    /// reproduces the current screen state; `history_gap` is set when the
    /// client's `resume_from` cursor was older than what we still have.
    ServerHello {
        protocol_version: u16,
        terminal_id: String,
        #[ts(type = "string")]
        session_id: Uuid,
        client_role: Role,
        #[ts(type = "string | null")]
        owner_client_id: Option<Uuid>,
        pty_size: PtySize,
        pty_seq_head: u32,
        pty_seq_tail: u32,
        render_rev: u32,
        snapshot: RenderSnapshot,
        history_gap: Option<HistoryGap>,
    },
    /// Standalone snapshot — sent when the daemon decided the client needs
    /// a hard re-sync mid-stream. PR-2 emits this on PTY resize and on
    /// broadcast lag (after a `SnapshotRequired`). Geometry-bound to the
    /// requesting client's `desired_size`; `data` is the server-rendered
    /// ANSI byte stream from `TerminalModel::snapshot_vt`.
    RenderSnapshot(RenderSnapshot),
    /// Incremental render-plane update. `data` is the raw PTY chunk that
    /// triggered the rev bump (`encoding = Vt`); xterm.js applies it
    /// directly to its own grid. Cell-grid diff encoding is a follow-up.
    RenderPatch(RenderPatch),
    /// Confirms an owner-issued [`ClientMsg::ResizeCommit`] took effect.
    /// `epoch` echoes the request so the owner can correlate.
    ResizeApplied {
        epoch: u32,
        pty_seq: u32,
        render_rev: u32,
        cols: u16,
        rows: u16,
    },
    /// Owner registry transition. Sent to every connected client whenever
    /// a successful [`ClientMsg::OwnerClaim`] / [`ClientMsg::OwnerRelease`]
    /// changes who holds owner. `None` means no one currently owns the
    /// session.
    OwnerChanged {
        #[ts(type = "string | null")]
        owner_client_id: Option<Uuid>,
    },
    /// Daemon is shedding load (lagged client, slow socket, ...). Wire
    /// shape only in this PR; nothing in the daemon emits it yet.
    Backpressure { policy: BackpressurePolicy },
    /// Daemon needs the client to discard its local state and accept a
    /// fresh [`Self::RenderSnapshot`]. Wire shape only in this PR.
    SnapshotRequired { reason: String },
    /// Terminal child exited; daemon is about to shut down. `pty_seq` and
    /// `render_rev` pin the cursor so the client can confirm it didn't
    /// miss any output between the last patch and the exit.
    TerminalExited {
        code: Option<i32>,
        pty_seq: u32,
        render_rev: u32,
    },
    /// Protocol-layer rejection. The shell closes the connection right
    /// after delivering this frame.
    ProtocolError {
        code: ProtocolErrorCode,
        message: String,
        expected_version: Option<u16>,
    },
    /// One-shot signal sent after the PTY child has reached
    /// input-readiness (e.g. shell prompt rendered, agent CLI listening
    /// on stdin). Fired at most once per session; clients can use this
    /// to know when injected stdin (e.g. auto-submit "\r") will be
    /// processed instead of swallowed by the shell startup. Carries the
    /// `pty_seq` and `render_rev` at the moment of detection so the
    /// client can correlate against its own cursor.
    ///
    /// Detection: emitted by the daemon shell after `render_rev` has
    /// remained stable for `CHILD_READY_QUIESCENT_MS` AND at least one
    /// PTY chunk has been observed. See
    /// [`crate::terminal_session::RenderPlane::detect_ready`] for the
    /// timing constants. Terminal mode only — chat mode never emits this
    /// (the chat runner has its own ready signal via its first stream
    /// event).
    ChildReady { pty_seq: u32, render_rev: u32 },
    /// Sent once right after the chat-mode handshake. `replay` is a list
    /// of already-serialized NeigeEvent JSON strings so a re-attaching
    /// client can rebuild conversation state without re-running the model.
    HelloChat { replay: Vec<String> },
    /// One serialized NeigeEvent JSON line emitted by the chat runner.
    ChatEvent { json: String },
    /// Chat-mode child exited; daemon is about to shut down. Terminal
    /// mode uses [`Self::TerminalExited`] instead.
    ChildExited { code: Option<i32> },
}

fn bincode_config() -> bincode::config::Configuration {
    bincode::config::standard()
}

pub async fn write_frame<T, W>(w: &mut W, msg: &T) -> Result<(), FrameError>
where
    T: Serialize,
    W: AsyncWrite + Unpin,
{
    let buf = bincode::serde::encode_to_vec(msg, bincode_config())?;
    // Cap on the *payload* length so the wire-side u32 can never overflow
    // and a malicious peer can't allocate-the-world on the read side.
    let len = u32::try_from(buf.len()).map_err(|_| FrameError::Oversize {
        len: u32::MAX,
        max: MAX_FRAME as u32,
    })?;
    if len as usize > MAX_FRAME {
        return Err(FrameError::Oversize {
            len,
            max: MAX_FRAME as u32,
        });
    }
    w.write_all(&FRAME_MAGIC).await?;
    w.write_all(&FRAME_VERSION.to_be_bytes()).await?;
    w.write_all(&len.to_be_bytes()).await?;
    w.write_all(&buf).await?;
    w.flush().await?;
    Ok(())
}

pub async fn read_frame<T, R>(r: &mut R) -> Result<T, FrameError>
where
    T: for<'de> Deserialize<'de>,
    R: AsyncRead + Unpin,
{
    // Magic — fails fast on wrong protocol / wrong daemon binary before we
    // try to interpret bincode bytes against a stale schema.
    let mut magic = [0u8; 4];
    r.read_exact(&mut magic).await?;
    if magic != FRAME_MAGIC {
        return Err(FrameError::BadMagic {
            got: magic,
            expected: FRAME_MAGIC,
        });
    }

    // Version — same role as magic but for incompatible schema bumps. We
    // only accept the exact current version; older/newer peers are expected
    // to be redeployed in lockstep with the kernel.
    let mut ver_buf = [0u8; 2];
    r.read_exact(&mut ver_buf).await?;
    let version = u16::from_be_bytes(ver_buf);
    if version != FRAME_VERSION {
        return Err(FrameError::UnsupportedFrameVersion {
            got: version,
            supported: FRAME_VERSION,
        });
    }

    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf);
    if len as usize > MAX_FRAME {
        return Err(FrameError::Oversize {
            len,
            max: MAX_FRAME as u32,
        });
    }
    let mut buf = vec![0u8; len as usize];
    r.read_exact(&mut buf).await?;
    let (msg, _) = bincode::serde::decode_from_slice(&buf, bincode_config())?;
    Ok(msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answer_question_bincode_roundtrip() {
        let qid = Uuid::parse_str("6b1f3a4d-2b5e-4d7e-9c1a-1b2c3d4e5f60").unwrap();
        let original = ClientMsg::AnswerQuestion {
            question_id: qid,
            answers: HashMap::from([("Which option?".to_string(), "the second one".to_string())]),
        };
        let encoded = bincode::serde::encode_to_vec(&original, bincode_config()).expect("encode");
        let (decoded, _): (ClientMsg, _) =
            bincode::serde::decode_from_slice(&encoded, bincode_config()).expect("decode");
        match decoded {
            ClientMsg::AnswerQuestion {
                question_id,
                answers,
            } => {
                assert_eq!(question_id, qid);
                assert_eq!(
                    answers.get("Which option?").map(String::as_str),
                    Some("the second one")
                );
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }
}

#[cfg(test)]
mod framing_tests {
    //! Cover the magic+version+length framing layer end-to-end against an
    //! in-memory `Vec<u8>` so we don't need real sockets. Each test drives
    //! `write_frame` / `read_frame` directly (or hand-crafts the bytes for
    //! the error paths).

    use super::*;
    use std::io::Cursor;

    /// Build a minimal `ClientHello` with default-everything for the
    /// framing tests that don't care about handshake semantics.
    fn sample_hello() -> ClientMsg {
        ClientMsg::ClientHello {
            protocol_version: PROTOCOL_VERSION,
            terminal_id: "t-1".to_string(),
            client_id: Uuid::nil(),
            desired_size: PtySize {
                cols: 80,
                rows: 24,
                pixel_width: None,
                pixel_height: None,
            },
            cell_size: None,
            initial_scrollback: InitialScrollback::None,
            resume_from: None,
            role_hint: None,
            capabilities: ClientCapabilities {
                render_encodings: vec![RenderEncoding::Vt],
                supports_scrollback: false,
                supports_sixel: false,
                supports_images: false,
                kernel_originated_input: false,
            },
        }
    }

    /// Bincode-encode a payload exactly the way `write_frame` does, so we
    /// can build a wire buffer with a *valid* payload but a *deliberately
    /// wrong* header (mismatched version, etc.).
    fn encode_payload<T: Serialize>(msg: &T) -> Vec<u8> {
        bincode::serde::encode_to_vec(msg, bincode_config()).expect("encode")
    }

    /// Hand-build a frame with arbitrary magic + version, used by the error
    /// path tests. Length and payload are always coherent.
    fn build_frame(magic: [u8; 4], version: u16, payload: &[u8]) -> Vec<u8> {
        let mut buf = Vec::with_capacity(10 + payload.len());
        buf.extend_from_slice(&magic);
        buf.extend_from_slice(&version.to_be_bytes());
        buf.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        buf.extend_from_slice(payload);
        buf
    }

    #[tokio::test]
    async fn round_trip_via_new_framing() {
        let original = sample_hello();
        let mut wire: Vec<u8> = Vec::new();
        write_frame(&mut wire, &original).await.expect("write");

        // Sanity-check: header is exactly magic+version+len, version is the
        // current FRAME_VERSION (i.e. 2 post-#44).
        assert_eq!(&wire[0..4], &FRAME_MAGIC);
        assert_eq!(
            u16::from_be_bytes([wire[4], wire[5]]),
            FRAME_VERSION,
            "version bytes"
        );

        let mut cursor = Cursor::new(wire);
        let decoded: ClientMsg = read_frame(&mut cursor).await.expect("read");
        assert_eq!(decoded, original);
    }

    #[tokio::test]
    async fn framing_version_2_round_trip() {
        // Locks the FRAME_VERSION=2 wire shape (issue #44). Hand-build the
        // header so the version byte is asserted independently from
        // FRAME_VERSION's value.
        let payload = encode_payload(&ClientMsg::Kill);
        let wire = build_frame(FRAME_MAGIC, 2, &payload);
        let mut cursor = Cursor::new(wire);
        let decoded: ClientMsg = read_frame(&mut cursor).await.expect("read v2");
        assert_eq!(decoded, ClientMsg::Kill);
    }

    #[tokio::test]
    async fn framing_version_1_payload_yields_unsupported_frame_version() {
        // Bytes pretending to be v1: same magic, version=1, valid bincode.
        // Post-#44 daemons MUST reject this — a v1 binary will never talk
        // to a v2 binary without an explicit upgrade.
        let payload = encode_payload(&ClientMsg::Kill);
        let wire = build_frame(FRAME_MAGIC, 1, &payload);
        let mut cursor = Cursor::new(wire);
        let err = read_frame::<ClientMsg, _>(&mut cursor)
            .await
            .expect_err("must reject v1 framing");
        match err {
            FrameError::UnsupportedFrameVersion { got, supported } => {
                assert_eq!(got, 1);
                assert_eq!(supported, FRAME_VERSION);
            }
            other => panic!("expected UnsupportedFrameVersion, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn bad_magic_is_typed_error() {
        let payload = encode_payload(&ClientMsg::Kill);
        let wire = build_frame(*b"XXXX", FRAME_VERSION, &payload);
        let mut cursor = Cursor::new(wire);
        let err = read_frame::<ClientMsg, _>(&mut cursor)
            .await
            .expect_err("must reject");
        match err {
            FrameError::BadMagic { got, expected } => {
                assert_eq!(&got, b"XXXX");
                assert_eq!(expected, FRAME_MAGIC);
            }
            other => panic!("expected BadMagic, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn bad_version_is_typed_error() {
        let payload = encode_payload(&ClientMsg::Kill);
        // Correct magic, version=FRAME_VERSION+1 (one ahead of current),
        // valid payload — the version mismatch fires before bincode parse.
        let wire = build_frame(FRAME_MAGIC, FRAME_VERSION + 1, &payload);
        let mut cursor = Cursor::new(wire);
        let err = read_frame::<ClientMsg, _>(&mut cursor)
            .await
            .expect_err("must reject");
        match err {
            FrameError::UnsupportedFrameVersion { got, supported } => {
                assert_eq!(got, FRAME_VERSION + 1);
                assert_eq!(supported, FRAME_VERSION);
            }
            other => panic!("expected UnsupportedFrameVersion, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn oversize_length_is_typed_error() {
        // Header advertises len = MAX_FRAME+1; we don't bother appending the
        // (non-existent) payload — the length check fires before we read it.
        let bogus_len = (MAX_FRAME as u32) + 1;
        let mut wire = Vec::with_capacity(10);
        wire.extend_from_slice(&FRAME_MAGIC);
        wire.extend_from_slice(&FRAME_VERSION.to_be_bytes());
        wire.extend_from_slice(&bogus_len.to_be_bytes());
        let mut cursor = Cursor::new(wire);
        let err = read_frame::<ClientMsg, _>(&mut cursor)
            .await
            .expect_err("must reject");
        match err {
            FrameError::Oversize { len, max } => {
                assert_eq!(len, bogus_len);
                assert_eq!(max, MAX_FRAME as u32);
            }
            other => panic!("expected Oversize, got {other:?}"),
        }
    }
}
