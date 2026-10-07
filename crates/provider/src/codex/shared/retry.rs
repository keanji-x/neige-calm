//! Retry timing for shared Codex supervision.
use super::FailureClass;
use std::{
    sync::atomic::Ordering,
    time::{Duration, Instant},
};
const HEAL_JITTER_FRACTION: f64 = 0.2;

#[derive(Debug)]
pub struct BackoffState {
    initial: Duration,
    max: Duration,
    stable_window: Duration,
    attempts: std::sync::atomic::AtomicU64,
    last_relaunch_at: std::sync::Mutex<Option<Instant>>,
}

impl BackoffState {
    pub fn new(initial: Duration, max: Duration) -> Self {
        let initial = initial.max(Duration::from_millis(1));
        let max = max.max(initial);
        Self {
            initial,
            max,
            stable_window: Duration::from_secs(60),
            attempts: std::sync::atomic::AtomicU64::new(0),
            last_relaunch_at: std::sync::Mutex::new(None),
        }
    }

    pub fn reset(&self) {
        self.attempts.store(0, Ordering::SeqCst);
        *self
            .last_relaunch_at
            .lock()
            .expect("backoff relaunch timestamp mutex poisoned") = None;
    }

    pub fn note_relaunch_now(&self) {
        *self
            .last_relaunch_at
            .lock()
            .expect("backoff relaunch timestamp mutex poisoned") = Some(Instant::now());
    }

    pub fn next_delay(&self) -> Duration {
        self.reset_if_stable();
        let attempt = self.attempts.fetch_add(1, Ordering::SeqCst);
        bounded_exponential_backoff(self.initial, self.max, attempt)
    }

    /// Slow heal lane: same attempts counter as [`Self::next_delay`], floor = this state's
    /// max, caller-supplied ceiling.
    pub fn next_slow_delay(&self, ceiling: Duration) -> Duration {
        self.reset_if_stable();
        let attempt = self.attempts.fetch_add(1, Ordering::SeqCst);
        bounded_exponential_backoff(self.max, ceiling.max(self.max), attempt)
    }

    fn reset_if_stable(&self) {
        let Some(last_relaunch_at) = *self
            .last_relaunch_at
            .lock()
            .expect("backoff relaunch timestamp mutex poisoned")
        else {
            return;
        };
        if last_relaunch_at.elapsed() >= self.stable_window {
            self.reset();
        }
    }

    #[cfg(any(test, feature = "fixtures"))]
    pub fn simulate_stable_run_for(&self, duration: Duration) {
        *self
            .last_relaunch_at
            .lock()
            .expect("backoff relaunch timestamp mutex poisoned") = Some(
            Instant::now()
                .checked_sub(duration)
                .unwrap_or_else(Instant::now),
        );
    }
}

pub fn bounded_exponential_backoff(initial: Duration, max: Duration, attempt: u64) -> Duration {
    let shift = attempt.min(31);
    let factor = 1_u32 << shift;
    initial.saturating_mul(factor).min(max)
}

/// Failure classification for the heal lanes: the Transient messages are exactly the
/// cold-start poll failures plus the crash-watcher exit shape; everything else is slow-lane.
pub fn classify_spawn_failure(err: &impl std::fmt::Display) -> FailureClass {
    let msg = err.to_string();
    if msg.contains("exited before initialize")
        || msg.contains("not initialized after")
        || msg.contains("app-server exited")
    {
        FailureClass::Transient
    } else {
        FailureClass::Persistent
    }
}

/// ±20% uniform jitter; clock-derived entropy suffices because the heal loop only needs
/// desynchronization.
pub fn heal_jitter(delay: Duration) -> Duration {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0) as u64;
    let unit = (nanos % 1_000_001) as f64 / 1_000_000.0;
    delay.mul_f64(1.0 - HEAL_JITTER_FRACTION + 2.0 * HEAL_JITTER_FRACTION * unit)
}
