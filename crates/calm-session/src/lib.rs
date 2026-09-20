//! Wire protocol + framing helpers shared between the daemon and its clients.
//! Framing: `[magic (4) = b"NEIG"] [version (u16 BE)] [length (u32 BE)] [payload (bincode)]`.

pub mod control;
pub mod terminal_model;
mod terminal_owner;
pub mod terminal_session;

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use ts_rs::TS;
use uuid::Uuid;

/// Cap on a single frame. Anything larger is either a bug or hostile.
pub const MAX_FRAME: usize = 16 * 1024 * 1024;

/// Four-byte sentinel at the head of every frame, so the reader rejects wrong-protocol bytes before a bincode decode that would succeed-with-garbage.
pub const FRAME_MAGIC: [u8; 4] = *b"NEIG";

/// Bumped whenever the on-wire payload format changes incompatibly (any enum variant addition or reorder
/// moves the bincode discriminant space); moves in lockstep with `PROTOCOL_VERSION`.
pub const FRAME_VERSION: u16 = 4;

/// Application-layer protocol version carried in `ClientHello`/`ServerHello`; distinct from [`FRAME_VERSION`] because envelope and payload schema can move independently.
pub const PROTOCOL_VERSION: u16 = 4;

/// Supervisor control wire version; bumped when the ControlMsg / ControlReply shapes change incompatibly.
pub const SUPERVISOR_CONTROL_VERSION: u32 = 1;

/// Typed errors from the framing layer; the kernel↔daemon WS bridge matches on `BadMagic` / `UnsupportedFrameVersion` to close on version skew.
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

/// Per-connection role assigned by the daemon's `OwnerRegistry`: the first successful handshake becomes
/// `Owner`; later clients default to `Observer` and can promote themselves with `OwnerClaim` (hostile takeover).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
pub enum Role {
    Owner,
    Observer,
}

/// Render-plane payload encoding; only `Vt` (raw escape-sequence bytes) is advertised today.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
pub enum RenderEncoding {
    Vt,
}

/// How much pre-attach history the client wants in the `ServerHello` snapshot; `Lines(n)` is whole-chunk granularity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
pub enum InitialScrollback {
    None,
    All,
    Lines(u32),
}

/// PTY viewport dimensions plus an optional pixel-size hint, consulted only by programs that draw inline images.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct PtySize {
    pub cols: u16,
    pub rows: u16,
    pub pixel_width: Option<u16>,
    pub pixel_height: Option<u16>,
}

/// Default foreground / background RGB the daemon advertises to the PTY child in reply to OSC 10/11 queries.
/// Each channel is 8-bit; the daemon expands to xterm's 16-bit `rgb:RRRR/GGGG/BBBB` reply form (`c * 257`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct TerminalTheme {
    pub fg: (u8, u8, u8),
    pub bg: (u8, u8, u8),
}

/// Single cell's pixel footprint as the client measured it; sent only when it differs from the daemon-side default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct CellSize {
    pub width: u16,
    pub height: u16,
}

/// Reconnect cursor — the latest `render_rev` and/or `pty_seq` the client already has; if the daemon cannot replay from there a [`HistoryGap`] is included in `ServerHello`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ResumeFrom {
    pub render_rev: Option<u32>,
    pub pty_seq: Option<u32>,
}

/// What the client can decode / display; the daemon validates the intersection during handshake.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ClientCapabilities {
    pub render_encodings: Vec<RenderEncoding>,
    pub supports_scrollback: bool,
    pub supports_sixel: bool,
    pub supports_images: bool,
    /// When true, this client may send [`ClientMsg::Input`] even when not the owner (`ResizeCommit`/`Kill` still
    /// require owner). The daemon does NOT verify this flag: the WebSocket bridge is an untrusted network surface and
    /// MUST zero it on every ClientHello before forwarding, or a browser could write to another user's PTY as an Observer.
    #[serde(default)]
    pub kernel_originated_input: bool,
}

