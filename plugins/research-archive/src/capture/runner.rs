//! The run phases after the plan (#2575, "Run phases" 2-4): membership, the
//! freeze, the fetch, and how the run ends.
//!
//! **One serialized recorder.** Responses of concurrent requests are
//! recorded by [`Recorder`], which takes `fetched_at` from the clock *after*
//! it has its turn and writes under that same turn. So `fetched_at` is the
//! wall time the response was recorded, and the order of the stamps is the
//! order of the commits: a run's clock never goes back (#2577 item 2). The
//! freeze and the end take their timestamps the same way.
//!
//! **Requests in flight.** At most [`MAX_IN_FLIGHT`] requests run at once,
//! and the source's limiter paces their starts at `rps`. The SDK retries a
//! bare HTTP 429 up to five times inside one request, outside the limiter
//! (`rust/crates/httpclient/src/request.rs:475-513` at v5.2.0); the server's
//! throttle is the envelope code 429002 instead, which the SDK does not
//! retry. So the worst burst beyond the limiter is `MAX_IN_FLIGHT × 6`
//! requests within about 3.1 s (backoff 0.1 s doubling).
//!
//! **Deadlines.** No request starts at or after its bound, and a request or
//! token refresh still in flight at the bound is dropped there
//! ([`clock::within`]). A retry never waits past the deadline.

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};

use tokio::task::JoinSet;

use super::fail;
use crate::catalog::EndpointId;
use crate::clock::{self, Clock};
use crate::source::{LongbridgeSource, RETRY_AFTER_FALLBACK};
use crate::state::{ExitKind, Outcome};
use crate::store::{PairCoverage, RunPlan};
use crate::types::{FailureClass, Finish, Key, Recorded, Response, RunId, Status, UnixMs, Window};
use crate::{Store, StoreError};

/// Requests in flight at once.
pub const MAX_IN_FLIGHT: usize = 4;

/// The longest wait between two counted attempts of one pair.
const BACKOFF_MAX: Duration = Duration::from_secs(60);

