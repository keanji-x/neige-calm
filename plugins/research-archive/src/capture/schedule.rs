//! The retry schedule: one queue of unsettled pairs keyed by the time each
//! may next be attempted.
//!
//! **One queue, no per-pair sleeps.** Every pair still to be worked carries
//! `next_at`. The dispatcher starts the earliest eligible pairs, at most
//! [`MAX_IN_FLIGHT`] at once, and otherwise waits for an attempt to finish
//! or for the earliest `next_at`. A pair that waits out its backoff holds no
//! worker, so one slow pair never delays the others.
//!
//! **Minute-scale backoff.** A counted failure (`transient`, `permanent`,
//! `region_restricted`) waits [`backoff`]: 1, 4, 16, then 64 minutes, so a
//! cap of 6 attempts spans 1 + 4 + 16 + 64 + 64 = 149 minutes. A `throttled`
//! failure waits the server's `retry_after` and never counts. The window is
//! about 16 hours, so a pair's attempts are spread over a real outage
//! instead of being spent in seconds.
//!
//! **Across starts.** `next_at` is not stored: [`survey`] derives it from
//! the pair's last stored attempt, so the next start resumes the schedule.
//!
//! **Requests in flight.** The source's limiter paces request starts at
//! `rps`. The SDK retries a bare HTTP 429 up to five times inside one
//! request, outside the limiter (`rust/crates/httpclient/src/request.rs:475-513`
//! at v5.2.0); the server throttles with the envelope code 429002 instead,
//! which the SDK does not retry. The worst burst beyond the limiter is
//! `MAX_IN_FLIGHT × 6` requests within about 3.1 s.
//!
//! **Deadlines.** An attempt starts only while `now` is before its bound,
//! checked when it is dispatched and again by the source at the actual
//! request start, after the rate-limit slot ([`LongbridgeSource::fetch_if`]).
//! A token resolution or request still in flight at the bound is dropped
//! there ([`clock::within`]).
//!
//! **The first stop latches.** When an attempt stops the run (no usable
//! token, the window closed, a write failed), every other attempt in flight
//! is dropped at once and nothing more is dispatched.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use tokio::task::JoinSet;

use super::runner::Recorder;
use crate::catalog::EndpointId;
use crate::clock::{self, Clock};
use crate::source::{LongbridgeSource, RETRY_AFTER_FALLBACK};
use crate::store::PairCoverage;
use crate::types::{FailureClass, Key, RunId, Status, UnixMs};
use crate::{Store, StoreError};

/// Requests in flight at once.
pub const MAX_IN_FLIGHT: usize = 4;

/// The first wait after a counted failure.
const BACKOFF_BASE: Duration = Duration::from_secs(60);

/// The longest wait between two counted attempts of one pair.
const BACKOFF_MAX: Duration = Duration::from_secs(64 * 60);

/// The wait after the `n`th counted failure: 1, 4, 16, 64 minutes, then 64
/// minutes again.
pub fn backoff(counted: u32) -> Duration {
    let exp = counted.saturating_sub(1).min(3);
    (BACKOFF_BASE * 4u32.pow(exp)).min(BACKOFF_MAX)
}

/// The statuses that count toward the attempt cap: the failures a retry may
/// fix or never will. Throttles and auth pauses never count.
pub const COUNTED: [Status; 3] = [
    Status::Failed(FailureClass::Transient),
    Status::Failed(FailureClass::Permanent),
    Status::Failed(FailureClass::RegionRestricted),
];

/// Whether an attempt with `status` counts toward the attempt cap.
pub fn counts_toward_cap(status: Status) -> bool {
    COUNTED.contains(&status)
}

/// One planned pair still to be worked.
#[derive(Clone, Debug)]
pub(super) struct Work {
    key: Key,
    endpoint: EndpointId,
    /// Rows of any status so far.
    rows: u32,
    /// Rows that count toward the cap.
    counted: u32,
    /// The earliest time of the next attempt.
    next_at: UnixMs,
}

/// Where a run's selected pairs stand.
pub(super) struct Survey {
    /// Neither `ok` nor at the cap.
    pub pending: Vec<Work>,
    /// At the cap, with no `ok`.
    pub capped: usize,
    /// With an `ok`.
    pub ok: usize,
}

impl Survey {
    /// Every selected pair has an `ok`.
    pub fn all_ok(&self) -> bool {
        self.pending.is_empty() && self.capped == 0
    }
}