/// Self-contained snapshot of the current render state; sent inside `ServerHello` and standalone when the client needs a hard resync.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct RenderSnapshot {
    pub render_rev: u32,
    pub pty_seq: u32,
    pub cols: u16,
    pub rows: u16,
    pub encoding: RenderEncoding,
    pub data: Vec<u8>,
    pub scrollback: Option<Vec<u8>>,
}

/// Incremental render-plane update; `prev_render_rev` lets the client detect a gap and request a fresh [`RenderSnapshot`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct RenderPatch {
    pub render_rev: u32,
    pub prev_render_rev: u32,
    pub pty_seq: u32,
    pub encoding: RenderEncoding,
    pub data: Vec<u8>,
}

/// The client's requested resume cursor was older than what the daemon still has buffered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct HistoryGap {
    pub requested_render_rev: Option<u32>,
    pub requested_pty_seq: Option<u32>,
    pub earliest_render_rev: u32,
    pub earliest_pty_seq: u32,
    pub requires_snapshot: bool,
}

/// Daemon-side back-pressure policy hint; nothing sends a `Backpressure` frame yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
pub enum BackpressurePolicy {
    LatestOnly,
    SnapshotRequired,
    Close,
}

/// Typed codes for [`DaemonMsg::ProtocolError`], so the client can branch on the error class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
pub enum ProtocolErrorCode {
    UnsupportedVersion,
    NotOwner,
    BadSequence,
    SnapshotMissing,
    UnsupportedEncoding,
    BadHandshake,
}