/// The wait after the `n`th counted failure: 1 s, 2 s, 4 s, … up to
/// [`BACKOFF_MAX`].
pub fn backoff(counted: u32) -> Duration {
    let exp = counted.saturating_sub(1).min(6);
    Duration::from_secs(1u64 << exp).min(BACKOFF_MAX)
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

/// What the runner needs about one open run.
pub(super) struct RunCtx<C: Clock> {
    pub clock: Arc<C>,
    pub store: Store,
    pub source: Arc<LongbridgeSource>,
    pub plan: RunPlan,
    /// `min(window_end, process start + attempt_timeout)`.
    pub deadline: UnixMs,
    /// `window_start + carry_deadline`.
    pub carry_deadline: UnixMs,
    pub attempt_cap: u32,
}

/// The one writer of a run's timestamps.
struct Recorder<C: Clock> {
    clock: Arc<C>,
    store: Store,
    run: RunId,
    window: Window,
    turn: tokio::sync::Mutex<()>,
}

impl<C: Clock> Recorder<C> {
    /// Record a response, stamped now. `None` when the window has closed:
    /// nothing is written.
    async fn observe(
        &self,
        key: &Key,
        endpoint: EndpointId,
        response: &Response,
    ) -> Result<Option<Recorded>, StoreError> {
        let _turn = self.turn.lock().await;
        let at = self.clock.now();
        if !self.window.contains(at) {
            return Ok(None);
        }
        self.store
            .record_observation(self.run, key, endpoint, at, response)
            .await
            .map(Some)
    }

    /// Freeze the membership now. `false` when the window has closed.
    async fn freeze(&self) -> Result<bool, StoreError> {
        let _turn = self.turn.lock().await;
        let at = self.clock.now();
        if !self.window.contains(at) {
            return Ok(false);
        }
        let frozen = self.store.freeze_membership(self.run, at).await?;
        for (index, membership) in &frozen {
            tracing::info!(
                index = %index,
                mode = membership.mode(),
                members = membership.members().len(),
                "membership frozen"
            );
        }
        Ok(true)
    }

    async fn end(&self, finish: Finish, reason: Option<&str>) -> Result<(), StoreError> {
        let _turn = self.turn.lock().await;
        let at = self.clock.now();
        self.store.end_run(self.run, at, finish, reason).await
    }
}

/// One planned pair still to be worked.
#[derive(Clone, Debug)]
struct Work {
    key: Key,
    endpoint: EndpointId,
    /// Rows of any status so far.
    rows: u32,
    /// Rows that count toward the cap.
    counted: u32,
}

/// Where a run's pairs stand.
struct Survey {
    open: Vec<Work>,
    exhausted: usize,
}

/// The pairs `keep` selects, split into open work and cap-exhausted pairs;
/// pairs with an `ok` are done.
async fn survey(
    store: &Store,
    run: RunId,
    cap: u32,
    keep: impl Fn(&PairCoverage) -> bool,
) -> Result<Survey, StoreError> {
    let mut open = Vec::new();
    let mut exhausted = 0;
    for pair in store.coverage(run).await? {
        if !keep(&pair) || pair.has_ok {
            continue;
        }
        let counted = if pair.attempts == 0 {
            0
        } else {
            let rows = store.attempts(run, &pair.key, pair.endpoint).await?;
            u32::try_from(
                rows.iter()
                    .filter(|(_, _, s)| counts_toward_cap(*s))
                    .count(),
            )
            .expect("attempt counts stay far below u32::MAX")
        };
        if counted >= cap {
            exhausted += 1;
        } else {
            open.push(Work {
                key: pair.key,
                endpoint: pair.endpoint,
                rows: pair.attempts,
                counted,
            });
        }
    }
    Ok(Survey { open, exhausted })
}

/// Why the workers stopped the whole run.
#[derive(Clone, Debug)]
enum Stop {
    /// A request found no usable credentials.
    Auth,
    /// A response came after the window closed.
    WindowClosed,
    /// A Store write failed.
    Fatal(String),
}

/// How long a pair may be worked: an attempt starts only before `until`,
/// except a pair's first attempt, which may start until `first_until`.
#[derive(Clone, Copy)]
struct Bounds {
    until: UnixMs,
    first_until: UnixMs,
}

/// The workers' shared state.
struct Pool<C: Clock> {
    clock: Arc<C>,
    source: Arc<LongbridgeSource>,
    recorder: Recorder<C>,
    catalog: crate::Catalog,
    cap: u32,
    queue: Mutex<VecDeque<Work>>,
    stop: Mutex<Option<Stop>>,
}

impl<C: Clock> Pool<C> {
    fn halt(&self, stop: Stop) {
        self.stop
            .lock()
            .expect("the stop lock is never poisoned")
            .get_or_insert(stop);
    }

    fn stopped(&self) -> Option<Stop> {
        self.stop
            .lock()
            .expect("the stop lock is never poisoned")
            .clone()
    }

    fn next(&self) -> Option<Work> {
        self.queue
            .lock()
            .expect("the queue lock is never poisoned")
            .pop_front()
    }

    /// Attempt one pair until it has an `ok`, its cap is exhausted, its
    /// bound passes, or the run stops.
    async fn drive(&self, mut work: Work, bounds: Bounds) {
        let endpoint = *self
            .catalog
            .get(work.endpoint.as_str())
            .expect("a coverage endpoint is a catalog endpoint");
        loop {
            if self.stopped().is_some() {
                return;
            }
            let bound = if work.rows == 0 {
                bounds.first_until
            } else {
                bounds.until
            };
            if self.clock.now() >= bound {
                return;
            }
            let fetch = self.source.fetch(&endpoint, &work.key);
            let Some(fetched) = clock::within(&*self.clock, bound, fetch).await else {
                return;
            };
            let recorded = match self
                .recorder
                .observe(&work.key, work.endpoint, fetched.response())
                .await
            {
                Ok(Some(recorded)) => recorded,
                Ok(None) => return self.halt(Stop::WindowClosed),
                Err(err) => return self.halt(Stop::Fatal(err.to_string())),
            };
            work.rows += 1;
            let wait = match recorded.status {
                Status::Ok => return,
                Status::Failed(FailureClass::AuthUnavailable) => return self.halt(Stop::Auth),
                Status::Failed(FailureClass::Throttled) => {
                    fetched.retry_after().unwrap_or(RETRY_AFTER_FALLBACK)
                }
                status => {
                    debug_assert!(counts_toward_cap(status));
                    work.counted += 1;
                    if work.counted >= self.cap {
                        return;
                    }
                    backoff(work.counted)
                }
            };
            let wake = clock::after(self.clock.now(), wait);
            if wake >= bounds.until {
                return;
            }
            self.clock.sleep_until(wake).await;
        }
    }
}

/// Work `open` with up to [`MAX_IN_FLIGHT`] workers until each pair is
/// settled or out of bounds. Returns why the run stopped, if it did.
async fn work_off<C: Clock>(pool: &Arc<Pool<C>>, open: Vec<Work>, bounds: Bounds) -> Option<Stop> {
    let workers = open.len().min(MAX_IN_FLIGHT);
    pool.queue
        .lock()
        .expect("the queue lock is never poisoned")
        .extend(open);
    let mut set = JoinSet::new();
    for _ in 0..workers {
        let pool = Arc::clone(pool);
        set.spawn(async move {
            while pool.stopped().is_none() {
                let Some(work) = pool.next() else { break };
                pool.drive(work, bounds).await;
            }
        });
    }
    while let Some(joined) = set.join_next().await {
        if let Err(err) = joined {
            std::panic::resume_unwind(err.into_panic());
        }
    }
    pool.queue
        .lock()
        .expect("the queue lock is never poisoned")
        .clear();
    pool.stopped()
}

/// Run the phases of an open run and end it as far as the deadline allows.
pub(super) async fn drive<C: Clock>(ctx: RunCtx<C>) -> Outcome {
    match phases(&ctx).await {
        Ok(outcome) | Err(outcome) => outcome,
    }
}

async fn phases<C: Clock>(ctx: &RunCtx<C>) -> Result<Outcome, Outcome> {
    let as_of = ctx.plan.as_of;
    let run = ctx.plan.run_id;
    let store_err = |what: &str, err: StoreError| fail(what, &err).on(as_of);
    let pool = Arc::new(Pool {
        clock: Arc::clone(&ctx.clock),
        source: Arc::clone(&ctx.source),
        recorder: Recorder {
            clock: Arc::clone(&ctx.clock),
            store: ctx.store.clone(),
            run,
            window: ctx.plan.window,
            turn: tokio::sync::Mutex::new(()),
        },
        catalog: ctx.store.catalog().clone(),
        cap: ctx.attempt_cap,
        queue: Mutex::new(VecDeque::new()),
        stop: Mutex::new(None),
    });
    let stopped = |stop: Stop| match stop {
        Stop::Auth => {
            tracing::error!(%as_of, "a request found no usable token; the run stays open");
            Err(Outcome::of(ExitKind::AuthUnavailable, as_of))
        }
        Stop::Fatal(err) => {
            tracing::error!(%as_of, error = %err, "recording failed; the run stays open");
            Err(Outcome::of(ExitKind::Fatal, as_of))
        }
        // The end below finds the window closed.
        Stop::WindowClosed => Ok(()),
    };

    let membership = ctx.store.catalog().membership();
    let mut frozen = ctx.plan.frozen || ctx.plan.indexes.is_empty();
    if !frozen {
        let only_membership = |p: &PairCoverage| p.endpoint == membership;
        let open = survey(&ctx.store, run, ctx.attempt_cap, only_membership)
            .await
            .map_err(|e| store_err("survey", e))?
            .open;
        tracing::info!(%as_of, indexes = open.len(), "membership phase");
        let bounds = Bounds {
            until: ctx.deadline.min(ctx.carry_deadline),
            first_until: ctx.deadline,
        };
        if let Some(stop) = work_off(&pool, open, bounds).await {
            stopped(stop)?;
        }
        let settled = survey(&ctx.store, run, ctx.attempt_cap, only_membership)
            .await
            .map_err(|e| store_err("survey", e))?
            .open
            .is_empty();
        if settled || ctx.clock.now() >= ctx.carry_deadline {
            frozen = pool
                .recorder
                .freeze()
                .await
                .map_err(|e| store_err("freeze", e))?;
        }
    }
    if frozen && pool.stopped().is_none() {
        let open = survey(&ctx.store, run, ctx.attempt_cap, |_| true)
            .await
            .map_err(|e| store_err("survey", e))?
            .open;
        tracing::info!(%as_of, pairs = open.len(), "fetch phase");
        let bounds = Bounds {
            until: ctx.deadline,
            first_until: ctx.deadline,
        };
        if let Some(stop) = work_off(&pool, open, bounds).await {
            stopped(stop)?;
        }
    }
    finish(ctx, &pool.recorder, frozen).await
}

/// End the run when it is complete or its window has closed; otherwise
/// leave it open for the next start.
async fn finish<C: Clock>(
    ctx: &RunCtx<C>,
    recorder: &Recorder<C>,
    frozen: bool,
) -> Result<Outcome, Outcome> {
    let as_of = ctx.plan.as_of;
    let window = ctx.plan.window;
    let survey = survey(&ctx.store, ctx.plan.run_id, ctx.attempt_cap, |_| true)
        .await
        .map_err(|e| fail("survey", &e).on(as_of))?;
    let kind = if frozen && survey.open.is_empty() {
        recorder
            .end(Finish::Complete, None)
            .await
            .map_err(|e| fail("end_run", &e).on(as_of))?;
        if survey.exhausted == 0 {
            ExitKind::Complete
        } else {
            ExitKind::CompleteWithFailures
        }
    } else if ctx.deadline >= window.end() || ctx.clock.now() >= window.end() {
        // The deadline is the window's end: the work left cannot be done.
        ctx.clock.sleep_until(window.end()).await;
        recorder
            .end(Finish::Partial, Some("window closed"))
            .await
            .map_err(|e| fail("end_run", &e).on(as_of))?;
        ExitKind::Partial
    } else {
        tracing::info!(%as_of, pairs = survey.open.len(), frozen, "attempt timeout; the run stays open");
        return Ok(Outcome::of(ExitKind::Resumable, as_of));
    };
    let mut outcome = Outcome::of(kind, as_of);
    if kind != ExitKind::Complete {
        outcome.failure = ctx
            .store
            .dominant_failure(ctx.plan.run_id, &COUNTED)
            .await
            .map_err(|e| fail("dominant_failure", &e).on(as_of))?
            .map(|(class, code, _)| format!("{class}:{code}"));
    }
    tracing::info!(
        %as_of,
        kind = kind.as_str(),
        exhausted = survey.exhausted,
        open = survey.open.len(),
        failure = outcome.failure.as_deref().unwrap_or(""),
        "run ended"
    );
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_up_to_a_minute() {
        let waits: Vec<_> = (1..=8).map(|n| backoff(n).as_secs()).collect();
        assert_eq!(waits, [1, 2, 4, 8, 16, 32, 60, 60]);
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
