//! Test fixture: a fake Chrome for the `desktop` tests.
//!
//! - CDP on fds 3/4 (NUL-delimited JSON): `Browser.getVersion` answers, and
//!   `Target.getTargets` lists no page; anything else is an error. EOF on
//!   fd 3 ends the process. With `FAKE_CHROME_NO_CDP` set (a test that
//!   spawns it as a plain Wayland client) it serves no CDP.
//! - Wayland (`WAYLAND_DISPLAY`/`XDG_RUNTIME_DIR`): one toplevel, "Fake
//!   Chrome", solid blue. A press of `r` turns it red at its next frame
//!   callback, as a real client draws only when the compositor asks.
//! - Under `$HOME`: writes `fake-chrome.pid`, appends `key <evdev> <0|1>` and
//!   `button <code> <0|1>` lines to `fake-chrome.log`, and closes its window
//!   and exits when a file named `close` appears. While `open-second` exists it
//!   maps a second toplevel, "Fake Popup", solid red, and `close-second`
//!   closes it again. While `hang-cdp` exists, `Target.getTargets` is never
//!   answered (a hung browser).
#[cfg(target_os = "linux")]
fn main() {
    fake::main();
}

#[cfg(not(target_os = "linux"))]
fn main() {}

#[cfg(target_os = "linux")]
mod fake {
    use std::fs::{File, OpenOptions};
    use std::io::{Read, Write};
    use std::os::fd::{AsFd, AsRawFd, FromRawFd};
    use std::path::PathBuf;

    use serde_json::{Value, json};
    use wayland_client::globals::{GlobalListContents, registry_queue_init};
    use wayland_client::protocol::{
        wl_buffer, wl_callback, wl_compositor, wl_keyboard, wl_pointer, wl_registry, wl_seat,
        wl_shm, wl_shm_pool, wl_surface,
    };
    use wayland_client::{Connection, Dispatch, QueueHandle, WEnum, delegate_noop};
    use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

    pub const BLUE: u32 = 0x0020_50a0;
    pub const RED: u32 = 0x00c0_3020;
    const KEY_R: u32 = 19;

