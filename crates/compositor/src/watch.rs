//! Latest-frame slot shared between the compositor thread and one watcher.
//!
//! The compositor keeps only a `Weak` reference, so dropping the [`FrameWatch`]
//! is the unwatch: the window falls back to 1 Hz frame callbacks on the next tick.

use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use crate::api::{Error, Frame, Rect, Result, WindowId};

/// Above this many rectangles the accumulated damage collapses to the full frame.
const MAX_DAMAGE_RECTS: usize = 32;

/// A live subscription to one window's frames.
pub struct FrameWatch {
    window: WindowId,
    shared: Arc<Shared>,
}

pub(crate) struct Shared {
    slot: Mutex<Slot>,
    ready: Condvar,
}

struct Slot {
    pending: Option<Frame>,
    closed: bool,
}

impl FrameWatch {
    pub(crate) fn new(window: WindowId) -> (Self, Weak<Shared>) {
        let shared = Arc::new(Shared {
            slot: Mutex::new(Slot {
                pending: None,
                closed: false,
            }),
            ready: Condvar::new(),
        });
        let weak = Arc::downgrade(&shared);
        (Self { window, shared }, weak)
    }

    pub fn window(&self) -> WindowId {
        self.window
    }

    /// The newest frame not yet taken, with all damage since the last take.
    /// `Ok(None)` when nothing changed; an error once the window is gone.
    pub fn try_take(&self) -> Result<Option<Frame>> {
        let mut slot = self.shared.lock();
        take(&mut slot, self.window)
    }

    /// Like [`try_take`](Self::try_take), but waits up to `timeout` for a frame.
    pub fn take_timeout(&self, timeout: Duration) -> Result<Option<Frame>> {
        let deadline = Instant::now() + timeout;
        let mut slot = self.shared.lock();
        loop {
            if slot.closed || slot.pending.is_some() {
                return take(&mut slot, self.window);
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            slot = self
                .shared
                .ready
                .wait_timeout(slot, deadline - now)
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .0;
        }
    }
}

fn take(slot: &mut Slot, window: WindowId) -> Result<Option<Frame>> {
    if slot.closed {
        // A closed window never yields its last frame: unavailable is not stale.
        slot.pending = None;
        return Err(Error::WindowGone(window));
    }
    Ok(slot.pending.take())
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Slot> {
        self.slot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Replaces the pending frame, keeping the union of both damage lists.
    pub(crate) fn publish(&self, mut frame: Frame) {
        let mut slot = self.lock();
        if let Some(previous) = slot.pending.take() {
            if previous.size == frame.size {
                let mut damage = previous.damage;
                damage.extend(frame.damage.iter().copied());
                frame.damage = damage;
            } else {
                frame.damage = vec![full(frame.size)];
            }
        }
        if frame.damage.len() > MAX_DAMAGE_RECTS {
            frame.damage = vec![full(frame.size)];
        }
        slot.pending = Some(frame);
        self.ready.notify_all();
    }

    pub(crate) fn close(&self) {
        let mut slot = self.lock();
        slot.closed = true;
        slot.pending = None;
        self.ready.notify_all();
    }
}

pub(crate) fn full(size: (u32, u32)) -> Rect {
    Rect {
        x: 0,
        y: 0,
        width: size.0,
        height: size.1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(damage: Vec<Rect>) -> Frame {
        Frame {
            size: (4, 4),
            stride: 16,
            xrgb8888: Arc::from(vec![0u8; 64]),
            damage,
        }
    }

    fn rect(x: u32) -> Rect {
        Rect {
            x,
            y: 0,
            width: 1,
            height: 1,
        }
    }

    #[test]
    fn untaken_frames_merge_damage_and_a_take_clears_it() {
        let (watch, weak) = FrameWatch::new(WindowId(1));
        let shared = weak.upgrade().unwrap();
        shared.publish(frame(vec![rect(0)]));
        shared.publish(frame(vec![rect(1)]));
        let taken = watch.try_take().unwrap().unwrap();
        assert_eq!(taken.damage, vec![rect(0), rect(1)]);
        assert!(watch.try_take().unwrap().is_none());
    }

    #[test]
    fn a_closed_watch_errors_and_drops_its_pending_frame() {
        let (watch, weak) = FrameWatch::new(WindowId(7));
        let shared = weak.upgrade().unwrap();
        shared.publish(frame(vec![rect(0)]));
        shared.close();
        assert!(matches!(
            watch.try_take(),
            Err(Error::WindowGone(WindowId(7)))
        ));
        assert!(matches!(
            watch.take_timeout(Duration::from_millis(1)),
            Err(Error::WindowGone(_))
        ));
    }

    #[test]
    fn dropping_the_watch_is_the_unwatch() {
        let (watch, weak) = FrameWatch::new(WindowId(2));
        assert!(weak.upgrade().is_some());
        drop(watch);
        assert!(weak.upgrade().is_none());
    }
}
