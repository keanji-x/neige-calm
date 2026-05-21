//! Wire protocol + framing helpers shared between the daemon and its clients.
//!
//! A client (neige-server, or the standalone test CLI) opens the daemon's
//! Unix socket, sends `ClientMsg::Attach {cols, rows}` as the first frame,
//! then reads a `DaemonMsg::Hello { replay }` whose `replay` is the recent
//! window of raw PTY bytes (kept in a server-side ring buffer). The client
//! feeds those bytes straight into its own VT emulator (e.g. xterm.js) to
//! repaint the screen. From there it's a duplex stream of `ClientMsg::Stdin`
//! / `ClientMsg::Resize` upstream and `DaemonMsg::Stdout` /
//! `DaemonMsg::ChildExited` downstream.
//!
//! Framing: `[magic (4) = b"NEIG"] [version (u16 BE) = 1] [length (u32 BE)]
//! [payload (bincode)]`.
//!
//! Magic + version were added in issue #45 so a daemon binary built against
//! an incompatible `ClientMsg`/`DaemonMsg` enum (variant reorder, new variant
//! inserted, etc.) fails fast at the read site with a typed [`FrameError`]
//! instead of silently misinterpreting the bincode discriminants that follow.

pub mod stream_json;

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
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
pub const FRAME_VERSION: u16 = 1;

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

// NOTE (D7 / issue #5): the kernel's `Event` enum in `calm-server` now drives
// its TS counterpart via `ts-rs` (see `web/src/api/generated-events.ts`). The
// same treatment could be applied to `ClientMsg` / `DaemonMsg` below to retire
// the hand-mirror in `web/src/cards/builtins/terminal/*` — out of scope for
// this PR but the path is clear: add `ts-rs = "12"` to this crate, derive `TS`,
// add a second `export_to` target in the npm `gen:api` step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClientMsg {
    /// First message on every connection. Tells the daemon the client's
    /// viewport so the PTY can be sized for it on first attach (latest-
    /// attach-wins for subsequent clients).
    Attach { cols: u16, rows: u16 },
    /// Raw bytes from the client's keyboard → PTY stdin.
    Stdin(Vec<u8>),
    /// Viewport change after attach. Also latest-wins.
    Resize { cols: u16, rows: u16 },
    /// Ask the daemon to terminate the child (SIGHUP). The daemon's
    /// child-waiter then broadcasts ChildExited and the daemon shuts
    /// itself down.
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
        question_id: Uuid,
        answers: HashMap<String, String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DaemonMsg {
    /// Sent once right after `Attach` in terminal mode. `replay` is the
    /// recent PTY byte window kept in the daemon's ring buffer — feed it
    /// straight into the client's terminal emulator and it reproduces the
    /// current screen.
    Hello { replay: Vec<u8> },
    /// Sent once right after `Attach` in chat mode. `replay` is a list of
    /// already-serialized NeigeEvent JSON strings (one per buffered event)
    /// so a re-attaching client can rebuild conversation state without
    /// re-running the model.
    HelloChat { replay: Vec<String> },
    /// Live PTY output, forwarded as it arrives.
    Stdout(Vec<u8>),
    /// One serialized NeigeEvent JSON line, emitted by the daemon for each
    /// stream-json line that produced a unified event. Chat mode only.
    ChatEvent { json: String },
    /// The child program exited. Daemon will shut down right after this.
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
        let original = ClientMsg::Attach {
            cols: 132,
            rows: 50,
        };
        let mut wire: Vec<u8> = Vec::new();
        write_frame(&mut wire, &original).await.expect("write");

        // Sanity-check: header is exactly magic+version+len.
        assert_eq!(&wire[0..4], &FRAME_MAGIC);
        assert_eq!(
            u16::from_be_bytes([wire[4], wire[5]]),
            FRAME_VERSION,
            "version bytes"
        );

        let mut cursor = Cursor::new(wire);
        let decoded: ClientMsg = read_frame(&mut cursor).await.expect("read");
        match decoded {
            ClientMsg::Attach { cols, rows } => {
                assert_eq!(cols, 132);
                assert_eq!(rows, 50);
            }
            other => panic!("unexpected variant: {other:?}"),
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
        // Correct magic, version=2 (one ahead of current), valid payload.
        let wire = build_frame(FRAME_MAGIC, 2, &payload);
        let mut cursor = Cursor::new(wire);
        let err = read_frame::<ClientMsg, _>(&mut cursor)
            .await
            .expect_err("must reject");
        match err {
            FrameError::UnsupportedFrameVersion { got, supported } => {
                assert_eq!(got, 2);
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