    pub fn main() {
        let home = PathBuf::from(std::env::var_os("HOME").expect("HOME"));
        std::fs::write(home.join("fake-chrome.pid"), std::process::id().to_string()).unwrap();
        if std::env::var_os("FAKE_CHROME_NO_CDP").is_none() {
            let home = home.clone();
            std::thread::spawn(move || serve_cdp(&home));
        }
        let conn = Connection::connect_to_env().expect("connect to WAYLAND_DISPLAY");
        let (globals, mut queue) = registry_queue_init::<Client>(&conn).expect("registry");
        let qh = queue.handle();
        let compositor: wl_compositor::WlCompositor = globals.bind(&qh, 4..=4, ()).unwrap();
        let wm_base: xdg_wm_base::XdgWmBase = globals.bind(&qh, 2..=2, ()).unwrap();
        let shm: wl_shm::WlShm = globals.bind(&qh, 1..=1, ()).unwrap();
        let _seat: wl_seat::WlSeat = globals.bind(&qh, 5..=5, ()).unwrap();
        let mut client = Client {
            qh: qh.clone(),
            shm,
            windows: Vec::new(),
            redraw: None,
            log: OpenOptions::new()
                .create(true)
                .append(true)
                .open(home.join("fake-chrome.log"))
                .unwrap(),
        };
        client.open(&compositor, &wm_base, "Fake Chrome", BLUE);
        let (close, open_second, close_second) = (
            home.join("close"),
            home.join("open-second"),
            home.join("close-second"),
        );
        let mut second_opened = false;
        loop {
            queue.dispatch_pending(&mut client).unwrap();
            for window in &mut client.windows {
                if let Some(serial) = window.configure.take() {
                    window.xdg.ack_configure(serial);
                    if !window.mapped {
                        window.mapped = true;
                        let rgb = window.rgb;
                        window.draw(&client.shm, &client.qh, rgb);
                    }
                }
            }
            if open_second.exists() && !second_opened {
                second_opened = true;
                client.open(&compositor, &wm_base, "Fake Popup", RED);
            }
            if close_second.exists() && client.windows.len() == 2 {
                client.windows.pop().unwrap().destroy();
            }
            queue.flush().unwrap();
            if close.exists() {
                for window in client.windows.drain(..) {
                    window.destroy();
                }
                queue.flush().unwrap();
                std::process::exit(0);
            }
            if let Some(guard) = queue.prepare_read() {
                let mut fd = libc::pollfd {
                    fd: guard.connection_fd().as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                // SAFETY: one valid pollfd.
                let ready = unsafe { libc::poll(&mut fd, 1, 50) };
                if ready > 0 {
                    guard.read().unwrap();
                }
            }
        }
    }

    struct Client {
        qh: QueueHandle<Client>,
        shm: wl_shm::WlShm,
        /// The main window first; a second one while `open-second` exists.
        windows: Vec<Window>,
        /// The colour the main window draws at its next frame callback.
        redraw: Option<u32>,
        log: File,
    }

    struct Window {
        surface: wl_surface::WlSurface,
        xdg: xdg_surface::XdgSurface,
        toplevel: xdg_toplevel::XdgToplevel,
        rgb: u32,
        size: Option<(i32, i32)>,
        configure: Option<u32>,
        mapped: bool,
        buffers: Vec<(File, wl_shm_pool::WlShmPool, wl_buffer::WlBuffer)>,
    }

    impl Client {
        fn open(
            &mut self,
            compositor: &wl_compositor::WlCompositor,
            wm_base: &xdg_wm_base::XdgWmBase,
            title: &str,
            rgb: u32,
        ) {
            let index = self.windows.len();
            let surface = compositor.create_surface(&self.qh, ());
            let xdg = wm_base.get_xdg_surface(&surface, &self.qh, index);
            let toplevel = xdg.get_toplevel(&self.qh, index);
            toplevel.set_title(title.into());
            surface.commit();
            self.windows.push(Window {
                surface,
                xdg,
                toplevel,
                rgb,
                size: None,
                configure: None,
                mapped: false,
                buffers: Vec::new(),
            });
        }

        fn note(&mut self, line: String) {
            writeln!(self.log, "{line}").unwrap();
            self.log.flush().unwrap();
        }
    }

    impl Window {
        fn draw(&mut self, shm: &wl_shm::WlShm, qh: &QueueHandle<Client>, rgb: u32) {
            let (w, h) = self.size.unwrap_or((640, 480));
            let len = (w * h * 4) as usize;
            // SAFETY: memfd_create with a valid NUL-terminated name.
            let fd = unsafe { libc::memfd_create(c"fake-chrome".as_ptr(), libc::MFD_CLOEXEC) };
            assert!(fd >= 0, "memfd_create");
            // SAFETY: a fresh fd we own.
            let mut file = unsafe { File::from_raw_fd(fd) };
            let pixels: Vec<u8> = rgb.to_le_bytes().repeat((w * h) as usize);
            file.write_all(&pixels).unwrap();
            let pool = shm.create_pool(file.as_fd(), len as i32, qh, ());
            let buffer = pool.create_buffer(0, w, h, w * 4, wl_shm::Format::Xrgb8888, qh, ());
            self.surface.attach(Some(&buffer), 0, 0);
            self.surface.damage_buffer(0, 0, w, h);
            self.surface.commit();
            // Keep the last two buffers alive; older ones are released.
            self.buffers.push((file, pool, buffer));
            if self.buffers.len() > 2 {
                let (_, pool, buffer) = self.buffers.remove(0);
                buffer.destroy();
                pool.destroy();
            }
        }

        fn destroy(self) {
            self.toplevel.destroy();
            self.xdg.destroy();
            self.surface.destroy();
        }
    }

    /// Minimal CDP: fd 3 in, fd 4 out, NUL-delimited.
    fn serve_cdp(home: &std::path::Path) {
        // SAFETY: the launcher put the CDP pipe ends on fds 3 and 4.
        let mut commands = unsafe { File::from_raw_fd(3) };
        let mut responses = unsafe { File::from_raw_fd(4) };
        let mut pending = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let n = match commands.read(&mut chunk) {
                Ok(0) | Err(_) => std::process::exit(0),
                Ok(n) => n,
            };
            pending.extend_from_slice(&chunk[..n]);
            while let Some(end) = pending.iter().position(|&b| b == 0) {
                let message: Value = serde_json::from_slice(&pending[..end]).unwrap();
                pending.drain(..=end);
                let id = message["id"].clone();
                let reply = match message["method"].as_str().unwrap_or("") {
                    "Browser.getVersion" => {
                        json!({ "id": id, "result": { "product": "FakeChrome/1" } })
                    }
                    // A hung browser while `hang-cdp` exists: no answer at all.
                    "Target.getTargets" if home.join("hang-cdp").exists() => continue,
                    "Target.getTargets" => json!({ "id": id, "result": { "targetInfos": [] } }),
                    other => {
                        json!({ "id": id, "error": { "code": -32601, "message": format!("{other} not faked") } })
                    }
                };
                let mut bytes = serde_json::to_vec(&reply).unwrap();
                bytes.push(0);
                responses.write_all(&bytes).unwrap();
            }
        }
    }

    impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Client {
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

    impl Dispatch<xdg_wm_base::XdgWmBase, ()> for Client {
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

    impl Dispatch<xdg_surface::XdgSurface, usize> for Client {
        fn event(
            state: &mut Self,
            _: &xdg_surface::XdgSurface,
            event: xdg_surface::Event,
            index: &usize,
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            if let xdg_surface::Event::Configure { serial } = event
                && let Some(window) = state.windows.get_mut(*index)
            {
                window.configure = Some(serial);
            }
        }
    }

    impl Dispatch<xdg_toplevel::XdgToplevel, usize> for Client {
        fn event(
            state: &mut Self,
            _: &xdg_toplevel::XdgToplevel,
            event: xdg_toplevel::Event,
            index: &usize,
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            if let xdg_toplevel::Event::Configure { width, height, .. } = event
                && width > 0
                && height > 0
                && let Some(window) = state.windows.get_mut(*index)
            {
                window.size = Some((width, height));
            }
        }
    }

    impl Dispatch<wl_seat::WlSeat, ()> for Client {
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

    impl Dispatch<wl_keyboard::WlKeyboard, ()> for Client {
        fn event(
            state: &mut Self,
            _: &wl_keyboard::WlKeyboard,
            event: wl_keyboard::Event,
            _: &(),
            _: &Connection,
            qh: &QueueHandle<Self>,
        ) {
            if let wl_keyboard::Event::Key {
                key,
                state: WEnum::Value(key_state),
                ..
            } = event
            {
                let pressed = key_state == wl_keyboard::KeyState::Pressed;
                state.note(format!("key {key} {}", u8::from(pressed)));
                if key == KEY_R && pressed {
                    state.redraw = Some(RED);
                    let main = &state.windows[0].surface;
                    main.frame(qh, ());
                    main.commit();
                }
            }
        }
    }

    impl Dispatch<wl_pointer::WlPointer, ()> for Client {
        fn event(
            state: &mut Self,
            _: &wl_pointer::WlPointer,
            event: wl_pointer::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            if let wl_pointer::Event::Button {
                button,
                state: WEnum::Value(button_state),
                ..
            } = event
            {
                let pressed = button_state == wl_pointer::ButtonState::Pressed;
                state.note(format!("button {button} {}", u8::from(pressed)));
            }
        }
    }

    impl Dispatch<wl_callback::WlCallback, ()> for Client {
        fn event(
            state: &mut Self,
            _: &wl_callback::WlCallback,
            event: wl_callback::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            if let wl_callback::Event::Done { .. } = event
                && let Some(rgb) = state.redraw.take()
            {
                let (shm, qh) = (state.shm.clone(), state.qh.clone());
                state.windows[0].draw(&shm, &qh, rgb);
            }
        }
    }

    delegate_noop!(Client: ignore wl_compositor::WlCompositor);
    delegate_noop!(Client: ignore wl_surface::WlSurface);
    delegate_noop!(Client: ignore wl_shm::WlShm);
    delegate_noop!(Client: ignore wl_shm_pool::WlShmPool);
    delegate_noop!(Client: ignore wl_buffer::WlBuffer);
}
