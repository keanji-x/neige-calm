//! The retry schedule: one dispatcher over the run's unsettled pairs, each
//! keyed by the time it may next be attempted, and the freeze between the
//! membership and the coverage work.
//!
//! **One queue, no per-pair sleeps.** Every pair still to be worked carries
//! `next_at`. The dispatcher starts the earliest eligible pairs, at most
//! [`MAX_IN_FLIGHT`] at once, and otherwise waits for an attempt to finish,
//! for the earliest `next_at` or for the carry deadline
//! ([`clock::wait_until`]). A pair that waits holds no worker.
//!
//! **Two rules.**
//! - *Membership, before the freeze:* each unfrozen index's constituents is
//!   retried every [`MEMBERSHIP_RETRY`] (a `throttled` one after its
//!   `retry_after`), whatever the attempt cap. No retry is scheduled past
//!   the carry deadline: the last is clamped to [`MEMBERSHIP_LAST_LEAD`]
//!   before it.
//! - *Coverage, after the freeze:* a counted failure (`transient`,
//!   `permanent`, `region_restricted`) waits [`backoff`] (1, 4, 16, then 64
//!   minutes) and counts toward `attempt_cap`; a cap of 6 spreads a pair's
//!   attempts over 149 minutes. A `throttled` failure waits its
//!   `retry_after` and never counts. Every planned pair follows it,
//!   constituents included.
//!
//! **The freeze** comes when every planned index has an `ok` constituents
//! row, or once `now ≥ carry_deadline` (at once when a start begins after
//! it). It never waits for a constituents attempt still in flight: that
//! attempt, and every later one, is ordinary coverage work, and its result
//! changes nothing in the frozen universe. No coverage pair is attempted
//! before the freeze.
//!
//! **Across starts.** `next_at` is not stored: [`survey`] derives it from
//! the pair's last stored attempt, so the next start resumes the schedule.
//! The wait a throttle named is not stored either, so a pair whose last row
//! is `throttled` waits the longest wait a server may name
//! ([`RETRY_AFTER_MAX`], 60 s) from that row.
//!
//! **The cap after the freeze.** A constituents pair's failures before the
//! freeze were the membership rule's; only its rows after the freeze count
//! toward the coverage cap.
//!
//! **Requests in flight.** The source's limiter paces request starts at
//! `rps`. The SDK retries a bare HTTP 429 up to five times inside one
//! request, outside the limiter (`rust/crates/httpclient/src/request.rs:475-513`
//! at v5.2.0); the server throttles with `429xxx` envelope codes instead,
//! which the SDK does not retry. The worst burst beyond the limiter is
//! `MAX_IN_FLIGHT × 6` requests within about 3.1 s.
//!
//! **Deadlines.** An attempt starts only before the start's deadline,
//! checked when it is dispatched and again by the source at the actual
//! request start, after the rate-limit slot
//! ([`LongbridgeSource::fetch_if`]). A token resolution or request still in
//! flight at the deadline is dropped there ([`clock::within`]).
//!
//! **The first stop latches** ([`super::latch`]). An attempt that finds no
//! usable token stops the run *before* it records that row. Every other
//! attempt in flight (its token resolution, rate-limit wait or request) is
//! dropped at once, stop first even when its result is ready in the same
//! poll; no request starts after the latch; and nothing more is dispatched.
//! The latch governs sending: a response that has already arrived is still
//! recorded. The one exception keeps a single auth row: only the attempt
//! that latched an auth stop records its auth failure; a later one is
//! dropped.

use std::{collections::HashSet, sync::Arc, time::Duration};

use tokio::task::JoinSet;

use super::latch::{Latch, Stop};
use super::runner::{Observed, Recorder};
use crate::catalog::{CaptureTier, EndpointId};
use crate::clock::{self, Clock};
use crate::source::{LongbridgeSource, RETRY_AFTER_FALLBACK, RETRY_AFTER_MAX};
use crate::store::PairCoverage;
use crate::types::{FailureClass, Key, Response, RunId, Status, UnixMs};
use crate::{Store, StoreError};

/// Requests in flight at once.
pub const MAX_IN_FLIGHT: usize = 4;

/// The interval between constituents attempts before the carry deadline.
pub const MEMBERSHIP_RETRY: Duration = Duration::from_secs(2 * 60);

