use crate::ids::TrackId;

/// Proof token that the per-track push lock for `track_id` is held; the owned guard can cross
/// `.await`. Proves only that the lock is held — not that replay events are complete or ordered.
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
