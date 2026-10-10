//! Frames in, encoded bytes out.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// A rectangle in frame pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// One window frame. Pixels are DRM `XRGB8888`: little-endian `0xXXRRGGBB`
/// words, so the bytes of a pixel are B, G, R, X. Row `y` starts at byte
/// `y * stride`.
///
/// The fields match the `compositor` crate's frame, so an adapter converts
/// between the two by moving the `Arc` without copying pixels.
#[derive(Debug, Clone)]
pub struct Frame {
    pub size: (u32, u32),
    pub stride: u32,
    pub xrgb8888: Arc<[u8]>,
    /// Regions that changed since the previous frame of the same feed.
    pub damage: Vec<Rect>,
}

/// How a frame payload is encoded. Each codec has a fixed wire id (see `PROTOCOL.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Codec {
    /// One baseline JFIF image per frame; every frame is a keyframe.
    Jpeg,
}

impl Codec {
    /// The codec's byte in the binary frame header.
    pub const fn wire_id(self) -> u8 {
        match self {
            Codec::Jpeg => 1,
        }
    }

    pub const fn from_wire_id(id: u8) -> Option<Codec> {
        match id {
            1 => Some(Codec::Jpeg),
            _ => None,
        }
    }
}

/// An encoded frame payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedFrame {
    /// True when the payload decodes without any earlier payload.
    pub keyframe: bool,
    pub data: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub enum EncodeError {
    #[error("frame is malformed: {0}")]
    BadFrame(String),
    #[error("encoder failed: {0}")]
    Encoder(String),
}

/// Turns frames into one codec's payloads. Version 1 has [`JpegEncoder`];
/// an H.264 encoder later implements the same trait.
pub trait FrameEncoder: Send {
    fn codec(&self) -> Codec;
    /// Encodes the whole frame. A session calls this only when its socket can
    /// take the next message.
    fn encode(&mut self, frame: &Frame) -> Result<EncodedFrame, EncodeError>;
}