/// The last constituents retry starts this long before the carry deadline.
pub const MEMBERSHIP_LAST_LEAD: Duration = Duration::from_secs(15);

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
    /// Rows that count toward the cap.
    counted: u32,
    /// The earliest time of the next attempt.
    next_at: UnixMs,
}

impl Work {
    fn id(&self) -> (Key, EndpointId) {
        (self.key.clone(), self.endpoint)
    }
}

/// Which rule a survey applies.
#[derive(Clone, Copy)]
pub(super) enum Rule {
    /// Constituents before the freeze: no cap, retried every
    /// [`MEMBERSHIP_RETRY`] up to the carry deadline.
    Membership { carry: UnixMs },
    /// Every pair after the freeze: the cap and [`backoff`]. For the
    /// membership endpoint only rows at or after the freeze (`since`) count
    /// toward the cap: its failures before the freeze were the membership
    /// rule's, so a recovered upstream is still retried. A row stamped in
    /// the freeze's own millisecond counts, the conservative side.
    Coverage {
        cap: u32,
        since: Option<(EndpointId, UnixMs)>,
    },
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

/// The next attempt of a membership pair before the freeze, after a failure
/// at `at` (`retry_after` for a throttle): `None` when it would fall at or
/// past the clamp before the carry deadline.
fn membership_next(at: UnixMs, retry_after: Option<Duration>, carry: UnixMs) -> Option<UnixMs> {
    let wanted = clock::after(at, retry_after.unwrap_or(MEMBERSHIP_RETRY));
    let lead = i64::try_from(MEMBERSHIP_LAST_LEAD.as_millis()).expect("15 s fits");
    let next = wanted.min(UnixMs(carry.0 - lead));
    (next > at).then_some(next)
}

/// The pairs `keep` selects, split by where they stand under `rule`. A
/// pending pair's `next_at` follows from its last stored attempt.
pub(super) async fn survey(
    store: &Store,
    run: RunId,
    rule: Rule,
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
        let after_freeze = |at: UnixMs| match rule {
            Rule::Coverage {
                since: Some((endpoint, frozen_at)),
                ..
            } if endpoint == pair.endpoint => at >= frozen_at,
            _ => true,
        };
        let counted = u32::try_from(
            rows.iter()
                .filter(|r| counts_toward_cap(r.2) && after_freeze(r.1))
                .count(),
        )
        .expect("attempt counts stay far below u32::MAX");
        let throttled = |status: &Status| *status == Status::Failed(FailureClass::Throttled);
        let next_at = match (rule, rows.last()) {
            (Rule::Coverage { cap, .. }, _) if counted >= cap => {
                survey.capped += 1;
                continue;
            }
            (_, None) => now,
            (Rule::Membership { carry }, Some((_, at, status))) => {
                // The wait the server named is not stored: assume the
                // longest one it may name.
                let retry_after = throttled(status).then_some(RETRY_AFTER_MAX);
                membership_next(*at, retry_after, carry)
                    .unwrap_or(carry)
                    .max(now)
            }
            (Rule::Coverage { .. }, Some((_, at, status))) => {
                if counts_toward_cap(*status) {
                    clock::after(*at, backoff(counted.max(1)))
                } else if throttled(status) {
                    clock::after(*at, RETRY_AFTER_MAX)
                } else {
                    now
                }
            }
        };
        survey.pending.push(Work {
            key: pair.key,
            endpoint: pair.endpoint,
            counted,
            next_at,
        });
    }
    Ok(survey)
}

/// What every attempt shares.
pub(super) struct Pool<C: Clock> {
    pub latch: Arc<Latch>,
    pub clock: Arc<C>,
    pub source: Arc<LongbridgeSource>,
    pub recorder: Recorder<C>,
    pub catalog: crate::Catalog,
}

/// What one attempt recorded: its status, and the wait a throttle names.
struct Attempted {
    work: Work,
    status: Status,
    retry_after: Option<Duration>,
}

