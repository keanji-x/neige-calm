//! Wire protocol v1 (`PROTOCOL.md` is the normative description).
//!
//! Server → client: a text [`ServerMessage::Hello`], then binary frame messages
//! (a [`FrameHeader`] followed by the encoded payload), text
//! [`ServerMessage::Title`] when the title changes and a final text
//! [`ServerMessage::Closed`] when the window is gone. Client → server: text
//! [`ClientMessage`]s.

use serde::{Deserialize, Serialize};

use crate::frame::Codec;

/// The protocol version carried by `hello` and by every frame header.
pub const VERSION: u8 = 1;
/// Length of [`FrameHeader`] on the wire.
pub const HEADER_LEN: usize = 12;
const KEYFRAME: u8 = 0b0000_0001;

/// Server text messages, JSON with a `type` tag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    /// First message of every session, sent right before the first frame.
    Hello {
        version: u8,
        codec: Codec,
        width: u32,
        height: u32,
        title: String,
    },
    /// The window title changed.
    Title { title: String },
    /// The window is gone. The last message of the session; no frame follows.
    Closed,
}

impl ServerMessage {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("server messages always serialize")
    }
}

/// Client text messages, JSON with a `type` tag. Coordinates and wheel
/// distances are window pixels.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    Pointer {
        x: f64,
        y: f64,
    },
    /// `button` is `MouseEvent.button` (0 left, 1 middle, 2 right, 3 back, 4 forward).
    Button {
        button: i64,
        pressed: bool,
    },
    Wheel {
        dx: f64,
        dy: f64,
    },
    /// `code` is `KeyboardEvent.code`, a physical key name such as `"KeyA"`.
    Key {
        code: String,
        pressed: bool,
    },
}

/// The fixed header of a binary frame message. All integers are little-endian.
///
/// | offset | size | field |
/// |---|---|---|
/// | 0 | 1 | protocol version (1) |
/// | 1 | 1 | codec wire id (1 = JPEG) |
/// | 2 | 1 | flags: bit 0 keyframe; other bits 0 |
/// | 3 | 1 | reserved, 0 |
/// | 4 | 4 | frame width in window pixels (u32) |
/// | 8 | 4 | frame height in window pixels (u32) |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    pub codec: Codec,
    pub keyframe: bool,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HeaderError {
    #[error("frame message is {0} bytes, shorter than the {HEADER_LEN}-byte header")]
    Short(usize),
    #[error("frame message has protocol version {0}, expected {VERSION}")]
    Version(u8),
    #[error("frame message has unknown codec id {0}")]
    Codec(u8),
}

impl FrameHeader {
    /// A binary frame message: this header followed by `payload`.
    pub fn message(&self, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
        out.push(VERSION);
        out.push(self.codec.wire_id());
        out.push(if self.keyframe { KEYFRAME } else { 0 });
        out.push(0);
        out.extend_from_slice(&self.width.to_le_bytes());
        out.extend_from_slice(&self.height.to_le_bytes());
        out.extend_from_slice(payload);
        out
    }

    /// Splits a binary frame message into its header and payload.
    pub fn parse(message: &[u8]) -> Result<(FrameHeader, &[u8]), HeaderError> {
        let Some((head, payload)) = message.split_first_chunk::<HEADER_LEN>() else {
            return Err(HeaderError::Short(message.len()));
        };
        if head[0] != VERSION {
            return Err(HeaderError::Version(head[0]));
        }
        let codec = Codec::from_wire_id(head[1]).ok_or(HeaderError::Codec(head[1]))?;
        let word =
            |at: usize| u32::from_le_bytes([head[at], head[at + 1], head[at + 2], head[at + 3]]);
        Ok((
            FrameHeader {
                codec,
                keyframe: head[2] & KEYFRAME != 0,
                width: word(4),
                height: word(8),
            },
            payload,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_round_trips() {
        let header = FrameHeader {
            codec: Codec::Jpeg,
            keyframe: true,
            width: 1280,
            height: 800,
        };
        let message = header.message(b"\xff\xd8payload");
        assert_eq!(&message[..4], &[1, 1, 1, 0]);
        assert_eq!(&message[4..12], &[0x00, 0x05, 0, 0, 0x20, 0x03, 0, 0]);
        assert_eq!(
            FrameHeader::parse(&message),
            Ok((header, &b"\xff\xd8payload"[..]))
        );
    }

    #[test]
    fn header_parse_refuses_bad_input() {
        assert_eq!(FrameHeader::parse(&[1, 1, 1]), Err(HeaderError::Short(3)));
        let mut message = FrameHeader {
            codec: Codec::Jpeg,
            keyframe: false,
            width: 1,
            height: 1,
        }
        .message(&[]);
        message[0] = 2;
        assert_eq!(FrameHeader::parse(&message), Err(HeaderError::Version(2)));
        message[0] = 1;
        message[1] = 9;
        assert_eq!(FrameHeader::parse(&message), Err(HeaderError::Codec(9)));
    }

    #[test]
    fn server_messages_have_their_documented_json() {
        let hello = ServerMessage::Hello {
            version: VERSION,
            codec: Codec::Jpeg,
            width: 1280,
            height: 800,
            title: "t".into(),
        };
        assert_eq!(
            hello.to_json(),
            r#"{"type":"hello","version":1,"codec":"jpeg","width":1280,"height":800,"title":"t"}"#
        );
        assert_eq!(
            ServerMessage::Title { title: "x".into() }.to_json(),
            r#"{"type":"title","title":"x"}"#
        );
        assert_eq!(ServerMessage::Closed.to_json(), r#"{"type":"closed"}"#);
    }

    #[test]
    fn client_messages_parse_from_their_documented_json() {
        let parse = |s: &str| serde_json::from_str::<ClientMessage>(s).unwrap();
        assert_eq!(
            parse(r#"{"type":"pointer","x":1.5,"y":2}"#),
            ClientMessage::Pointer { x: 1.5, y: 2.0 }
        );
        assert_eq!(
            parse(r#"{"type":"button","button":2,"pressed":true}"#),
            ClientMessage::Button {
                button: 2,
                pressed: true
            }
        );
        assert_eq!(
            parse(r#"{"type":"wheel","dx":0,"dy":-120}"#),
            ClientMessage::Wheel {
                dx: 0.0,
                dy: -120.0
            }
        );
        assert_eq!(
            parse(r#"{"type":"key","code":"KeyA","pressed":false}"#),
            ClientMessage::Key {
                code: "KeyA".into(),
                pressed: false
            }
        );
        assert!(serde_json::from_str::<ClientMessage>(r#"{"type":"paste","text":"x"}"#).is_err());
    }
}
