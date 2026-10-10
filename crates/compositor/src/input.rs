//! Input injection into one window through the single seat.
//!
//! Every window has its own coordinate space with its geometry at the origin.
//! The seat's pointer lives in the space of the window that last got input.

use smithay::backend::input::{Axis, AxisSource, ButtonState, KeyState};
use smithay::desktop::{PopupUngrabStrategy, WindowSurfaceType};
use smithay::input::keyboard::{FilterResult, Keycode};
use smithay::input::pointer::{AxisFrame, ButtonEvent, MotionEvent};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point, SERIAL_COUNTER};

use crate::api::{Error, InputEvent, Result, WindowId};
use crate::state::State;

/// evdev key codes are offset by 8 in XKB keycodes.
const XKB_EVDEV_OFFSET: u32 = 8;
/// Pixels per wheel notch; one notch is 120 in `axis_value120`.
const PIXELS_PER_NOTCH: f64 = 15.0;

impl State {
    pub fn input(&mut self, id: WindowId, events: Vec<InputEvent>) -> Result<()> {
        let tracked = self.tracked(id).filter(|t| t.is_mapped());
        let Some(tracked) = tracked else {
            return Err(Error::WindowGone(id));
        };
        let position = tracked.pointer;
        // With no grab left outside `id`, nothing in the batch can move the
        // keyboard to another window: only clients start grabs, and no client
        // request runs until the batch is done.
        self.end_popup_grab_outside(id);
        self.focus(id);
        if self.pointer_window != Some(id) {
            // Bring the pointer into this window's space before any button lands.
            self.pointer_window = Some(id);
            self.motion(id, position);
        }
        for event in events {
            match event {
                InputEvent::Motion { x, y } => self.motion(id, (x, y)),
                InputEvent::Button { code, pressed } => self.button(code, pressed),
                InputEvent::Axis { dx, dy } => self.axis(dx, dy),
                InputEvent::Key { evdev, pressed } => self.key(evdev, pressed),
            }
        }
        Ok(())
    }

    /// Ends the seat's popup grab when it is rooted in another window than `id`:
    /// dismisses its popups and releases the keyboard and pointer grabs, which
    /// would otherwise keep that window's popup focused.
    fn end_popup_grab_outside(&mut self, id: WindowId) {
        let Some((root, mut grab)) = self.popup_grab.take() else {
            return;
        };
        if grab.has_ended() {
            return;
        }
        if self.tracked(id).is_some_and(|t| t.has_root(&root)) {
            self.popup_grab = Some((root, grab));
            return;
        }
        grab.ungrab(PopupUngrabStrategy::All);
        // The keyboard first: releasing the pointer grab would otherwise hand
        // the keyboard back to the grab's root before `focus` moves it.
        if let Some(keyboard) = self.seat.get_keyboard()
            && keyboard.has_grab(grab.serial())
        {
            keyboard.unset_grab(self);
        }
        if let Some(pointer) = self.seat.get_pointer()
            && pointer.has_grab(grab.serial())
        {
            pointer.unset_grab(self, SERIAL_COUNTER.next_serial(), self.now_ms());
        }
    }

    /// Moves keyboard focus to the window unless it, or one of its popups, has it.
    fn focus(&mut self, id: WindowId) {
        let Some(root) = self
            .tracked(id)
            .and_then(|t| t.window.toplevel())
            .map(|t| t.wl_surface().clone())
        else {
            return;
        };
        let Some(keyboard) = self.seat.get_keyboard() else {
            return;
        };
        let focused_here = keyboard.current_focus().is_some_and(|focus| {
            let mut inside = false;
            if let Some(t) = self.tracked(id) {
                t.window.with_surfaces(|s, _| inside |= *s == focus);
            }
            inside
        });
        if focused_here {
            return;
        }
        for tracked in &self.windows {
            let active = tracked.id == id;
            if let Some(toplevel) = tracked.window.toplevel() {
                let changed = toplevel.with_pending_state(|state| {
                    let was = state.states.contains(xdg_toplevel::State::Activated);
                    if active {
                        state.states.set(xdg_toplevel::State::Activated);
                    } else {
                        state.states.unset(xdg_toplevel::State::Activated);
                    }
                    was != active
                });
                if changed && toplevel.is_initial_configure_sent() {
                    toplevel.send_pending_configure();
                }
            }
        }
        keyboard.set_focus(self, Some(root), SERIAL_COUNTER.next_serial());
    }

    fn motion(&mut self, id: WindowId, (x, y): (f64, f64)) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let Some(tracked) = self.tracked_mut(id) else {
            return;
        };
        tracked.pointer = (x, y);
        let focus = surface_under(tracked, (x, y));
        let event = MotionEvent {
            location: (x, y).into(),
            serial: SERIAL_COUNTER.next_serial(),
            time: self.now_ms(),
        };
        pointer.motion(self, focus, &event);
        pointer.frame(self);
    }

    fn button(&mut self, code: u32, pressed: bool) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let event = ButtonEvent {
            serial: SERIAL_COUNTER.next_serial(),
            time: self.now_ms(),
            button: code,
            state: if pressed {
                ButtonState::Pressed
            } else {
                ButtonState::Released
            },
        };
        pointer.button(self, &event);
        pointer.frame(self);
    }

    fn axis(&mut self, dx: f64, dy: f64) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let mut frame = AxisFrame::new(self.now_ms()).source(AxisSource::Wheel);
        for (axis, value) in [(Axis::Horizontal, dx), (Axis::Vertical, dy)] {
            if value != 0.0 {
                let notches = (value / PIXELS_PER_NOTCH * 120.0).round() as i32;
                frame = frame.value(axis, value).v120(axis, notches);
            }
        }
        pointer.axis(self, frame);
        pointer.frame(self);
    }

    fn key(&mut self, evdev: u32, pressed: bool) {
        let Some(keyboard) = self.seat.get_keyboard() else {
            return;
        };
        let state = if pressed {
            KeyState::Pressed
        } else {
            KeyState::Released
        };
        keyboard.input::<(), _>(
            self,
            Keycode::new(evdev + XKB_EVDEV_OFFSET),
            state,
            SERIAL_COUNTER.next_serial(),
            self.now_ms(),
            |_, _, _| FilterResult::Forward,
        );
    }
}

/// The surface under a point in window coordinates and that surface's origin
/// in the same coordinates.
fn surface_under(
    tracked: &crate::windows::Tracked,
    (x, y): (f64, f64),
) -> Option<(WlSurface, Point<f64, Logical>)> {
    let geometry = tracked.window.geometry();
    // `surface_under` works relative to the toplevel surface origin, which sits
    // at -geometry.loc in window coordinates.
    let point = Point::<f64, Logical>::from((x, y)) + geometry.loc.to_f64();
    tracked
        .window
        .surface_under(point, WindowSurfaceType::ALL)
        .map(|(surface, origin)| (surface, (origin - geometry.loc).to_f64()))
}
