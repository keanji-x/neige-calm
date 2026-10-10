//! Public types and the thread-safe handle. Every handle call is a message to
//! the compositor thread; nothing here touches Wayland state directly.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::thread::JoinHandle;

use smithay::reexports::calloop::channel::Sender;

use crate::watch::FrameWatch;

/// Output size used when the caller has no reason to pick another.
pub const DEFAULT_SIZE: (u32, u32) = (1280, 800);
/// Frame rate cap for watched windows used when the caller has no reason to pick another.
pub const DEFAULT_MAX_FPS: u32 = 15;

/// Compositor configuration. All fields are required.
#[derive(Debug, Clone)]
pub struct Config {
    /// Directory that holds the Wayland socket. It must exist and should be private (0700).
    pub run_dir: PathBuf,
    /// Fixed output size in pixels; every toplevel is configured maximized to it.
    pub size: (u32, u32),
    /// Frame callback rate for windows that have at least one [`FrameWatch`].
    pub max_fps: u32,
}

/// Process-unique window identity. Never reused within one process.
///
/// An id names one mapping of a toplevel. A toplevel that commits a null
/// buffer is unmapped and closed; if it maps again it is a new window with a
/// new id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WindowId(pub u64);

impl fmt::Display for WindowId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A mapped toplevel window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowInfo {
    pub id: WindowId,
    pub title: String,
    /// Window geometry size in pixels (the size of its frames).
    pub size: (u32, u32),
    /// Process id of the Wayland client that owns the window (SO_PEERCRED).
    pub pid: i32,
}

/// Window lifecycle notifications, in the order the compositor observed them.
///
/// They arrive on the receiver [`start`](crate::start) returns. That channel is
/// unbounded: a caller must keep draining it, or drop the receiver to stop
/// listening, or the events pile up in memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowEvent {
    /// A toplevel committed its first buffer, or its first one after an unmap.
    Opened(WindowInfo),
    /// Title or size changed.
    Changed(WindowInfo),
    /// The toplevel was destroyed or unmapped, or its client disconnected. The
    /// id never comes back.
    Closed(WindowId),
}

/// Input for one window. Coordinates are window pixels; codes are evdev codes
/// (`BTN_LEFT` = 0x110, `KEY_A` = 30).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InputEvent {
    Motion { x: f64, y: f64 },
    Button { code: u32, pressed: bool },
    Axis { dx: f64, dy: f64 },
    Key { evdev: u32, pressed: bool },
}

/// A rectangle in frame pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// One rendered window frame. Pixels are DRM `XRGB8888`: little-endian
/// `0xXXRRGGBB` words, so bytes are B, G, R, X.
#[derive(Debug, Clone)]
pub struct Frame {
    pub size: (u32, u32),
    pub stride: u32,
    pub xrgb8888: Arc<[u8]>,
    /// Regions that changed since the previous frame handed to the same receiver.
    pub damage: Vec<Rect>,
}

impl Frame {
    /// The pixel at (x, y) as `0x00RRGGBB`.
    ///
    /// # Panics
    ///
    /// When (x, y) lies outside the frame.
    pub fn pixel(&self, x: u32, y: u32) -> u32 {
        assert!(
            x < self.size.0 && y < self.size.1,
            "pixel ({x}, {y}) is outside the {:?} frame",
            self.size
        );
        let at = (y * self.stride + x * 4) as usize;
        let px = &self.xrgb8888[at..at + 4];
        u32::from_le_bytes([px[0], px[1], px[2], 0])
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("compositor setup failed: {0}")]
    Setup(String),
    #[error("window {0} does not exist or was closed")]
    WindowGone(WindowId),
    #[error("window {0} has no committed buffer")]
    NoBuffer(WindowId),
    #[error("rendering window {0} failed: {1}")]
    Render(WindowId, String),
    #[error("the compositor thread has stopped")]
    Stopped,
}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) type Reply<T> = SyncSender<T>;

pub(crate) enum Command {
    Windows(Reply<Vec<WindowInfo>>),
    Watch(WindowId, Reply<Result<FrameWatch>>),
    Capture(WindowId, Reply<Result<Frame>>),
    Input(WindowId, Vec<InputEvent>, Reply<Result<()>>),
}

/// Handle to a running compositor. Clone + Send; the compositor stops when the
/// last clone is dropped.
#[derive(Clone)]
pub struct Compositor {
    inner: Arc<Handle>,
}

// Field order matters: `commands` drops first, which closes the channel and
// stops the loop, then `_thread` joins it.
struct Handle {
    commands: Sender<Command>,
    socket: PathBuf,
    _thread: Joiner,
}

struct Joiner(Option<JoinHandle<()>>);

impl Drop for Joiner {
    fn drop(&mut self) {
        if let Some(thread) = self.0.take() {
            let _ = thread.join();
        }
    }
}

impl Compositor {
    pub(crate) fn new(commands: Sender<Command>, socket: PathBuf, thread: JoinHandle<()>) -> Self {
        Self {
            inner: Arc::new(Handle {
                commands,
                socket,
                _thread: Joiner(Some(thread)),
            }),
        }
    }

    /// Absolute path of the Wayland socket. Clients use it as `WAYLAND_DISPLAY`
    /// (absolute), or its parent as `XDG_RUNTIME_DIR` and its file name as `WAYLAND_DISPLAY`.
    pub fn wayland_socket(&self) -> &Path {
        &self.inner.socket
    }

    /// Mapped toplevel windows, oldest first.
    pub fn windows(&self) -> Result<Vec<WindowInfo>> {
        self.call(Command::Windows)
    }

    /// Starts watching a window. The watch holds its latest frame plus the damage
    /// accumulated since the last take; while it lives the window gets frame
    /// callbacks at up to `max_fps`.
    pub fn watch(&self, id: WindowId) -> Result<FrameWatch> {
        self.call(|reply| Command::Watch(id, reply))?
    }

    /// Renders and returns one full frame of the window now.
    pub fn capture(&self, id: WindowId) -> Result<Frame> {
        self.call(|reply| Command::Capture(id, reply))?
    }

    /// Delivers input to the window and gives it keyboard focus. A popup grab
    /// held by another window ends first, and that window's grabbing popups
    /// are dismissed.
    pub fn input(&self, id: WindowId, events: Vec<InputEvent>) -> Result<()> {
        self.call(|reply| Command::Input(id, events, reply))?
    }

    fn call<T>(&self, command: impl FnOnce(Reply<T>) -> Command) -> Result<T> {
        let (reply, answer): (Reply<T>, Receiver<T>) = sync_channel(1);
        self.inner
            .commands
            .send(command(reply))
            .map_err(|_| Error::Stopped)?;
        answer.recv().map_err(|_| Error::Stopped)
    }
}
