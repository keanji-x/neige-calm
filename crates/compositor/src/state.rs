//! Compositor state and the smithay protocol handlers.

use std::sync::mpsc;
use std::time::Instant;

use smithay::backend::renderer::pixman::PixmanRenderer;
use smithay::backend::renderer::utils::on_commit_buffer_handler;
use smithay::desktop::{
    PopupGrab, PopupKeyboardGrab, PopupKind, PopupManager, PopupPointerGrab, PopupUngrabStrategy,
    Window, find_popup_root_surface, get_popup_toplevel_coords,
};
use smithay::input::pointer::{CursorImageStatus, Focus};
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::output::Output;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::backend::{ClientData, ClientId, DisconnectReason};
use smithay::reexports::wayland_server::protocol::{wl_buffer, wl_seat, wl_surface::WlSurface};
use smithay::reexports::wayland_server::{Client, DisplayHandle, Resource};
use smithay::utils::{Rectangle, Serial};
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::compositor::{
    CompositorClientState, CompositorHandler, CompositorState, get_parent, is_sync_subsurface,
};
use smithay::wayland::output::{OutputHandler, OutputManagerState};
use smithay::wayland::shell::xdg::{
    PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
};
use smithay::wayland::shm::{ShmHandler, ShmState};
use smithay::{
    delegate_compositor, delegate_output, delegate_seat, delegate_shm, delegate_xdg_shell,
};

use crate::api::{WindowEvent, WindowId};
use crate::windows::Tracked;

pub(crate) struct State {
    pub dh: DisplayHandle,
    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub shm_state: ShmState,
    pub _output_manager_state: OutputManagerState,
    pub seat_state: SeatState<State>,
    pub seat: Seat<State>,
    pub output: Output,
    pub popups: PopupManager,
    pub renderer: PixmanRenderer,
    /// Toplevels in creation order, mapped or not.
    pub windows: Vec<Tracked>,
    pub events: mpsc::Sender<WindowEvent>,
    pub size: (u32, u32),
    pub started: Instant,
    /// The window whose coordinate space the seat's pointer is in. It may name
    /// a closed window; ids are never reused, so that matches no live window.
    pub pointer_window: Option<WindowId>,
    /// The seat's latest explicit popup grab and the toplevel surface it is rooted in.
    pub popup_grab: Option<(WlSurface, PopupGrab<State>)>,
}

#[derive(Default)]
pub(crate) struct ClientState {
    pub compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}

impl State {
    pub fn now_ms(&self) -> u32 {
        self.started.elapsed().as_millis() as u32
    }

    pub fn emit(&self, event: WindowEvent) {
        // A dropped receiver only means nobody listens for events.
        let _ = self.events.send(event);
    }

    pub fn tracked(&self, id: WindowId) -> Option<&Tracked> {
        self.windows.iter().find(|w| w.id == id)
    }

    pub fn tracked_mut(&mut self, id: WindowId) -> Option<&mut Tracked> {
        self.windows.iter_mut().find(|w| w.id == id)
    }

    /// The window whose toplevel surface is `root`.
    fn position_by_root(&self, root: &WlSurface) -> Option<usize> {
        self.windows.iter().position(|w| w.has_root(root))
    }

    fn tracked_by_root(&mut self, root: &WlSurface) -> Option<&mut Tracked> {
        let index = self.position_by_root(root)?;
        Some(&mut self.windows[index])
    }

    /// Constrains a popup to its toplevel's window geometry, the only screen it has.
    fn unconstrain_popup(&self, popup: &PopupSurface) {
        let kind = PopupKind::Xdg(popup.clone());
        let Ok(root) = find_popup_root_surface(&kind) else {
            return;
        };
        let Some(window) = self.position_by_root(&root).map(|i| &self.windows[i]) else {
            return;
        };
        // Window coordinates put the window geometry at the origin; the positioner
        // wants the target relative to the popup's parent geometry.
        let mut target = Rectangle::from_size(window.window.geometry().size);
        target.loc -= get_popup_toplevel_coords(&kind);
        popup.with_pending_state(|state| {
            state.geometry = state.positioner.get_unconstrained_geometry(target);
        });
    }
}

impl BufferHandler for State {
    fn buffer_destroyed(&mut self, _buffer: &wl_buffer::WlBuffer) {}
}