/// The pairs `keep` selects, split by where they stand. A pending pair's
/// `next_at` follows from its last stored attempt: after a counted failure,
/// that attempt's time plus [`backoff`]; after a throttle, plus
/// [`RETRY_AFTER_FALLBACK`]; otherwise `now`.
pub(super) async fn survey(
    store: &Store,
    run: RunId,
    cap: u32,
    now: UnixMs,
    keep: impl Fn(&PairCoverage) -> bool,
) -> Result<Survey, StoreError> {
    let mut survey = Survey {
        pending: Vec::new(),
        capped: 0,
        ok: 0,
    };
    for pair in store.coverage(run).await? {
        if !keep(&pair) {
            continue;
        }
        if pair.has_ok {
            survey.ok += 1;
            continue;
        }
        let rows = if pair.attempts == 0 {
            Vec::new()
        } else {
            store.attempts(run, &pair.key, pair.endpoint).await?
        };
        let counted = u32::try_from(rows.iter().filter(|r| counts_toward_cap(r.2)).count())
            .expect("attempt counts stay far below u32::MAX");
        if counted >= cap {
            survey.capped += 1;
            continue;
        }
        let next_at = match rows.last() {
            Some((_, at, status)) if counts_toward_cap(*status) => {
                clock::after(*at, backoff(counted))
            }
            Some((_, at, Status::Failed(FailureClass::Throttled))) => {
                clock::after(*at, RETRY_AFTER_FALLBACK)
            }
            _ => now,
        };
        survey.pending.push(Work {
            key: pair.key,
            endpoint: pair.endpoint,
            rows: pair.attempts,
            counted,
            next_at,
        });
    }
    Ok(survey)
}

/// Why an attempt stopped the whole run.
#[derive(Clone, Debug)]
pub(super) enum Stop {
    /// A request found no usable credentials.
    Auth,
    /// A response came after the window closed.
    WindowClosed,
    /// A Store write failed.
    Fatal(String),
}

/// How long pairs may be attempted in one phase: an attempt starts only
/// before `until`, except a pair's first attempt, which may start until
/// `first_until`.
#[derive(Clone, Copy)]
pub(super) struct Bounds {
    pub until: UnixMs,
    pub first_until: UnixMs,
}

impl Bounds {
    fn of(self, work: &Work) -> UnixMs {
        if work.rows == 0 {
            self.first_until
        } else {
            self.until
        }
    }
}

/// What every attempt shares.
pub(super) struct Pool<C: Clock> {
    /// Flips to `true` on the first [`Pool::halt`]; attempts in flight race
    /// it ([`Pool::unless_stopped`]).
    pub halted: tokio::sync::watch::Sender<bool>,
    pub clock: Arc<C>,
    pub source: Arc<LongbridgeSource>,
    pub recorder: Recorder<C>,
    pub catalog: crate::Catalog,
    pub cap: u32,
    pub stop: Mutex<Option<Stop>>,
}

impl<C: Clock> Pool<C> {
    fn halt(&self, stop: Stop) {
        self.stop
            .lock()
            .expect("the stop lock is never poisoned")
            .get_or_insert(stop);
        self.halted.send_replace(true);
    }

    /// `fut`'s output, or `None` when the run is stopped first, in which
    /// case `fut` is dropped.
    async fn unless_stopped<F: std::future::Future>(&self, fut: F) -> Option<F::Output> {
        let mut halted = self.halted.subscribe();
        tokio::select! {
            output = fut => Some(output),
            _ = halted.wait_for(|halted| *halted) => None,
        }
    }

    pub fn stopped(&self) -> Option<Stop> {
        self.stop
            .lock()
            .expect("the stop lock is never poisoned")
            .clone()
    }
}

