//! Test seams of the resolver (#1628 D2 seam list): a recorder for the
//! unstarted mode and the failpoints the lane / admission tests drive.
//! Compiled only for tests and the `fixtures` feature; production builds
//! carry none of it.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex as StdMutex, PoisonError};

use super::resolver::{Enqueue, Job};

/// `SeriesResolver::new_unstarted` records here instead of spawning lanes.
#[derive(Default)]
pub(super) struct Recorder {
    pub(super) enqueue_calls: AtomicUsize,
    pub(super) outcomes: StdMutex<Vec<Enqueue>>,
    pub(super) jobs: StdMutex<Vec<Job>>,
}

/// Deterministic seams for the acceptance tests. Each `*_once` flag fires
/// exactly once; the three holds block until released.
#[derive(Default)]
pub struct Failpoints {
    pub(super) fail_write_once: AtomicBool,
    pub(super) panic_drain_once: AtomicBool,
    pub(super) hold_in_rebuild: AtomicBool,
    pub(super) hold_in_precheck: AtomicBool,
    pub(super) hold_before_write: AtomicBool,
    pub(super) rebuild_released: StdMutex<bool>,
    pub(super) rebuild_condvar: std::sync::Condvar,
    pub(super) precheck_release: tokio::sync::Notify,
    pub(super) write_release: tokio::sync::Notify,
    pub(super) rebuild_entered: AtomicUsize,
    pub(super) precheck_entered: AtomicUsize,
    pub(super) write_held: AtomicUsize,
    pub(super) drain_spawned: AtomicUsize,
}

impl Failpoints {
    /// The next row write returns an error instead of writing.
    pub fn fail_write_once(&self) {
        self.fail_write_once.store(true, Ordering::SeqCst);
    }
    /// The next `resolve` panics before doing anything (kills a drain task).
    pub fn panic_drain_once(&self) {
        self.panic_drain_once.store(true, Ordering::SeqCst);
    }
    /// The next lane rebuild blocks (holding the `lanes` lock) until
    /// [`Self::release_rebuild`].
    pub fn hold_in_rebuild(&self) {
        *self
            .rebuild_released
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = false;
        self.hold_in_rebuild.store(true, Ordering::SeqCst);
    }
    pub fn release_rebuild(&self) {
        *self
            .rebuild_released
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = true;
        self.rebuild_condvar.notify_all();
    }
    /// The next `enqueue` parks after its in-flight insert, before the
    /// route pre-check, until [`Self::release_precheck`].
    pub fn hold_in_precheck(&self) {
        self.hold_in_precheck.store(true, Ordering::SeqCst);
    }
    pub fn release_precheck(&self) {
        self.precheck_release.notify_one();
    }
    /// The next row write parks after its row is built, before the DB is
    /// touched, until [`Self::release_write`]. One-shot: only the first
    /// `resolve` to reach the write is held; later ones write straight
    /// through. Tests wait on [`Self::write_held`] for the park, an event,
    /// instead of on a plugin delay.
    pub fn hold_before_write(&self) {
        self.hold_before_write.store(true, Ordering::SeqCst);
    }
    pub fn release_write(&self) {
        self.write_release.notify_one();
    }
    pub fn rebuild_entered(&self) -> usize {
        self.rebuild_entered.load(Ordering::SeqCst)
    }
    pub fn precheck_entered(&self) -> usize {
        self.precheck_entered.load(Ordering::SeqCst)
    }
    pub fn write_held(&self) -> usize {
        self.write_held.load(Ordering::SeqCst)
    }
    pub fn drain_spawned(&self) -> usize {
        self.drain_spawned.load(Ordering::SeqCst)
    }
}
