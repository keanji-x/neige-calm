//! The capture job's clock. Every decision the runner makes (the window, the
//! deadlines, the backoff, the calendar's age) and every timestamp it stores
//! comes from one [`Clock`]. Production uses [`SystemClock`]; the library
//! tests drive the runner with a fake one, so window and deadline logic run
//! in milliseconds. There is no flag or variable that swaps the clock of the
//! binary.
//!
//! Request pacing is not a clock decision: the source's rate limiter spaces
//! actual request starts on the runtime's monotonic clock.

use std::future::Future;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::types::UnixMs;

/// Wall time and waiting on it.
pub trait Clock: Send + Sync + 'static {
    /// The current wall time.
    fn now(&self) -> UnixMs;

    /// Wait until `now() >= at`. Returns at once when `at` has passed.
    fn sleep_until(&self, at: UnixMs) -> impl Future<Output = ()> + Send;
}

/// The system's wall clock.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> UnixMs {
        let since = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("the system clock is after 1970");
        UnixMs(i64::try_from(since.as_millis()).expect("milliseconds since 1970 fit an i64"))
    }

    async fn sleep_until(&self, at: UnixMs) {
        let wait = at.0.saturating_sub(self.now().0);
        if let Ok(ms) = u64::try_from(wait)
            && ms > 0
        {
            tokio::time::sleep(Duration::from_millis(ms)).await;
        }
    }
}

/// `at + d`, saturating.
pub fn after(at: UnixMs, d: Duration) -> UnixMs {
    UnixMs(at.0.saturating_add(i64::try_from(d.as_millis()).unwrap_or(i64::MAX)))
}

/// Run `fut` until `deadline` on `clock`: its output, or `None` when the
/// deadline came first, in which case the future is dropped (cancelling
/// whatever it was waiting on, a request or a token refresh) and the clock
/// is brought to the deadline.
///
/// The race uses the runtime's timer for `deadline - now`. With
/// [`SystemClock`] that expires at the deadline; with a test clock it is a
/// real-time bound of the same length, after which the test clock is moved
/// to the deadline, so the caller sees the deadline passed either way.
pub async fn within<C: Clock, F: Future>(clock: &C, deadline: UnixMs, fut: F) -> Option<F::Output> {
    let left = u64::try_from(deadline.0.saturating_sub(clock.now().0)).unwrap_or(0);
    match tokio::time::timeout(Duration::from_millis(left), fut).await {
        Ok(output) => Some(output),
        Err(_) => {
            clock.sleep_until(deadline).await;
            None
        }
    }
}

/// Wait until `at`. With `busy` (work is in flight that may yet change what
/// happens next), the wait runs on the runtime's timer for `at - now`, as
/// [`within`] does, and the clock is brought to `at` only when that timer
/// fires; otherwise it is the clock's own [`Clock::sleep_until`]. With
/// [`SystemClock`] both are the same wait. A test clock moves on its own
/// only while nothing is in flight, so time never jumps under an attempt.
/// A time already past returns at once.
pub async fn wait_until<C: Clock>(clock: &C, at: UnixMs, busy: bool) {
    if at <= clock.now() {
        return;
    }
    if busy {
        let left = u64::try_from(at.0.saturating_sub(clock.now().0)).unwrap_or(0);
        tokio::time::sleep(Duration::from_millis(left)).await;
    }
    clock.sleep_until(at).await;
}
