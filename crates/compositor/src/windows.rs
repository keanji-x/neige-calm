//! Per-window bookkeeping: identity, mapping, announced info and watchers.

use std::sync::Weak;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use smithay::backend::renderer::utils::with_renderer_surface_state;
use smithay::desktop::Window;
use smithay::wayland::compositor::with_states;
use smithay::wayland::shell::xdg::XdgToplevelSurfaceData;

use crate::api::{WindowEvent, WindowId, WindowInfo};
use crate::render::Canvas;
use crate::watch::Shared;

static NEXT_WINDOW_ID: AtomicU64 = AtomicU64::new(1);

pub(crate) struct Tracked {
    pub id: WindowId,
    pub window: Window,
    pub pid: i32,
    /// The info last announced through `Opened`/`Changed`; `None` until the
    /// toplevel first commits a buffer.
    pub announced: Option<WindowInfo>,
    pub canvas: Option<Canvas>,
    /// Something in the window's surface tree or popups committed since the last render.
    pub dirty: bool,
    pub watchers: Vec<Weak<Shared>>,
    pub last_frame_callback: Option<Instant>,
    /// Last pointer position the seat had in this window's coordinates.
    pub pointer: (f64, f64),
}

impl Tracked {
    pub fn new(window: Window, pid: i32) -> Self {
        Self {
            id: WindowId(NEXT_WINDOW_ID.fetch_add(1, Ordering::Relaxed)),
            window,
            pid,
            announced: None,
            canvas: None,
            dirty: true,
            watchers: Vec::new(),
            last_frame_callback: None,
            pointer: (0.0, 0.0),
        }
    }

    pub fn is_mapped(&self) -> bool {
        self.announced.is_some()
    }

    /// Whether the toplevel surface currently holds a buffer.
    pub fn has_buffer(&self) -> bool {
        self.window.toplevel().is_some_and(|toplevel| {
            with_renderer_surface_state(toplevel.wl_surface(), |state| state.buffer().is_some())
                .unwrap_or(false)
        })
    }

    pub fn info(&self) -> WindowInfo {
        let title = self
            .window
            .toplevel()
            .map(|toplevel| {
                with_states(toplevel.wl_surface(), |states| {
                    states
                        .data_map
                        .get::<XdgToplevelSurfaceData>()
                        .and_then(|data| data.lock().ok().and_then(|d| d.title.clone()))
                        .unwrap_or_default()
                })
            })
            .unwrap_or_default();
        let size = self.window.geometry().size;
        WindowInfo {
            id: self.id,
            title,
            size: (size.w.max(0) as u32, size.h.max(0) as u32),
            pid: self.pid,
        }
    }

    /// Handles a commit of the toplevel surface (or an unsynchronized subsurface).
    pub fn root_committed(&mut self) -> Option<WindowEvent> {
        self.window.on_commit();
        self.dirty = true;
        let toplevel = self.window.toplevel()?;
        if !toplevel.is_initial_configure_sent() {
            toplevel.send_configure();
            return None;
        }
        if self.announced.is_none() {
            if !self.has_buffer() {
                return None;
            }
            let info = self.info();
            self.announced = Some(info.clone());
            return Some(WindowEvent::Opened(info));
        }
        self.refresh_info()
    }

    /// Announces a title or size change of a mapped window.
    pub fn refresh_info(&mut self) -> Option<WindowEvent> {
        let announced = self.announced.as_ref()?;
        let info = self.info();
        if *announced == info {
            return None;
        }
        self.announced = Some(info.clone());
        Some(WindowEvent::Changed(info))
    }

    /// Drops live watchers' references; returns true when at least one remains.
    pub fn is_watched(&mut self) -> bool {
        self.watchers.retain(|w| w.strong_count() > 0);
        !self.watchers.is_empty()
    }

    /// Ends every watch and reports the close if the window was ever mapped.
    pub fn close(self) -> Option<WindowEvent> {
        for watcher in self.watchers.iter().filter_map(Weak::upgrade) {
            watcher.close();
        }
        self.announced.map(|_| WindowEvent::Closed(self.id))
    }
}
