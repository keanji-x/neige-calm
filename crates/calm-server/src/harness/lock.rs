use crate::ids::WaveId;

/// #480 §D — proof token that the per-wave push lock for `wave_id` is held.
/// `OwnedMutexGuard<()>` owns the `Arc<tokio::sync::Mutex<()>>` so the guard
/// is not tied to a `DashMap` entry borrow and can cross `.await`. Holding
/// across `.await` is intentional (catch-up replay) but can starve that
/// wave; bound replay bodies.
///
/// **Invariant**: this guard proves the lock is held — NOT that replay
/// events are semantically complete or ordered (#480 §F4).
pub struct PushLockGuard {
    wave_id: WaveId,
    _guard: tokio::sync::OwnedMutexGuard<()>,
}

impl PushLockGuard {
    pub(crate) fn new(wave_id: WaveId, guard: tokio::sync::OwnedMutexGuard<()>) -> Self {
        Self {
            wave_id,
            _guard: guard,
        }
    }

    pub fn wave_id(&self) -> &WaveId {
        &self.wave_id
    }
}
