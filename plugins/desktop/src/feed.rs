//! `window-stream`'s window source over one compositor window.
//!
//! The feed reads the compositor's `FrameWatch` slot itself each time it
//! wakes. Nothing sits between the two, so a frame the compositor dropped
//! when the window closed can never reach the viewer before `closed`.

use std::sync::Arc;
use std::time::Duration;

use compositor::{Compositor, FrameWatch, InputEvent, WindowId};
use futures_util::future::BoxFuture;
use window_stream::{Frame, FrameFeed, Rect, SourceError, StreamInput, WindowSource};

use crate::windows::Windows;

/// How long one blocking wait for the compositor's wakeup lasts; it bounds
/// how long a finished session's watch outlives it.
const WAKE_WAIT: Duration = Duration::from_millis(500);

/// One compositor window as a `window-stream` source.
pub struct CompositorWindow {
    compositor: Compositor,
    windows: Windows,
    id: WindowId,
    /// Set when the window is served as the main window of the app with this
    /// client pid: a newer window of that app (a login popup) ends the feed,
    /// so the viewer reconnects and gets the new main window.
    main_of: Option<u32>,
}

impl CompositorWindow {
    /// The window `id` for as long as it is open.
    pub fn new(compositor: Compositor, windows: Windows, id: WindowId) -> Self {
        Self {
            compositor,
            windows,
            id,
            main_of: None,
        }
    }

    /// The window `id` for as long as it is the main window of client `pid`.
    pub fn main_window(compositor: Compositor, windows: Windows, id: WindowId, pid: u32) -> Self {
        Self {
            main_of: Some(pid),
            ..Self::new(compositor, windows, id)
        }
    }
}

impl WindowSource for CompositorWindow {
    fn watch(&self) -> Result<Box<dyn FrameFeed>, SourceError> {
        let watch = self
            .compositor
            .watch(self.id)
            .map_err(|e| SourceError(e.to_string()))?;
        Ok(Box::new(WatchFeed {
            watch: Arc::new(watch),
            changes: self.windows.changes(),
            windows: self.windows.clone(),
            main_of: self.main_of,
        }))
    }

    fn input(&self, events: Vec<StreamInput>) -> Result<(), SourceError> {
        let events = events.into_iter().map(input_event).collect();
        self.compositor
            .input(self.id, events)
            .map_err(|e| SourceError(e.to_string()))
    }
}

/// Feeds the frames of one `FrameWatch`. The watch lives as long as the feed
/// (plus at most one [`WAKE_WAIT`]), and its end is the unwatch.
pub struct WatchFeed {
    watch: Arc<FrameWatch>,
    windows: Windows,
    changes: tokio::sync::watch::Receiver<u64>,
    main_of: Option<u32>,
}

impl WatchFeed {
    /// Whether the app has a newer main window than this one.
    fn superseded(&self) -> bool {
        self.main_of.is_some_and(|pid| {
            self.windows.main_window(pid).map(|w| w.id) != Some(self.watch.window())
        })
    }
}

impl FrameFeed for WatchFeed {
    fn next(&mut self) -> BoxFuture<'_, Option<Frame>> {
        Box::pin(async move {
            loop {
                self.changes.borrow_and_update();
                if self.superseded() {
                    return None;
                }
                // The slot itself, on every wakeup: the newest frame, or the
                // window's end, which drops any frame it still held.
                match self.watch.try_take() {
                    Ok(Some(frame)) => return Some(stream_frame(frame)),
                    Ok(None) => {}
                    Err(_) => return None,
                }
                // Wait for the compositor's wakeup off the async workers, or a
                // change of the window table. The wait takes nothing, so
                // dropping this future loses no frame.
                let watch = self.watch.clone();
                let woken = tokio::task::spawn_blocking(move || watch.wait(WAKE_WAIT));
                tokio::select! {
                    joined = woken => if joined.is_err() {
                        return None;
                    },
                    _ = self.changes.changed() => {}
                }
            }
        })
    }

    fn title(&self) -> String {
        self.windows
            .get(self.watch.window())
            .map(|w| w.title)
            .unwrap_or_default()
    }
}

/// Moves the pixels across without copying them.
fn stream_frame(frame: compositor::Frame) -> Frame {
    Frame {
        size: frame.size,
        stride: frame.stride,
        xrgb8888: frame.xrgb8888,
        damage: frame
            .damage
            .into_iter()
            .map(|r| Rect {
                x: r.x,
                y: r.y,
                width: r.width,
                height: r.height,
            })
            .collect(),
    }
}

fn input_event(input: StreamInput) -> InputEvent {
    match input {
        StreamInput::Pointer { x, y } => InputEvent::Motion { x, y },
        StreamInput::Button { code, pressed } => InputEvent::Button { code, pressed },
        StreamInput::Wheel { dx, dy } => InputEvent::Axis { dx, dy },
        StreamInput::Key { evdev, pressed } => InputEvent::Key { evdev, pressed },
    }
}

/// A window that is not there: its session sends `closed` at once.
pub struct NoWindow(pub String);

impl WindowSource for NoWindow {
    fn watch(&self) -> Result<Box<dyn FrameFeed>, SourceError> {
        Err(SourceError(self.0.clone()))
    }

    fn input(&self, _: Vec<StreamInput>) -> Result<(), SourceError> {
        Err(SourceError(self.0.clone()))
    }
}
