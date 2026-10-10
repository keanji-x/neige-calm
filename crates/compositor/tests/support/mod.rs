//! A minimal pure-Rust Wayland client for the compositor tests: one xdg
//! toplevel filled with a solid colour through `wl_shm`, an optional popup,
//! frame-callback counting and a record of the pointer and keyboard events it
//! receives.

use std::fs::File;
use std::io::Write;
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use compositor::{Compositor, Config, WindowEvent, WindowInfo};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{
    wl_buffer, wl_callback, wl_compositor, wl_keyboard, wl_pointer, wl_registry, wl_seat, wl_shm,
    wl_shm_pool, wl_surface,
};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::xdg::shell::client::{
    xdg_popup, xdg_positioner, xdg_surface, xdg_toplevel, xdg_wm_base,
};

pub const BTN_LEFT: u32 = 0x110;
pub const KEY_A: u32 = 30;

/// A private run directory under `TMPDIR` (kept short for `sun_path`).
pub fn run_dir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("cmp")
        .tempdir()
        .expect("create run dir")
}

pub fn start(
    run_dir: &Path,
    size: (u32, u32),
    max_fps: u32,
) -> (Compositor, std::sync::mpsc::Receiver<WindowEvent>) {
    compositor::start(Config {
        run_dir: run_dir.to_path_buf(),
        size,
        max_fps,
    })
    .expect("start compositor")
}

