//! The run's stop latch: the first attempt that stops the run wins, and
//! every other attempt sees it at once.
//!
//! - **One winner.** [`Latch::halt`] tells its caller whether it latched the
//!   stop. Only the winner of an auth stop records its auth row; a later
//!   auth failure is dropped. Other responses that have already arrived are
//!   recorded: the latch governs sending, not data received.
//! - **Stop first.** [`Latch::unless_stopped`] races a future against the
//!   stop with the stop branch first, so a sibling whose response or
//!   rate-limit slot is ready in the same poll as the latch is dropped.
//! - **No send after the latch.** [`Latch::send_check`] is the check the
//!   source runs at the actual request start: the deadline has not passed
//!   and the run has not stopped.
//! - **A failed write is fatal.** A `Fatal` stop replaces an earlier one: an
//!   auth row that cannot be written ends the start `FATAL`, not
//!   `AUTH_UNAVAILABLE`.

use std::sync::{Arc, Mutex};

use crate::clock::Clock;
use crate::types::UnixMs;

/// Why the run stopped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Stop {
    /// A request found no usable credentials.
    Auth,
    /// A response came after the window closed.
    WindowClosed,
    /// A Store write failed.
    Fatal(String),
}

/// The stop, and a signal that flips once it is set.
#[derive(Debug)]
pub(super) struct Latch {
    stop: Mutex<Option<Stop>>,
    halted: tokio::sync::watch::Sender<bool>,
}

impl Latch {
    pub fn new() -> Latch {
        Latch {
            stop: Mutex::new(None),
            halted: tokio::sync::watch::Sender::new(false),
        }
    }

    /// Stop the run. Returns whether this call latched it (it was not
    /// stopped before). A `Fatal` stop replaces an earlier stop.
    pub fn halt(&self, stop: Stop) -> bool {
        let won = {
            let mut current = self.stop.lock().expect("the stop lock is never poisoned");
            let won = current.is_none();
            if won || matches!(stop, Stop::Fatal(_)) {
                *current = Some(stop);
            }
            won
        };
        self.halted.send_replace(true);
        won
    }

    pub fn stopped(&self) -> Option<Stop> {
        self.stop
            .lock()
            .expect("the stop lock is never poisoned")
            .clone()
    }

    fn is_stopped(&self) -> bool {
        *self.halted.borrow()
    }

    /// `fut`'s output, or `None` when the run is stopped first or in the
    /// same poll; `fut` is then dropped.
    pub async fn unless_stopped<F: std::future::Future>(&self, fut: F) -> Option<F::Output> {
        let mut halted = self.halted.subscribe();
        tokio::select! {
            biased;
            _ = halted.wait_for(|halted| *halted) => None,
            output = fut => Some(output),
        }
    }

    /// Wait until the run is stopped.
    pub async fn wait(&self) {
        let mut halted = self.halted.subscribe();
        let _ = halted.wait_for(|halted| *halted).await;
    }

    /// The check at the actual request start: before `deadline` on `clock`,
    /// and the run not stopped.
    pub fn send_check<C: Clock>(
        self: &Arc<Self>,
        clock: Arc<C>,
        deadline: UnixMs,
    ) -> impl FnOnce() -> bool + Send + 'static {
        let latch = Arc::clone(self);
        move || !latch.is_stopped() && clock.now() < deadline
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed(UnixMs);

    impl Clock for Fixed {
        fn now(&self) -> UnixMs {
            self.0
        }

        async fn sleep_until(&self, _at: UnixMs) {}
    }

    #[test]
    fn the_first_halt_wins_and_a_fatal_one_replaces_it() {
        let latch = Latch::new();
        assert_eq!(latch.stopped(), None);
        assert!(latch.halt(Stop::Auth));
        assert!(!latch.halt(Stop::Auth));
        assert!(!latch.halt(Stop::WindowClosed));
        assert_eq!(latch.stopped(), Some(Stop::Auth));
        assert!(!latch.halt(Stop::Fatal("write".into())));
        assert_eq!(latch.stopped(), Some(Stop::Fatal("write".into())));
    }

    /// A stop and a result ready in the same poll: the stop wins, every
    /// time.
    #[tokio::test]
    async fn a_latched_stop_beats_a_ready_result() {
        let latch = Latch::new();
        latch.halt(Stop::Auth);
        for _ in 0..64 {
            assert_eq!(latch.unless_stopped(std::future::ready(1)).await, None);
        }
        let open = Latch::new();
        assert_eq!(open.unless_stopped(std::future::ready(1)).await, Some(1));
    }

    #[test]
    fn nothing_is_sent_once_the_run_has_stopped() {
        let latch = Arc::new(Latch::new());
        let clock = Arc::new(Fixed(UnixMs(10)));
        assert!(latch.send_check(Arc::clone(&clock), UnixMs(11))());
        assert!(!latch.send_check(Arc::clone(&clock), UnixMs(10))());
        let check = latch.send_check(Arc::clone(&clock), UnixMs(11));
        latch.halt(Stop::Auth);
        assert!(!check());
    }
}
