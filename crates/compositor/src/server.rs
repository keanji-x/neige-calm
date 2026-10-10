//! The compositor thread: Wayland display, listening socket, command channel
//! and the frame tick, all on one calloop event loop.

use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use smithay::backend::renderer::pixman::PixmanRenderer;
use smithay::desktop::PopupManager;
use smithay::input::SeatState;
use smithay::input::keyboard::XkbConfig;
use smithay::output::{Mode, Output, PhysicalProperties, Scale, Subpixel};
use smithay::reexports::calloop::channel::{self, Channel, Event as ChannelEvent};
use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::calloop::{EventLoop, Interest, LoopSignal, Mode as PollMode, PostAction};
use smithay::reexports::wayland_server::{Display, ListeningSocket};
use smithay::utils::{IsAlive, Transform};
use smithay::wayland::compositor::CompositorState;
use smithay::wayland::output::OutputManagerState;
use smithay::wayland::shell::xdg::XdgShellState;
use smithay::wayland::shm::ShmState;

use crate::api::{Command, Compositor, Config, Error, Frame, Result, WindowEvent, WindowId};
use crate::render::{render, whole};
use crate::state::{ClientState, State};
use crate::watch::FrameWatch;

/// File name of the Wayland socket inside `Config::run_dir`.
pub const SOCKET_NAME: &str = "wayland-0";
/// Frame callback period for windows nobody watches.
const IDLE_FRAME_PERIOD: Duration = Duration::from_secs(1);
const REPEAT_DELAY_MS: i32 = 600;
const REPEAT_RATE_HZ: i32 = 25;

struct Server {
    display: Display<State>,
    state: State,
    signal: LoopSignal,
}

/// Starts the compositor on its own thread. Returns once the socket accepts clients.
pub fn start(config: Config) -> Result<(Compositor, mpsc::Receiver<WindowEvent>)> {
    if config.size.0 == 0 || config.size.1 == 0 || config.max_fps == 0 {
        return Err(Error::Setup(
            "size and max_fps must be greater than zero".into(),
        ));
    }
    let socket = config.run_dir.join(SOCKET_NAME);
    let (commands, channel) = channel::channel::<Command>();
    let (events, event_receiver) = mpsc::channel();
    let (ready, started) = mpsc::sync_channel::<Result<()>>(1);
    let socket_path = socket.clone();
    let thread = std::thread::Builder::new()
        .name("compositor".into())
        .spawn(move || run(config, socket_path, channel, events, ready))
        .map_err(|e| Error::Setup(format!("spawn compositor thread: {e}")))?;
    match started.recv() {
        Ok(Ok(())) => Ok((Compositor::new(commands, socket, thread), event_receiver)),
        Ok(Err(e)) => {
            let _ = thread.join();
            Err(e)
        }
        Err(_) => {
            let _ = thread.join();
            Err(Error::Setup("compositor thread exited during setup".into()))
        }
    }
}

