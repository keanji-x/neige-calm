//! Per-outage reconnect deadline and backoff: a pure type the pump drives with `Instant`s.

use std::time::{Duration, Instant};

/// First delay between reconnect attempts; doubles each time.
pub(crate) const RECONNECT_BACKOFF_INITIAL: Duration = Duration::from_millis(100);
/// Ceiling for the doubling.
pub(crate) const RECONNECT_BACKOFF_CAP: Duration = Duration::from_secs(5);
/// Total time one outage may take before the shim gives up (exit 5).
/// Well below codex's default 120 s `tools/call` timeout.
pub(crate) const RECONNECT_BUDGET: Duration = Duration::from_secs(30);
/// Total time the first connection may take (exit 3). Below codex's default 30 s MCP
/// startup timeout.
pub(crate) const INITIAL_CONNECT_BUDGET: Duration = Duration::from_secs(15);

/// Deadline + backoff for one outage. Pure: the methods that depend on time take `now`.
#[derive(Debug)]
pub(crate) struct ReconnectBudget {
    total: Duration,
    start: Instant,
    backoff: Duration,
    attempts: u32,
}

impl ReconnectBudget {
    pub(crate) fn new(now: Instant, total: Duration) -> Self {
        Self {
            total,
            start: now,
            backoff: RECONNECT_BACKOFF_INITIAL,
            attempts: 0,
        }
    }

    pub(crate) fn reset(&mut self, now: Instant, total: Duration) {
        *self = Self::new(now, total);
    }

    /// The instant the outage gives up; `next_delay` never sleeps past it.
    pub(crate) fn deadline(&self) -> Instant {
        self.start + self.total
    }

    /// The delay to sleep before the next attempt, or `None` once the deadline has passed.
    pub(crate) fn next_delay(&mut self, now: Instant) -> Option<Duration> {
        let elapsed = now.saturating_duration_since(self.start);
        if elapsed >= self.total {
            return None;
        }
        let delay = self.backoff.min(self.total - elapsed);
        self.backoff = (self.backoff * 2).min(RECONNECT_BACKOFF_CAP);
        Some(delay)
    }

    pub(crate) fn record_attempt(&mut self) {
        self.attempts += 1;
    }

    pub(crate) fn attempts(&self) -> u32 {
        self.attempts
    }

    pub(crate) fn elapsed(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.start)
    }
}

#[cfg(test)]
mod budget_tests {
    use std::time::{Duration, Instant};

    use super::{INITIAL_CONNECT_BUDGET, RECONNECT_BACKOFF_CAP, RECONNECT_BUDGET, ReconnectBudget};

    #[test]
    fn backoff_doubles_from_100ms_to_the_5s_cap() {
        let t0 = Instant::now();
        let mut budget = ReconnectBudget::new(t0, RECONNECT_BUDGET);
        let delays: Vec<u64> = (0..8)
            .map(|_| budget.next_delay(t0).expect("inside budget").as_millis() as u64)
            .collect();
        assert_eq!(delays, [100, 200, 400, 800, 1600, 3200, 5000, 5000]);
        assert_eq!(RECONNECT_BACKOFF_CAP, Duration::from_secs(5));
    }

    #[test]
    fn gives_up_once_the_deadline_is_reached() {
        let t0 = Instant::now();
        let mut budget = ReconnectBudget::new(t0, RECONNECT_BUDGET);
        assert!(budget.next_delay(t0 + Duration::from_secs(29)).is_some());
        assert_eq!(budget.next_delay(t0 + RECONNECT_BUDGET), None);
        assert_eq!(budget.next_delay(t0 + Duration::from_secs(40)), None);
    }

    #[test]
    fn delay_is_clipped_to_the_time_left() {
        let t0 = Instant::now();
        let mut budget = ReconnectBudget::new(t0, RECONNECT_BUDGET);
        let near_end = t0 + RECONNECT_BUDGET - Duration::from_millis(30);
        assert_eq!(budget.next_delay(near_end), Some(Duration::from_millis(30)));
    }

    #[test]
    fn reset_restarts_deadline_backoff_and_attempts() {
        let t0 = Instant::now();
        let mut budget = ReconnectBudget::new(t0, RECONNECT_BUDGET);
        for _ in 0..4 {
            budget.record_attempt();
            budget.next_delay(t0);
        }
        assert_eq!(budget.attempts(), 4);
        let t1 = t0 + Duration::from_secs(25);
        budget.reset(t1, RECONNECT_BUDGET);
        assert_eq!(budget.attempts(), 0);
        assert_eq!(budget.elapsed(t1), Duration::ZERO);
        assert_eq!(budget.next_delay(t1), Some(Duration::from_millis(100)));
        assert!(budget.next_delay(t1 + Duration::from_secs(29)).is_some());
        assert_eq!(budget.next_delay(t1 + RECONNECT_BUDGET), None);
    }

    #[test]
    fn deadline_is_start_plus_total_and_follows_reset() {
        let t0 = Instant::now();
        let mut budget = ReconnectBudget::new(t0, RECONNECT_BUDGET);
        assert_eq!(budget.deadline(), t0 + RECONNECT_BUDGET);
        let t1 = t0 + Duration::from_secs(3);
        budget.reset(t1, INITIAL_CONNECT_BUDGET);
        assert_eq!(budget.deadline(), t1 + INITIAL_CONNECT_BUDGET);
        assert_eq!(
            budget.next_delay(budget.deadline() - Duration::from_millis(1)),
            Some(Duration::from_millis(1))
        );
        assert_eq!(budget.next_delay(budget.deadline()), None);
    }
}
