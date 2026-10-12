//! The run phases after the plan (#2575, "Run phases" 2-4): membership, the
//! freeze, the fetch, and how the run ends. The retry schedule they share
//! is [`super::schedule`].
//!
//! **One serialized recorder.** Responses of concurrent requests are
//! recorded by [`Recorder`], which takes `fetched_at` from the clock *after*
//! it has its turn and writes under that same turn. So `fetched_at` is the
//! wall time the response was recorded, and the order of the stamps is the
//! order of the commits: a run's clock never goes back (#2577 item 2). The
//! freeze and the end take their timestamps the same way.
//!
//! **The freeze.** Membership is frozen when every planned index has an
//! `ok` constituents row, or once `now ≥ carry_deadline`. Before the carry
//! deadline the constituents keep being retried on the schedule; the cap
//! only limits counted attempts, and an index at its cap waits for the
//! carry deadline. When the carry deadline falls inside this start, the
//! start waits for it; when this start's deadline comes first, the run is
//! left unfrozen for the next start. No per-key fetch starts before the
//! freeze.
//!
//! **The end.** A run ends `complete` only when it is frozen and every pair
//! has an `ok` or is at its cap. With work left, it ends `partial` once the
//! window closes, and otherwise stays open for the next start.

use std::sync::{Arc, Mutex};

use super::fail;
use super::schedule::{Bounds, COUNTED, Pool, Stop, survey, work_off};
use crate::catalog::EndpointId;
use crate::clock::Clock;
use crate::source::LongbridgeSource;
use crate::state::{ExitKind, Outcome};
use crate::store::{PairCoverage, RunPlan};
use crate::types::{Finish, Key, Recorded, Response, RunId, UnixMs, Window};
use crate::{Store, StoreError};

/// A run that ends `complete` with fewer than this share of its planned
/// pairs having an `ok` exits `DEGRADED` (non-zero), so the alert fires.
/// Ordinary losses (a few delisted symbols, a few capped pairs) stay above
/// it and exit 0 as `complete_with_failures`.
pub const DEGRADED_BELOW: f64 = 0.5;

/// Whether `ok` of `planned` pairs is below [`DEGRADED_BELOW`].
pub fn degraded(ok: usize, planned: usize) -> bool {
    planned > 0 && (ok as f64) < DEGRADED_BELOW * planned as f64
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
pub(super) struct Recorder<C: Clock> {
    clock: Arc<C>,
    store: Store,
    run: RunId,
    window: Window,
    turn: tokio::sync::Mutex<()>,
}

impl<C: Clock> Recorder<C> {
    /// Record a response, stamped now. `None` when the window has closed:
    /// nothing is written.
    pub(super) async fn observe(
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
        halted: tokio::sync::watch::Sender::new(false),
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
        let only_membership = move |p: &PairCoverage| p.endpoint == membership;
        let indexes = survey(
            &ctx.store,
            run,
            ctx.attempt_cap,
            ctx.clock.now(),
            only_membership,
        )
        .await
        .map_err(|e| store_err("survey", e))?;
        tracing::info!(%as_of, indexes = indexes.pending.len(), "membership phase");
        let bounds = Bounds {
            until: ctx.deadline.min(ctx.carry_deadline),
            first_until: ctx.deadline,
        };
        if let Some(stop) = work_off(&pool, indexes.pending, bounds).await {
            stopped(stop)?;
        }
        let all_ok = survey(
            &ctx.store,
            run,
            ctx.attempt_cap,
            ctx.clock.now(),
            only_membership,
        )
        .await
        .map_err(|e| store_err("survey", e))?
        .all_ok();
        if pool.stopped().is_none() && !all_ok && ctx.carry_deadline < ctx.deadline {
            // The carry deadline falls inside this start: wait for it.
            ctx.clock.sleep_until(ctx.carry_deadline).await;
        }
        if pool.stopped().is_none() && (all_ok || ctx.clock.now() >= ctx.carry_deadline) {
            frozen = pool
                .recorder
                .freeze()
                .await
                .map_err(|e| store_err("freeze", e))?;
        }
    }
    if frozen && pool.stopped().is_none() {
        let pairs = survey(&ctx.store, run, ctx.attempt_cap, ctx.clock.now(), |_| true)
            .await
            .map_err(|e| store_err("survey", e))?;
        tracing::info!(%as_of, pairs = pairs.pending.len(), "fetch phase");
        let bounds = Bounds {
            until: ctx.deadline,
            first_until: ctx.deadline,
        };
        if let Some(stop) = work_off(&pool, pairs.pending, bounds).await {
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
    let survey = survey(
        &ctx.store,
        ctx.plan.run_id,
        ctx.attempt_cap,
        ctx.clock.now(),
        |_| true,
    )
    .await
    .map_err(|e| fail("survey", &e).on(as_of))?;
    let kind = if frozen && survey.pending.is_empty() {
        recorder
            .end(Finish::Complete, None)
            .await
            .map_err(|e| fail("end_run", &e).on(as_of))?;
        let planned = survey.ok + survey.capped;
        if survey.capped == 0 {
            ExitKind::Complete
        } else if degraded(survey.ok, planned) {
            ExitKind::Degraded
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
        tracing::info!(%as_of, pairs = survey.pending.len(), frozen, "attempt timeout; the run stays open");
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
        ok = survey.ok,
        capped = survey.capped,
        pending = survey.pending.len(),
        failure = outcome.failure.as_deref().unwrap_or(""),
        "run ended"
    );
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn degraded_is_strictly_below_half() {
        assert!(degraded(2, 6));
        assert!(!degraded(3, 6));
        assert!(degraded(0, 1));
        assert!(!degraded(1, 1));
        assert!(!degraded(0, 0));
    }
}