// `ClientMsg` / `DaemonMsg` and the helper types drive their TS counterparts via `ts-rs`, regenerated by `cargo test export_bindings_` (`npm run gen:api`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub enum ClientMsg {
    /// First frame on every connection; the daemon validates it and responds with `ServerHello` or `ProtocolError`.
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
    /// Raw bytes from the client keyboard → PTY stdin. Owner-only. When `input_seq > 0` the daemon emits
    /// [`DaemonMsg::InputAck`] with the same seq after the PTY write returns; `0` means no ack requested.
    /// The daemon never validates ordering or uniqueness of `input_seq`.
    Input {
        data: Vec<u8>,
        #[serde(default)]
        input_seq: u64,
    },
    /// Owner-driven viewport change; `epoch` is monotonic per-session so the daemon can ignore stale resizes.
    ResizeCommit { epoch: u32, cols: u16, rows: u16 },
    /// Observer asking to be promoted to owner; the daemon transfers ownership immediately and broadcasts `OwnerChanged`.
    OwnerClaim,
    /// Owner relinquishing ownership; subsequent input is rejected with `NotOwner` until someone else claims.
    OwnerRelease,
    /// Client acknowledging it has rendered up through `render_rev`.
    RenderAck {
        render_rev: u32,
        pty_seq: Option<u32>,
    },
    /// Ask the terminal session to terminate the child (SIGHUP). Owner-only.
    Kill,
    /// Resolve an `AskUserQuestion` posed by the SDK's `canUseTool` callback; forwarded to the runner's stdin. Ignored in terminal mode.
    AnswerQuestion {
        #[ts(type = "string")]
        question_id: Uuid,
        answers: HashMap<String, String>,
    },
    /// Browser-driven mid-session theme toggle. The daemon updates the model's default colors and, when the child has
    /// DECSET 1004, writes `ESC[I` so a focus-aware TUI re-queries OSC 10/11. Owner-only, same gating as `Input`.
    TerminalThemeUpdate { fg: (u8, u8, u8), bg: (u8, u8, u8) },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub enum DaemonMsg {
    /// Successful handshake response. `is_child_ready` snapshots whether the one-shot `ChildReady` already fired,
    /// so late-joining clients know whether to wait for it; older peers decode it as `false` (wait-for-ready).
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
        #[serde(default)]
        is_child_ready: bool,
    },
    /// Standalone snapshot for a hard re-sync mid-stream (on PTY resize and broadcast lag); geometry-bound to the requesting client's `desired_size`.
    RenderSnapshot(RenderSnapshot),
    /// Incremental render-plane update; `data` is the raw PTY chunk that triggered the rev bump.
    RenderPatch(RenderPatch),
    /// Confirms an owner-issued [`ClientMsg::ResizeCommit`] took effect; `epoch` echoes the request.
    ResizeApplied {
        epoch: u32,
        pty_seq: u32,
        render_rev: u32,
        cols: u16,
        rows: u16,
    },
    /// Owner registry transition, sent to every connected client; `None` means no one currently owns the session.
    OwnerChanged {
        #[ts(type = "string | null")]
        owner_client_id: Option<Uuid>,
    },
    /// Daemon is shedding load; wire shape only, nothing emits it yet.
    Backpressure { policy: BackpressurePolicy },
    /// Daemon needs the client to discard its local state and accept a fresh snapshot; wire shape only.
    SnapshotRequired { reason: String },
    /// Terminal child exited; `pty_seq` and `render_rev` pin the cursor so the client can confirm it missed no output.
    TerminalExited {
        code: Option<i32>,
        pty_seq: u32,
        render_rev: u32,
    },
    /// Protocol-layer rejection; the shell closes the connection right after delivering this frame.
    ProtocolError {
        code: ProtocolErrorCode,
        message: String,
        expected_version: Option<u16>,
    },
    /// One-shot signal after the PTY child has reached input-readiness (emitted once `render_rev` has been stable for
    /// `CHILD_READY_QUIESCENT_MS` and at least one PTY chunk was seen); injected stdin before this may be swallowed.
    ChildReady { pty_seq: u32, render_rev: u32 },
    /// Per-connection (not broadcast) acknowledgement of an `Input` with `input_seq > 0`, emitted after the PTY master
    /// write returned; no ack is emitted if the write fails, so the client times out. Acks arrive in write-completion order.
    InputAck { input_seq: u64 },
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

    // Only the exact current version is accepted; peers are redeployed in lockstep with the kernel.
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
    //! Cover the magic+version+length framing layer against an in-memory `Vec<u8>`.

    use super::*;
    use std::io::Cursor;

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

    /// Encode a payload exactly as `write_frame` does, for building a valid payload under a deliberately wrong header.
    fn encode_payload<T: Serialize>(msg: &T) -> Vec<u8> {
        bincode::serde::encode_to_vec(msg, bincode_config()).expect("encode")
    }

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
    async fn framing_current_version_round_trip() {
        // Hand-build the header so the version byte is asserted independently from FRAME_VERSION's value.
        let payload = encode_payload(&ClientMsg::Kill);
        let wire = build_frame(FRAME_MAGIC, FRAME_VERSION, &payload);
        let mut cursor = Cursor::new(wire);
        let decoded: ClientMsg = read_frame(&mut cursor).await.expect("read");
        assert_eq!(decoded, ClientMsg::Kill);
    }

    #[tokio::test]
    async fn framing_older_version_yields_unsupported_frame_version() {
        // Same magic, version=FRAME_VERSION-1, valid bincode: peers move in lockstep, so this MUST be rejected.
        const _: () = assert!(FRAME_VERSION >= 1, "FRAME_VERSION sanity");
        let payload = encode_payload(&ClientMsg::Kill);
        let older = FRAME_VERSION - 1;
        let wire = build_frame(FRAME_MAGIC, older, &payload);
        let mut cursor = Cursor::new(wire);
        let err = read_frame::<ClientMsg, _>(&mut cursor)
            .await
            .expect_err("must reject older framing");
        match err {
            FrameError::UnsupportedFrameVersion { got, supported } => {
                assert_eq!(got, older);
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
        // No payload appended: the length check fires before it is read.
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
