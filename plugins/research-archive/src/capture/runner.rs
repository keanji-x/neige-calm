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
//! `ok` constituents row, or once `now ≥ carry_deadline`, and before any
//! per-key fetch. The membership and coverage rules, and how the freeze
//! treats an attempt still in flight, are in [`super::schedule`]. When this
//! start's deadline comes before the carry deadline, the run is left
//! unfrozen for the next start.
//!
//! **The end.** A run ends `complete` only when it is frozen and every pair
//! has an `ok` or is at its cap. With work left, it ends `partial` once the
//! window closes, and otherwise stays open for the next start.

use std::sync::Arc;

use super::fail;
use super::latch::{Latch, Stop};
use super::schedule::{COUNTED, Pool, Rule, Span, survey, work_off};
use crate::catalog::EndpointId;
use crate::clock::Clock;
use crate::source::LongbridgeSource;
use crate::state::{ExitKind, Outcome};
use crate::store::RunPlan;
use crate::types::{Finish, Key, Recorded, Response, RunId, UnixMs, Window, degraded};
use crate::{Store, StoreError};

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

/// What [`Recorder::observe`] did.
pub(super) enum Observed {
    Recorded(Recorded),
    /// The window has closed: nothing was written.
    WindowClosed,
}

impl<C: Clock> Recorder<C> {
    /// Record a response that has arrived, stamped when it has its turn.
    /// A response that arrived is recorded even after the run stopped: the
    /// stop governs sending, not data already received.
    pub(super) async fn observe(
        &self,
        key: &Key,
        endpoint: EndpointId,
        response: &Response,
    ) -> Result<Observed, StoreError> {
        let _turn = self.turn.lock().await;
        let at = self.clock.now();
        if !self.window.contains(at) {
            return Ok(Observed::WindowClosed);
        }
        self.store
            .record_observation(self.run, key, endpoint, at, response)
            .await
            .map(Observed::Recorded)
    }

    /// The run's window.
    pub(super) fn window(&self) -> Window {
        self.window
    }

    /// Freeze the membership now. Returns the freeze's stamp, or `None` when
    /// the window has closed.
    pub(super) async fn freeze(&self) -> Result<Option<UnixMs>, StoreError> {
        let _turn = self.turn.lock().await;
        let at = self.clock.now();
        if !self.window.contains(at) {
            return Ok(None);
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
        Ok(Some(at))
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
        latch: Arc::new(Latch::new()),
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
    });
    let span = Span {
        deadline: ctx.deadline,
        carry_deadline: ctx.carry_deadline,
        cap: ctx.attempt_cap,
    };
    let frozen = ctx.plan.frozen || ctx.plan.indexes.is_empty();
    tracing::info!(%as_of, frozen, "working the run");
    let worked = work_off(&pool, &ctx.store, run, frozen, span)
        .await
        .map_err(|e| store_err("working the run", e))?;
    match worked.stop {
        Some(Stop::Auth) => {
            tracing::error!(%as_of, "a request found no usable token; the run stays open");
            return Err(Outcome::of(ExitKind::AuthUnavailable, as_of));
        }
        Some(Stop::Fatal(err)) => {
            tracing::error!(%as_of, error = %err, "recording failed; the run stays open");
            return Err(Outcome::of(ExitKind::Fatal, as_of));
        }
        // The end below finds the window closed.
        Some(Stop::WindowClosed) | None => {}
    }
    let frozen = worked.frozen;
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
    let frozen_at = ctx
        .store
        .frozen_at(ctx.plan.run_id)
        .await
        .map_err(|e| fail("survey", &e).on(as_of))?;
    let survey = survey(
        &ctx.store,
        ctx.plan.run_id,
        Rule::Coverage {
            cap: ctx.attempt_cap,
            since: frozen_at.map(|at| (ctx.store.catalog().membership(), at)),
        },
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
