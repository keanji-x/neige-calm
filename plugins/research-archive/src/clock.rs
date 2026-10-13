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

    /// Follows the wall clock: re-checks it every [`WALL_STEP`] at most, so
    /// a forward step of the wall clock (suspend, NTP) ends the wait within
    /// one step.
    async fn sleep_until(&self, at: UnixMs) {
        reached(self, at).await;
    }
}

/// The longest a wall-clock wait sleeps before it reads the clock again.
/// After a forward jump of the clock, a wait ends at most this late.
pub const WALL_STEP: Duration = Duration::from_millis(250);

/// Wait until `clock.now() >= at`, without moving the clock: sleep at most
/// [`WALL_STEP`] at a time, and never past the time that is left, then read
/// the clock again. A time already past returns at once.
pub async fn reached<C: Clock>(clock: &C, at: UnixMs) {
    loop {
        let left = at.0.saturating_sub(clock.now().0);
        let Ok(left) = u64::try_from(left) else {
            return;
        };
        if left == 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(left).min(WALL_STEP)).await;
    }
}

/// A wait for `at` that follows `clock` and also ends after `at - now` of
/// the runtime's timer, bringing the clock to `at` then. With
/// [`SystemClock`] both arms end at `at` (the first within [`WALL_STEP`] of
/// a forward jump); with a test clock the timer arm lets time pass while
/// nothing moves the clock, and the clock arm follows a test that moves it.
async fn deadline_wait<C: Clock>(clock: &C, at: UnixMs) {
    let left = u64::try_from(at.0.saturating_sub(clock.now().0)).unwrap_or(0);
    tokio::select! {
        () = reached(clock, at) => {}
        () = async {
            tokio::time::sleep(Duration::from_millis(left)).await;
            clock.sleep_until(at).await;
        } => {}
    }
}

/// `at + d`, saturating.
pub fn after(at: UnixMs, d: Duration) -> UnixMs {
    UnixMs(at.0.saturating_add(i64::try_from(d.as_millis()).unwrap_or(i64::MAX)))
}

/// Run `fut` until `deadline` on `clock`: its output, or `None` when the
/// deadline came first, in which case the future is dropped (cancelling
/// whatever it was waiting on: a request, the SDK's own retries inside it,
/// or a token refresh) and the clock is at or past the deadline.
///
/// The deadline is the clock's, not a timeout fixed once: the race follows
/// the clock in [`WALL_STEP`]s ([`reached`]), so when the wall clock jumps
/// past the deadline the future is dropped within one step. With a test
/// clock, a runtime timer of `deadline - now` also ends the race, and moves
/// the clock to the deadline.
pub async fn within<C: Clock, F: Future>(clock: &C, deadline: UnixMs, fut: F) -> Option<F::Output> {
    tokio::select! {
        output = fut => Some(output),
        () = deadline_wait(clock, deadline) => None,
    }
}

/// Wait until `at`. With `busy` (work is in flight that may yet change what
/// happens next), the wait is [`within`]'s deadline wait, which follows the
/// clock and lets time pass on the runtime's timer; otherwise it is the
/// clock's own [`Clock::sleep_until`]. With [`SystemClock`] both are the
/// same wall-clock wait. A test clock moves on its own only while nothing is
/// in flight, so time never jumps under an attempt. A time already past
/// returns at once.
pub async fn wait_until<C: Clock>(clock: &C, at: UnixMs, busy: bool) {
    if at <= clock.now() {
        return;
    }
    if busy {
        deadline_wait(clock, at).await;
    } else {
        clock.sleep_until(at).await;
    }
}