impl ShmHandler for State {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl CompositorHandler for State {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client
            .get_data::<ClientState>()
            .expect("every client is inserted with ClientState")
            .compositor_state
    }

    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);
        self.output.enter(surface);
        self.popups.commit(surface);
        if let Some(PopupKind::Xdg(popup)) = self.popups.find_popup(surface)
            && !popup.is_initial_configure_sent()
        {
            // The initial configure of a popup cannot fail before it is sent.
            let _ = popup.send_configure();
        }
        if !is_sync_subsurface(surface) {
            let mut root = surface.clone();
            while let Some(parent) = get_parent(&root) {
                root = parent;
            }
            if let Some(tracked) = self.tracked_by_root(&root)
                && let Some(event) = tracked.root_committed()
            {
                self.emit(event);
            }
        }
        // Popups and subsurfaces belong to some window's frame: mark it changed.
        for tracked in &mut self.windows {
            let mut contains = false;
            tracked
                .window
                .with_surfaces(|s, _| contains |= s == surface);
            if contains {
                tracked.dirty = true;
            }
        }
    }
}

impl XdgShellHandler for State {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        let Some(pid) = surface
            .wl_surface()
            .client()
            .and_then(|client| client.get_credentials(&self.dh).ok())
            .map(|credentials| credentials.pid)
        else {
            // The client is already gone; its toplevel dies with it.
            return;
        };
        let (width, height) = self.size;
        surface.with_pending_state(|state| {
            state.size = Some((width as i32, height as i32).into());
            state.bounds = Some((width as i32, height as i32).into());
            state.states.set(xdg_toplevel::State::Maximized);
        });
        self.windows
            .push(Tracked::new(Window::new_wayland_window(surface), pid));
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        let Some(index) = self.position_by_root(surface.wl_surface()) else {
            return;
        };
        if let Some(event) = self.windows.remove(index).close() {
            self.emit(event);
        }
    }

    fn title_changed(&mut self, surface: ToplevelSurface) {
        if let Some(tracked) = self.tracked_by_root(surface.wl_surface())
            && let Some(event) = tracked.refresh_info()
        {
            self.emit(event);
        }
    }

    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        self.unconstrain_popup(&surface);
        // A dead parent means the popup is already gone.
        let _ = self.popups.track_popup(PopupKind::Xdg(surface));
    }

    fn popup_destroyed(&mut self, _surface: PopupSurface) {
        // The popup's parent is no longer reachable from it; any frame may have held it.
        for tracked in &mut self.windows {
            tracked.dirty = true;
        }
    }

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        surface.with_pending_state(|state| {
            state.positioner = positioner;
        });
        self.unconstrain_popup(&surface);
        surface.send_repositioned(token);
    }

    fn grab(&mut self, surface: PopupSurface, seat: wl_seat::WlSeat, serial: Serial) {
        let Some(seat) = Seat::<State>::from_resource(&seat) else {
            return;
        };
        let kind = PopupKind::Xdg(surface);
        let Ok(root) = find_popup_root_surface(&kind) else {
            return;
        };
        let Ok(mut grab) = self.popups.grab_popup(root.clone(), kind, &seat, serial) else {
            return;
        };
        if let Some(keyboard) = seat.get_keyboard() {
            if keyboard.is_grabbed()
                && !(keyboard.has_grab(serial)
                    || keyboard.has_grab(grab.previous_serial().unwrap_or(serial)))
            {
                grab.ungrab(PopupUngrabStrategy::All);
                return;
            }
            keyboard.set_focus(self, grab.current_grab(), serial);
            keyboard.set_grab(self, PopupKeyboardGrab::new(&grab), serial);
        }
        if let Some(pointer) = seat.get_pointer() {
            if pointer.is_grabbed()
                && !(pointer.has_grab(serial)
                    || pointer.has_grab(grab.previous_serial().unwrap_or_else(|| grab.serial())))
            {
                grab.ungrab(PopupUngrabStrategy::All);
                return;
            }
            pointer.set_grab(self, PopupPointerGrab::new(&grab), serial, Focus::Keep);
        }
        self.popup_grab = Some((root, grab));
    }
}

impl SeatHandler for State {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    fn focus_changed(&mut self, _seat: &Seat<Self>, _focused: Option<&WlSurface>) {}

    fn cursor_image(&mut self, _seat: &Seat<Self>, _image: CursorImageStatus) {}
}

impl OutputHandler for State {}

delegate_compositor!(State);
delegate_shm!(State);
delegate_xdg_shell!(State);
delegate_seat!(State);
delegate_output!(State);