fn run(
    config: Config,
    socket: std::path::PathBuf,
    channel: Channel<Command>,
    events: mpsc::Sender<WindowEvent>,
    ready: mpsc::SyncSender<Result<()>>,
) {
    let mut event_loop = match EventLoop::<Server>::try_new() {
        Ok(event_loop) => event_loop,
        Err(e) => {
            let _ = ready.send(Err(Error::Setup(format!("event loop: {e}"))));
            return;
        }
    };
    let mut server = match setup(&config, &socket, &event_loop, channel, events) {
        Ok(server) => server,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let _ = ready.send(Ok(()));
    let result = event_loop.run(None, &mut server, |server| {
        if let Err(e) = server.display.flush_clients() {
            tracing::warn!(error = %e, "flushing Wayland clients failed");
        }
    });
    if let Err(e) = result {
        tracing::error!(error = %e, "compositor event loop failed");
    }
    // Close every watch so watchers see the window as gone, not frozen.
    for tracked in std::mem::take(&mut server.state.windows) {
        let _ = tracked.close();
    }
}

fn setup(
    config: &Config,
    socket: &std::path::Path,
    event_loop: &EventLoop<Server>,
    channel: Channel<Command>,
    events: mpsc::Sender<WindowEvent>,
) -> Result<Server> {
    let setup_err = |what: &str, e: &dyn std::fmt::Display| Error::Setup(format!("{what}: {e}"));
    let mut display: Display<State> = Display::new().map_err(|e| setup_err("display", &e))?;
    let dh = display.handle();
    let listener =
        ListeningSocket::bind_absolute(socket.to_path_buf()).map_err(|e| setup_err("bind", &e))?;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| setup_err("socket permissions", &e))?;

    let (width, height) = (config.size.0 as i32, config.size.1 as i32);
    let output = Output::new(
        "headless-1".into(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "compositor".into(),
            model: "headless".into(),
        },
    );
    let mode = Mode {
        size: (width, height).into(),
        refresh: (config.max_fps * 1000) as i32,
    };
    output.change_current_state(
        Some(mode),
        Some(Transform::Normal),
        Some(Scale::Integer(1)),
        Some((0, 0).into()),
    );
    output.set_preferred(mode);
    let _output_global = output.create_global::<State>(&dh);

    let mut seat_state = SeatState::new();
    let mut seat = seat_state.new_wl_seat(&dh, "seat0");
    seat.add_pointer();
    seat.add_keyboard(
        XkbConfig {
            rules: "evdev",
            model: "pc105",
            layout: "us",
            variant: "",
            options: None,
        },
        REPEAT_DELAY_MS,
        REPEAT_RATE_HZ,
    )
    .map_err(|e| setup_err("keyboard", &e))?;

    let state = State {
        compositor_state: CompositorState::new::<State>(&dh),
        xdg_shell_state: XdgShellState::new::<State>(&dh),
        shm_state: ShmState::new::<State>(&dh, vec![]),
        _output_manager_state: OutputManagerState::new_with_xdg_output::<State>(&dh),
        seat_state,
        seat,
        output,
        popups: PopupManager::default(),
        renderer: PixmanRenderer::new().map_err(|e| setup_err("pixman renderer", &e))?,
        windows: Vec::new(),
        events,
        size: config.size,
        started: Instant::now(),
        pointer_window: None,
        dh: dh.clone(),
    };

    let handle = event_loop.handle();
    let insert_err = |what: &str, e: &dyn std::fmt::Display| setup_err(what, e);
    handle
        .insert_source(
            Generic::new(listener, Interest::READ, PollMode::Level),
            |_, listener, server| {
                while let Some(stream) = listener.accept()? {
                    if let Err(e) = server
                        .state
                        .dh
                        .insert_client(stream, Arc::new(ClientState::default()))
                    {
                        tracing::warn!(error = %e, "inserting a Wayland client failed");
                    }
                }
                Ok(PostAction::Continue)
            },
        )
        .map_err(|e| insert_err("socket source", &e))?;
    let poll_fd = display
        .backend()
        .poll_fd()
        .try_clone_to_owned()
        .map_err(|e| setup_err("display fd", &e))?;
    handle
        .insert_source(
            Generic::new(poll_fd, Interest::READ, PollMode::Level),
            |_, _, server| {
                server.display.dispatch_clients(&mut server.state)?;
                Ok(PostAction::Continue)
            },
        )
        .map_err(|e| insert_err("display source", &e))?;
    handle
        .insert_source(channel, |event, _, server| match event {
            ChannelEvent::Msg(command) => server.state.handle(command),
            ChannelEvent::Closed => server.signal.stop(),
        })
        .map_err(|e| insert_err("command source", &e))?;
    let period = Duration::from_secs_f64(1.0 / f64::from(config.max_fps));
    handle
        .insert_source(Timer::from_duration(period), move |_, _, server| {
            server.state.tick();
            TimeoutAction::ToDuration(period)
        })
        .map_err(|e| insert_err("frame timer", &e))?;

    Ok(Server {
        display,
        state,
        signal: event_loop.get_signal(),
    })
}

impl State {
    fn handle(&mut self, command: Command) {
        // A dropped reply receiver only means the caller stopped waiting.
        match command {
            Command::Windows(reply) => {
                let windows = self
                    .windows
                    .iter()
                    .filter_map(|t| t.announced.clone())
                    .collect();
                let _ = reply.send(windows);
            }
            Command::Watch(id, reply) => {
                let _ = reply.send(self.watch(id));
            }
            Command::Capture(id, reply) => {
                let _ = reply.send(self.capture(id));
            }
            Command::Input(id, events, reply) => {
                let _ = reply.send(self.input(id, events));
            }
        }
    }

    fn watch(&mut self, id: WindowId) -> Result<FrameWatch> {
        let frame = self.capture(id)?;
        let (watch, shared) = FrameWatch::new(id);
        if let Some(upgraded) = shared.upgrade() {
            upgraded.publish(frame);
        }
        let tracked = self.tracked_mut(id).ok_or(Error::WindowGone(id))?;
        tracked.watchers.push(shared);
        // Start the fast pacing now rather than at the next idle callback.
        tracked.last_frame_callback = None;
        Ok(watch)
    }

    fn capture(&mut self, id: WindowId) -> Result<Frame> {
        let renderer = &mut self.renderer;
        let tracked = self
            .windows
            .iter_mut()
            .find(|t| t.id == id && t.is_mapped())
            .ok_or(Error::WindowGone(id))?;
        render(renderer, tracked).map(whole)
    }

    /// Renders watched windows that changed and sends frame callbacks: every tick
    /// for watched windows, once per [`IDLE_FRAME_PERIOD`] for the others.
    fn tick(&mut self) {
        self.popups.cleanup();
        let now = Instant::now();
        let time = self.started.elapsed();
        let renderer = &mut self.renderer;
        let output = &self.output;
        for tracked in &mut self.windows {
            if !tracked.window.alive() {
                continue;
            }
            let watched = tracked.is_watched();
            if watched
                && tracked.dirty
                && tracked.is_mapped()
                && let Err(e) = render(renderer, tracked)
            {
                tracing::debug!(window = %tracked.id, error = %e, "render skipped");
            }
            let due = watched
                || tracked
                    .last_frame_callback
                    .is_none_or(|last| now.duration_since(last) >= IDLE_FRAME_PERIOD);
            if due {
                tracked
                    .window
                    .send_frame(output, time, None, |_, _| Some(output.clone()));
                tracked.last_frame_callback = Some(now);
            }
        }
    }
}
