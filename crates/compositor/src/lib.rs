//! A headless Wayland compositor that renders each window on its own.
//!
//! [`start`] runs one Wayland display on a dedicated thread. Clients connect to
//! its socket and use `wl_shm` buffers (no `linux-dmabuf`, no GPU). Every
//! toplevel is configured maximized to one fixed output size and rendered in
//! software into its own frame, which holds the window's surface tree and its
//! popups and nothing else. The [`Compositor`] handle lists windows, watches or
//! captures their frames and injects pointer and keyboard input.
//!
//! The crate knows nothing about browsers, encoding or networks.
#![cfg(target_os = "linux")]

mod api;
mod input;
mod render;
mod server;
mod state;
mod watch;
mod windows;

pub use api::{
    Compositor, Config, DEFAULT_MAX_FPS, DEFAULT_SIZE, Error, Frame, InputEvent, Rect, Result,
    WindowEvent, WindowId, WindowInfo,
};
pub use server::{SOCKET_NAME, start};
pub use watch::FrameWatch;