/// One attempt of `work`, started before `deadline`. `None` when nothing
/// was recorded: the run stopped, the deadline cut it off, or the deadline
/// passed before the request could start.
async fn attempt<C: Clock>(pool: Arc<Pool<C>>, work: Work, deadline: UnixMs) -> Option<Attempted> {
    let endpoint = *pool
        .catalog
        .get(work.endpoint.as_str())
        .expect("a coverage endpoint is a catalog endpoint");
    let may_send = pool.latch.send_check(
        Arc::clone(&pool.clock),
        pool.recorder.window().start(),
        deadline,
    );
    let query = match pool.recorder.request(&work.key, work.endpoint).await {
        Ok(q) => q,
        Err(e) => {
            pool.latch.halt(Stop::Fatal(e.to_string()));
            return None;
        }
    };
    let fetch = pool.source.fetch_request_if(&endpoint, &query, may_send);
    let bounded = clock::within(&*pool.clock, deadline, fetch);
    let Some(Some(Some(fetched))) = pool.latch.unless_stopped(bounded).await else {
        return None;
    };
    let auth = matches!(
        fetched.response(),
        Response::Failed {
            class: FailureClass::AuthUnavailable,
            ..
        }
    );
    // Latch before the row is written: a slow write must not let the
    // siblings queued on the token or the rate-limit slot go on. Only the
    // winner of the latch records its auth row.
    let won = auth && pool.latch.halt(Stop::Auth);
    if auth && !won {
        return None;
    }
    let recorded = match pool
        .recorder
        .observe(&work.key, work.endpoint, fetched.response())
        .await
    {
        Ok(Observed::Recorded(recorded)) => recorded,
        Ok(Observed::WindowClosed) => {
            pool.latch.halt(Stop::WindowClosed);
            return None;
        }
        Err(err) => {
            pool.latch.halt(Stop::Fatal(err.to_string()));
            return None;
        }
    };
    Some(Attempted {
        work,
        status: recorded.status,
        retry_after: fetched.retry_after(),
    })
}

/// The bounds of one start.
#[derive(Clone, Copy)]
pub(super) struct Span {
    pub deadline: UnixMs,
    pub carry_deadline: UnixMs,
    pub cap: u32,
}

/// What [`work_off`] left.
pub(super) struct Worked {
    pub frozen: bool,
    pub stop: Option<Stop>,
}