/// Waits for the next `Opened` event, skipping others.
pub fn wait_opened(events: &std::sync::mpsc::Receiver<WindowEvent>) -> WindowInfo {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match events.recv_timeout(left).expect("window opened in time") {
            WindowEvent::Opened(info) => return info,
            _ => continue,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Seen {
    PointerEnter { x: f64, y: f64 },
    PointerMotion { x: f64, y: f64 },
    Button { button: u32, pressed: bool },
    KeyboardEnter,
    KeyboardLeave,
    Key { key: u32, pressed: bool },
}

pub struct TestClient {
    conn: Connection,
    queue: EventQueue<ClientState>,
    pub state: ClientState,
}

pub struct ClientState {
    qh: QueueHandle<ClientState>,
    compositor: wl_compositor::WlCompositor,
    shm: wl_shm::WlShm,
    wm_base: xdg_wm_base::XdgWmBase,
    surface: Option<wl_surface::WlSurface>,
    xdg_surface: Option<xdg_surface::XdgSurface>,
    _toplevel: Option<xdg_toplevel::XdgToplevel>,
    pending_configure: Option<u32>,
    pub configured_size: Option<(i32, i32)>,
    popup: Option<PopupParts>,
    buffers: Vec<ShmBuffer>,
    pub seen: Vec<Seen>,
    pub frame_callbacks: u32,
    /// Request a new frame callback (and commit) every time one is done.
    pub keep_drawing: bool,
    _seat: wl_seat::WlSeat,
}

struct PopupParts {
    surface: wl_surface::WlSurface,
    xdg_surface: xdg_surface::XdgSurface,
    _popup: xdg_popup::XdgPopup,
    configured: Option<u32>,
}

struct ShmBuffer {
    _file: File,
    _pool: wl_shm_pool::WlShmPool,
    _buffer: wl_buffer::WlBuffer,
}

impl TestClient {
    pub fn connect(socket: &Path) -> Self {
        let stream = UnixStream::connect(socket).expect("connect to compositor");
        let conn = Connection::from_socket(stream).expect("wayland connection");
        let (globals, mut queue) = registry_queue_init::<ClientState>(&conn).expect("registry");
        let qh = queue.handle();
        let compositor = globals.bind(&qh, 4..=4, ()).expect("wl_compositor");
        let shm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
        let wm_base = globals.bind(&qh, 2..=2, ()).expect("xdg_wm_base");
        let seat = globals.bind(&qh, 5..=5, ()).expect("wl_seat");
        let mut state = ClientState {
            qh: qh.clone(),
            compositor,
            shm,
            wm_base,
            surface: None,
            xdg_surface: None,
            _toplevel: None,
            pending_configure: None,
            configured_size: None,
            popup: None,
            buffers: Vec::new(),
            seen: Vec::new(),
            frame_callbacks: 0,
            keep_drawing: false,
            _seat: seat,
        };
        queue.roundtrip(&mut state).expect("initial roundtrip");
        Self { conn, queue, state }
    }

    pub fn roundtrip(&mut self) {
        self.queue.roundtrip(&mut self.state).expect("roundtrip");
    }

    /// Dispatches events for `duration`.
    pub fn pump(&mut self, duration: Duration) {
        let deadline = Instant::now() + duration;
        while Instant::now() < deadline {
            self.roundtrip();
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Creates a toplevel, waits for its configure and commits a solid buffer.
    pub fn open_window(&mut self, title: &str, rgb: u32) {
        let qh = self.state.qh.clone();
        let surface = self.state.compositor.create_surface(&qh, ());
        let xdg_surface = self.state.wm_base.get_xdg_surface(&surface, &qh, ());
        let toplevel = xdg_surface.get_toplevel(&qh, ());
        toplevel.set_title(title.into());
        surface.commit();
        self.state.surface = Some(surface);
        self.state.xdg_surface = Some(xdg_surface);
        self.state._toplevel = Some(toplevel);
        self.wait_for(|s| s.pending_configure.is_some());
        let serial = self.state.pending_configure.take().unwrap();
        self.state
            .xdg_surface
            .as_ref()
            .unwrap()
            .ack_configure(serial);
        let (w, h) = self.state.configured_size.expect("configured size");
        self.draw(rgb, (w, h));
    }

    /// Fills the whole toplevel with a colour and commits.
    pub fn draw(&mut self, rgb: u32, size: (i32, i32)) {
        let buffer = self.state.solid_buffer(rgb, size);
        let surface = self.state.surface.as_ref().unwrap();
        surface.attach(Some(&buffer), 0, 0);
        surface.damage_buffer(0, 0, size.0, size.1);
        surface.commit();
        self.roundtrip();
    }

    /// Attaches a null buffer: the toplevel loses its content.
    pub fn unmap(&mut self) {
        let surface = self.state.surface.as_ref().unwrap();
        surface.attach(None, 0, 0);
        surface.commit();
        self.roundtrip();
    }

    /// Opens a popup at `rect` (x, y, w, h) relative to the toplevel, solid `rgb`.
    pub fn open_popup(&mut self, rgb: u32, rect: (i32, i32, i32, i32)) {
        let qh = self.state.qh.clone();
        let positioner = self.state.wm_base.create_positioner(&qh, ());
        positioner.set_size(rect.2, rect.3);
        positioner.set_anchor_rect(rect.0, rect.1, 1, 1);
        positioner.set_anchor(xdg_positioner::Anchor::TopLeft);
        positioner.set_gravity(xdg_positioner::Gravity::BottomRight);
        positioner.set_constraint_adjustment(
            xdg_positioner::ConstraintAdjustment::SlideX
                | xdg_positioner::ConstraintAdjustment::SlideY,
        );
        let surface = self.state.compositor.create_surface(&qh, ());
        let xdg_surface = self.state.wm_base.get_xdg_surface(&surface, &qh, ());
        let popup = xdg_surface.get_popup(self.state.xdg_surface.as_ref(), &positioner, &qh, ());
        surface.commit();
        self.state.popup = Some(PopupParts {
            surface,
            xdg_surface,
            _popup: popup,
            configured: None,
        });
        self.wait_for(|s| s.popup.as_ref().is_some_and(|p| p.configured.is_some()));
        let buffer = self.state.solid_buffer(rgb, (rect.2, rect.3));
        let parts = self.state.popup.as_ref().unwrap();
        parts.xdg_surface.ack_configure(parts.configured.unwrap());
        parts.surface.attach(Some(&buffer), 0, 0);
        parts.surface.damage_buffer(0, 0, rect.2, rect.3);
        parts.surface.commit();
        self.roundtrip();
    }

    /// Asks for frame callbacks continuously, committing on each one.
    pub fn start_frame_loop(&mut self) {
        self.state.keep_drawing = true;
        self.state.request_frame();
        self.roundtrip();
    }

    fn wait_for(&mut self, done: impl Fn(&ClientState) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done(&self.state) {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for the compositor"
            );
            self.roundtrip();
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    pub fn disconnect(self) {
        drop(self.queue);
        drop(self.conn);
    }
}

impl ClientState {
    fn solid_buffer(&mut self, rgb: u32, (w, h): (i32, i32)) -> wl_buffer::WlBuffer {
        let stride = w * 4;
        let len = (stride * h) as usize;
        let mut file = tempfile::tempfile().expect("shm file");
        let pixel = rgb.to_le_bytes();
        let row: Vec<u8> = pixel
            .iter()
            .copied()
            .cycle()
            .take(stride as usize)
            .collect();
        let mut bytes = Vec::with_capacity(len);
        for _ in 0..h {
            bytes.extend_from_slice(&row);
        }
        file.write_all(&bytes).expect("fill shm file");
        let pool = self.shm.create_pool(file.as_fd(), len as i32, &self.qh, ());
        let buffer = pool.create_buffer(0, w, h, stride, wl_shm::Format::Xrgb8888, &self.qh, ());
        self.buffers.push(ShmBuffer {
            _file: file,
            _pool: pool,
            _buffer: buffer.clone(),
        });
        buffer
    }

    fn request_frame(&mut self) {
        if let Some(surface) = &self.surface {
            surface.frame(&self.qh, ());
            surface.commit();
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for ClientState {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for ClientState {
    fn event(
        _: &mut Self,
        wm_base: &xdg_wm_base::XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
        }
    }
}

impl Dispatch<xdg_surface::XdgSurface, ()> for ClientState {
    fn event(
        state: &mut Self,
        surface: &xdg_surface::XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            match &mut state.popup {
                Some(popup) if &popup.xdg_surface == surface => popup.configured = Some(serial),
                _ => state.pending_configure = Some(serial),
            }
        }
    }
}

impl Dispatch<xdg_toplevel::XdgToplevel, ()> for ClientState {
    fn event(
        state: &mut Self,
        _: &xdg_toplevel::XdgToplevel,
        event: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_toplevel::Event::Configure { width, height, .. } = event
            && width > 0
            && height > 0
        {
            state.configured_size = Some((width, height));
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for ClientState {
    fn event(
        _: &mut Self,
        seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(caps),
        } = event
        {
            if caps.contains(wl_seat::Capability::Pointer) {
                seat.get_pointer(qh, ());
            }
            if caps.contains(wl_seat::Capability::Keyboard) {
                seat.get_keyboard(qh, ());
            }
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for ClientState {
    fn event(
        state: &mut Self,
        _: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let seen = match event {
            wl_pointer::Event::Enter {
                surface_x,
                surface_y,
                ..
            } => Seen::PointerEnter {
                x: surface_x,
                y: surface_y,
            },
            wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => Seen::PointerMotion {
                x: surface_x,
                y: surface_y,
            },
            wl_pointer::Event::Button {
                button,
                state: WEnum::Value(button_state),
                ..
            } => Seen::Button {
                button,
                pressed: button_state == wl_pointer::ButtonState::Pressed,
            },
            _ => return,
        };
        state.seen.push(seen);
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for ClientState {
    fn event(
        state: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let seen = match event {
            wl_keyboard::Event::Enter { .. } => Seen::KeyboardEnter,
            wl_keyboard::Event::Leave { .. } => Seen::KeyboardLeave,
            wl_keyboard::Event::Key {
                key,
                state: WEnum::Value(key_state),
                ..
            } => Seen::Key {
                key,
                pressed: key_state == wl_keyboard::KeyState::Pressed,
            },
            _ => return,
        };
        state.seen.push(seen);
    }
}

impl Dispatch<wl_callback::WlCallback, ()> for ClientState {
    fn event(
        state: &mut Self,
        _: &wl_callback::WlCallback,
        event: wl_callback::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { .. } = event {
            state.frame_callbacks += 1;
            if state.keep_drawing {
                state.request_frame();
            }
        }
    }
}

delegate_noop!(ClientState: ignore wl_compositor::WlCompositor);
delegate_noop!(ClientState: ignore wl_surface::WlSurface);
delegate_noop!(ClientState: ignore wl_shm::WlShm);
delegate_noop!(ClientState: ignore wl_shm_pool::WlShmPool);
delegate_noop!(ClientState: ignore wl_buffer::WlBuffer);
delegate_noop!(ClientState: ignore xdg_positioner::XdgPositioner);
delegate_noop!(ClientState: ignore xdg_popup::XdgPopup);