/// One attempt of `work`, started before `bound`. Returns the pair again
/// with its next eligible time when it is to be retried; `None` when it is
/// settled (`ok`, or at the cap), out of time, or the run stopped.
async fn attempt<C: Clock>(pool: Arc<Pool<C>>, mut work: Work, bound: UnixMs) -> Option<Work> {
    let endpoint = *pool
        .catalog
        .get(work.endpoint.as_str())
        .expect("a coverage endpoint is a catalog endpoint");
    let may_send = {
        let clock = Arc::clone(&pool.clock);
        move || clock.now() < bound
    };
    let fetch = pool.source.fetch_if(&endpoint, &work.key, may_send);
    let bounded = clock::within(&*pool.clock, bound, fetch);
    // Stopped, cut off at the bound, or too late at the request start:
    // nothing was received, so nothing is recorded.
    let Some(Some(Some(fetched))) = pool.unless_stopped(bounded).await else {
        return None;
    };
    let recorded = match pool
        .recorder
        .observe(&work.key, work.endpoint, fetched.response())
        .await
    {
        Ok(Some(recorded)) => recorded,
        Ok(None) => {
            pool.halt(Stop::WindowClosed);
            return None;
        }
        Err(err) => {
            pool.halt(Stop::Fatal(err.to_string()));
            return None;
        }
    };
    work.rows += 1;
    let now = pool.clock.now();
    match recorded.status {
        Status::Ok => None,
        Status::Failed(FailureClass::AuthUnavailable) => {
            pool.halt(Stop::Auth);
            None
        }
        Status::Failed(FailureClass::Throttled) => {
            let wait = fetched.retry_after().unwrap_or(RETRY_AFTER_FALLBACK);
            work.next_at = clock::after(now, wait);
            Some(work)
        }
        status => {
            debug_assert!(counts_toward_cap(status));
            work.counted += 1;
            if work.counted >= pool.cap {
                return None;
            }
            work.next_at = clock::after(now, backoff(work.counted));
            Some(work)
        }
    }
}

/// Work `pending` until every pair is settled or past its bound, or the run
/// stops. Returns why it stopped, if it did.
pub(super) async fn work_off<C: Clock>(
    pool: &Arc<Pool<C>>,
    mut pending: Vec<Work>,
    bounds: Bounds,
) -> Option<Stop> {
    let mut in_flight = JoinSet::new();
    let mut halted = pool.halted.subscribe();
    loop {
        if pool.stopped().is_some() {
            break;
        }
        let now = pool.clock.now();
        pending.retain(|w| w.next_at.max(now) < bounds.of(w));
        while in_flight.len() < MAX_IN_FLIGHT {
            let earliest = pending
                .iter()
                .enumerate()
                .filter(|(_, w)| w.next_at <= now)
                .min_by_key(|(i, w)| (w.next_at, *i))
                .map(|(i, _)| i);
            let Some(i) = earliest else { break };
            let work = pending.remove(i);
            let bound = bounds.of(&work);
            in_flight.spawn(attempt(Arc::clone(pool), work, bound));
        }
        if in_flight.is_empty() && pending.is_empty() {
            break;
        }
        // Wake for the earliest pending pair only when a slot is free.
        let wake = (in_flight.len() < MAX_IN_FLIGHT)
            .then(|| pending.iter().map(|w| w.next_at).min())
            .flatten();
        tokio::select! {
            biased;
            _ = halted.wait_for(|halted| *halted) => {}
            Some(joined) = in_flight.join_next(), if !in_flight.is_empty() => match joined {
                Ok(Some(work)) => pending.push(work),
                Ok(None) => {}
                Err(err) => std::panic::resume_unwind(err.into_panic()),
            },
            () = pool.clock.sleep_until(wake.unwrap_or(now)), if wake.is_some() => {}
        }
    }
    while let Some(joined) = in_flight.join_next().await {
        if let Err(err) = joined {
            std::panic::resume_unwind(err.into_panic());
        }
    }
    pool.stopped()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_is_minute_scale_and_capped_at_64_minutes() {
        let waits: Vec<_> = (1..=6).map(|n| backoff(n).as_secs() / 60).collect();
        assert_eq!(waits, [1, 4, 16, 64, 64, 64]);
        // The example config's cap of 6 spreads its attempts over 149 min.
        let span: u64 = (1..6).map(|n| backoff(n).as_secs() / 60).sum();
        assert_eq!(span, 149);
    }

    #[test]
    fn only_transient_permanent_and_region_failures_count() {
        use FailureClass as F;
        assert!(!counts_toward_cap(Status::Ok));
        for (class, counts) in [
            (F::Transient, true),
            (F::Permanent, true),
            (F::RegionRestricted, true),
            (F::Throttled, false),
            (F::AuthUnavailable, false),
        ] {
            assert_eq!(
                counts_toward_cap(Status::Failed(class)),
                counts,
                "{class:?}"
            );
        }
    }
}