/// Work the run until every pair is settled or past the deadline, or the
/// run stops: membership first (unless `frozen`), the freeze, then coverage.
pub(super) async fn work_off<C: Clock>(
    pool: &Arc<Pool<C>>,
    store: &Store,
    run: RunId,
    mut frozen: bool,
    span: Span,
) -> Result<Worked, StoreError> {
    let membership = pool.catalog.membership();
    let carry = span.carry_deadline;
    let mut frozen_at = store.frozen_at(run).await?;
    let coverage = |frozen_at: Option<UnixMs>| Rule::Coverage {
        cap: span.cap,
        since: frozen_at.map(|at| (membership, at)),
    };
    let mut pending = Vec::new();
    let mut supplemental = false;
    // Indexes without an `ok` constituents row yet.
    let mut unsettled: HashSet<Key> = HashSet::new();
    if frozen {
        pending = survey(store, run, coverage(frozen_at), pool.clock.now(), |p| {
            p.tier == CaptureTier::Core
        })
        .await?
        .pending;
    } else if pool.clock.now() < carry {
        let indexes = survey(
            store,
            run,
            Rule::Membership { carry },
            pool.clock.now(),
            |p| p.endpoint == membership,
        )
        .await?;
        unsettled = indexes.pending.iter().map(|w| w.key.clone()).collect();
        pending = indexes.pending;
    }
    let mut in_flight = JoinSet::new();
    let mut flying: HashSet<(Key, EndpointId)> = HashSet::new();
    // Constituents attempts in flight at the freeze: their count toward the
    // cap restarts when they return.
    let mut recount: HashSet<(Key, EndpointId)> = HashSet::new();
    loop {
        if pool.latch.stopped().is_some() {
            break;
        }
        let now = pool.clock.now();
        if !frozen && (unsettled.is_empty() || now >= carry) {
            // Freeze from the stored rows; attempts in flight are not waited
            // for and continue as coverage work.
            let Some(stamp) = pool.recorder.freeze().await? else {
                pool.latch.halt(Stop::WindowClosed);
                break;
            };
            frozen = true;
            frozen_at = Some(stamp);
            recount = flying
                .iter()
                .filter(|(_, endpoint)| *endpoint == membership)
                .cloned()
                .collect();
            pending = survey(store, run, coverage(frozen_at), stamp, |p| {
                p.tier == CaptureTier::Core
            })
            .await?
            .pending
            .into_iter()
            .filter(|w| !flying.contains(&w.id()))
            .collect();
        }
        // A second phase, not per-key ordering: no auxiliary request may
        // occupy a slot while Core is pending (including a backoff wait).
        if frozen && !supplemental && in_flight.is_empty() && pending.is_empty() {
            let core = survey(store, run, coverage(frozen_at), now, |p| {
                p.tier == CaptureTier::Core
            })
            .await?;
            if core.pending.is_empty() && now < pool.recorder.window().end() {
                store.complete_core(run, now).await?;
                supplemental = true;
                pending = survey(store, run, coverage(frozen_at), now, |p| {
                    p.tier == CaptureTier::Supplemental
                })
                .await?
                .pending;
            }
        }
        let bound = if frozen {
            span.deadline
        } else {
            span.deadline.min(carry)
        };
        pending.retain(|w| w.next_at.max(now) < bound);
        while in_flight.len() < MAX_IN_FLIGHT {
            let earliest = pending
                .iter()
                .enumerate()
                .filter(|(_, w)| w.next_at <= now)
                .min_by_key(|(i, w)| (w.next_at, *i))
                .map(|(i, _)| i);
            let Some(i) = earliest else { break };
            let work = pending.remove(i);
            flying.insert(work.id());
            in_flight.spawn(attempt(Arc::clone(pool), work, span.deadline));
        }
        let freeze_at = (!frozen && now < carry && carry < span.deadline).then_some(carry);
        let next_pair = (in_flight.len() < MAX_IN_FLIGHT)
            .then(|| pending.iter().map(|w| w.next_at).min())
            .flatten();
        let wake = [freeze_at, next_pair].into_iter().flatten().min();
        let busy = !in_flight.is_empty();
        if in_flight.is_empty() && wake.is_none() {
            break;
        }
        tokio::select! {
            biased;
            () = pool.latch.wait() => {}
            Some(joined) = in_flight.join_next(), if !in_flight.is_empty() => {
                let attempted = match joined {
                    Ok(attempted) => attempted,
                    Err(err) => std::panic::resume_unwind(err.into_panic()),
                };
                let Some(Attempted { mut work, status, retry_after }) = attempted else {
                    continue;
                };
                flying.remove(&work.id());
                if recount.remove(&work.id()) {
                    work.counted = 0;
                }
                let now = pool.clock.now();
                if counts_toward_cap(status) {
                    work.counted += 1;
                }
                let next = match status {
                    Status::Ok => {
                        unsettled.remove(&work.key);
                        None
                    }
                    Status::Failed(FailureClass::AuthUnavailable) => None,
                    _ if !frozen && work.endpoint == membership => {
                        membership_next(now, retry_after, carry)
                    }
                    Status::Failed(FailureClass::Throttled) => Some(clock::after(
                        now,
                        retry_after.unwrap_or(RETRY_AFTER_FALLBACK),
                    )),
                    _ if work.counted >= span.cap => None,
                    _ => Some(clock::after(now, backoff(work.counted))),
                };
                if let Some(next_at) = next {
                    work.next_at = next_at;
                    pending.push(work);
                }
            },
            () = clock::wait_until(&*pool.clock, wake.unwrap_or(now), busy), if wake.is_some() => {}
        }
    }
    while let Some(joined) = in_flight.join_next().await {
        if let Err(err) = joined {
            std::panic::resume_unwind(err.into_panic());
        }
    }
    Ok(Worked {
        frozen,
        stop: pool.latch.stopped(),
    })
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
    fn membership_retries_stop_short_of_the_carry_deadline() {
        let carry = UnixMs(60 * 60_000);
        let at = |min: i64| UnixMs(min * 60_000);
        assert_eq!(membership_next(at(10), None, carry), Some(at(12)));
        assert_eq!(
            membership_next(at(10), Some(Duration::from_millis(200)), carry),
            Some(UnixMs(at(10).0 + 200))
        );
        // Clamped to 15 s before the carry deadline, then no more.
        assert_eq!(
            membership_next(at(59), None, carry),
            Some(UnixMs(carry.0 - 15_000))
        );
        assert_eq!(membership_next(UnixMs(carry.0 - 15_000), None, carry), None);
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
