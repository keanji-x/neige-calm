//! Retry timing for shared Codex supervision.
use std::{
    sync::atomic::Ordering,
    time::{Duration, Instant},
};
const HEAL_JITTER_FRACTION: f64 = 0.2;

#[derive(Debug)]
pub(crate) struct BackoffState {
    initial: Duration,
    max: Duration,
    stable_window: Duration,
    attempts: std::sync::atomic::AtomicU64,
    last_relaunch_at: std::sync::Mutex<Option<Instant>>,
}

impl BackoffState {
    pub(crate) fn new(initial: Duration, max: Duration) -> Self {
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

    fn reset(&self) {
        self.attempts.store(0, Ordering::SeqCst);
        *self
            .last_relaunch_at
            .lock()
            .expect("backoff relaunch timestamp mutex poisoned") = None;
    }

    pub(crate) fn note_relaunch_now(&self) {
        *self
            .last_relaunch_at
            .lock()
            .expect("backoff relaunch timestamp mutex poisoned") = Some(Instant::now());
    }

    pub(crate) fn next_delay(&self) -> Duration {
        self.reset_if_stable();
        let attempt = self.attempts.fetch_add(1, Ordering::SeqCst);
        bounded_exponential_backoff(self.initial, self.max, attempt)
    }

    /// Slow heal lane: same attempts counter as [`Self::next_delay`], floor = this state's
    /// max, caller-supplied ceiling.
    pub(crate) fn next_slow_delay(&self, ceiling: Duration) -> Duration {
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

    #[cfg(test)]
    fn simulate_stable_run_for(&self, duration: Duration) {
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

fn bounded_exponential_backoff(initial: Duration, max: Duration, attempt: u64) -> Duration {
    let shift = attempt.min(31);
    let factor = 1_u32 << shift;
    initial.saturating_mul(factor).min(max)
}

/// Failure classification for the heal lanes: the Transient messages are exactly the
/// cold-start poll failures plus the crash-watcher exit shape; everything else is slow-lane.
pub(crate) fn classify_spawn_failure(err: &impl std::fmt::Display) -> FailureClass {
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
pub(crate) fn heal_jitter(delay: Duration) -> Duration {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0) as u64;
    let unit = (nanos % 1_000_001) as f64 / 1_000_000.0;
    delay.mul_f64(1.0 - HEAL_JITTER_FRACTION + 2.0 * HEAL_JITTER_FRACTION * unit)
}

/// Slow-lane retry ceiling for the self-heal loop. A code invariant, not a knob.
pub(crate) const HEAL_SLOW_RETRY_CEILING: Duration = Duration::from_secs(300);

/// Classified failure lanes for the self-heal loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailureClass {
    /// Fast lane — the existing `BackoffState` knobs. Child exited,
    /// handshake/cold-start deadline, socket errors.
    Transient,
    /// Slow lane — spawn exec error, settings/config read error, guard
    /// refusal that is safe to retry (floor `restart_max_delay_ms`, ceiling
    /// [`HEAL_SLOW_RETRY_CEILING`]).
    Persistent,
    /// Slow lane, reconciliation-only rounds: a possible surviving daemon we could not prove
    /// gone. The spawn path is unreachable until a round proves absence.
    Unreconciled,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::CalmError;

    /// Lane classification: cold-start poll failures and child-exit shapes are Transient;
    /// exec/config/guard failures are Persistent.
    #[test]
    fn classify_spawn_failure_assigns_lanes() {
        let transient = [
            "shared codex app-server exited before initialize (exit status: 7) after 0.1s: x",
            "shared codex app-server not initialized after 2.0s (deadline 2s, child still alive): x",
            "shared codex app-server exited: exit status: 1",
        ];
        for msg in transient {
            assert_eq!(
                classify_spawn_failure(&CalmError::CodexAppServer(msg.into())),
                FailureClass::Transient,
                "{msg}"
            );
        }
        let persistent = [
            "spawn shared codex app-server: No such file or directory (os error 2)",
            "refusing to launch shared codex app-server: unexpected mcp server evil",
            "db error: settings read failed",
        ];
        for msg in persistent {
            assert_eq!(
                classify_spawn_failure(&CalmError::CodexAppServer(msg.into())),
                FailureClass::Persistent,
                "{msg}"
            );
        }
    }

    /// Jitter stays within ±20%.
    #[test]
    fn heal_jitter_stays_within_twenty_percent() {
        for _ in 0..64 {
            let jittered = heal_jitter(Duration::from_secs(100));
            assert!(
                jittered >= Duration::from_secs(80) && jittered <= Duration::from_secs(120),
                "jittered delay {jittered:?} outside ±20% of 100s"
            );
        }
    }

    /// Persistent hits the slow ceiling with a floor at `restart_max_delay`; Transient stays
    /// on the fast lane cap.
    #[test]
    fn slow_lane_floors_at_restart_max_and_caps_at_const_ceiling() {
        let state = BackoffState::new(Duration::from_millis(250), Duration::from_secs(10));
        assert_eq!(
            state.next_slow_delay(HEAL_SLOW_RETRY_CEILING),
            Duration::from_secs(10),
            "slow-lane floor must be restart_max_delay"
        );
        let mut last = Duration::ZERO;
        for _ in 0..20 {
            last = state.next_slow_delay(HEAL_SLOW_RETRY_CEILING);
        }
        assert_eq!(
            last, HEAL_SLOW_RETRY_CEILING,
            "slow-lane ceiling must be the 300s const"
        );

        let fast = BackoffState::new(Duration::from_millis(250), Duration::from_secs(10));
        let mut last_fast = Duration::ZERO;
        for _ in 0..20 {
            last_fast = fast.next_delay();
        }
        assert_eq!(
            last_fast,
            Duration::from_secs(10),
            "the Transient lane keeps the configured fast cap"
        );
    }

    #[test]
    fn bounded_exponential_backoff_caps_at_max() {
        let initial = Duration::from_millis(250);
        let max = Duration::from_secs(10);
        assert_eq!(
            bounded_exponential_backoff(initial, max, 0),
            Duration::from_millis(250)
        );
        assert_eq!(bounded_exponential_backoff(initial, max, 99), max);

        let state = BackoffState::new(initial, max);
        let mut last = Duration::ZERO;
        for _ in 0..20 {
            last = state.next_delay();
        }
        assert_eq!(last, max);
    }

    #[test]
    fn backoff_does_not_reset_within_stable_window() {
        let state = BackoffState::new(Duration::from_millis(250), Duration::from_secs(10));

        let d1 = state.next_delay();
        state.note_relaunch_now();
        let d2_no_stable = state.next_delay();

        assert!(
            d2_no_stable > d1,
            "backoff must grow without stable window: {d1:?} -> {d2_no_stable:?}"
        );
    }

    #[test]
    fn backoff_resets_after_stable_window() {
        let state = BackoffState::new(Duration::from_millis(250), Duration::from_secs(10));
        let _ = state.next_delay();
        let _ = state.next_delay();
        let _ = state.next_delay();
        state.note_relaunch_now();
        state.simulate_stable_run_for(Duration::from_secs(61));

        let d_after_stable = state.next_delay();

        assert_eq!(
            d_after_stable,
            Duration::from_millis(250),
            "backoff must reset to initial after stable window: got {d_after_stable:?}"
        );
    }
}
