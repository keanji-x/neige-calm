//! Streams one window to one WebSocket viewer and carries that viewer's input back.
//!
//! A [`WindowSource`] supplies frames through a [`FrameFeed`] and accepts
//! [`StreamInput`]. A [`FrameEncoder`] (version 1: [`JpegEncoder`]) turns frames
//! into bytes. [`serve_viewer`] runs one viewer session over any WebSocket that
//! is a `Stream` + `Sink` of [`WsMessage`]; it is the only transport entry point.
//!
//! The wire protocol is documented in `PROTOCOL.md` next to this crate's
//! manifest and implemented in [`protocol`].
//!
//! The crate knows nothing about Wayland, the applications it streams, or Neige.

mod encode;
mod frame;
mod input;
pub mod protocol;
mod session;
mod socket;

pub use encode::{DEFAULT_JPEG_QUALITY, JpegEncoder};
pub use frame::{Codec, EncodeError, EncodedFrame, Frame, FrameEncoder, Rect};
pub use input::{StreamInput, evdev_button, evdev_key};
pub use session::{
    FrameFeed, SessionEnd, SessionSummary, SourceError, WindowSource, WsMessage, serve_viewer,
};
