use crate::ids::TrackId;

/// #480 §D — proof token that the per-track push lock for `track_id` is held.
/// `OwnedMutexGuard<()>` owns the `Arc<tokio::sync::Mutex<()>>` so the guard
/// is not tied to a `DashMap` entry borrow and can cross `.await`. Holding
/// across `.await` is intentional (catch-up replay) but can starve that
/// track; bound replay bodies.
///
/// **Invariant**: this guard proves the lock is held — NOT that replay
/// events are semantically complete or ordered (#480 §F4).
pub struct PushLockGuard {
    track_id: TrackId,
    _guard: tokio::sync::OwnedMutexGuard<()>,
}

impl PushLockGuard {
    pub(crate) fn new(track_id: TrackId, guard: tokio::sync::OwnedMutexGuard<()>) -> Self {
        Self {
            track_id,
            _guard: guard,
        }
    }

    pub fn track_id(&self) -> &TrackId {
        &self.track_id
    }
}
