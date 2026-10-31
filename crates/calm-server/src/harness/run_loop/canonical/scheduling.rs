//! Wait for source/result readiness and the nearest owner deadline. Deferred
//! admission or recovery retains its short probe until that path is converted.
use super::{HarnessPhaseTag, Inner};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Pacing from an actual pass, never a saved state or execution grant.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::harness::run_loop) enum Progress {
    Deferred,
    Observed,
    Waiting,
}

pub(in crate::harness::run_loop) async fn advance(
    inner: &Arc<Inner>,
    ingress_empty: bool,
) -> Progress {
    inner
        .inputs
        .canonical()
        .expect("canonical consumer")
        .begin_progress();
    let progress = match super::advance_once(inner, ingress_empty).await {
        Ok(progress) => progress,
        Err(error) => {
            super::publish_issuance_error(inner, &error).await;
            tracing::warn!(%error, "canonical harness tick did not advance");
            Progress::Deferred
        }
    };
    #[cfg(feature = "fixtures")]
    crate::test_seams::pause_point("canonical-tick-returned", inner.card_id.as_str()).await;
    // Deferred maintenance is work even when the last published frame is idle.
    if ingress_empty {
        progress
    } else {
        Progress::Deferred
    }
}

pub(in crate::harness::run_loop) async fn next_probe(
    inner: &Inner,
    progress: Progress,
) -> tokio::time::Instant {
    let inputs = inner.inputs.canonical().expect("canonical consumer");
    let frame = inputs.committed().await;
    let checkpoint = &frame.snapshot.canonical_inputs;
    let header = &frame.snapshot.header;
    // An empty durable queue is not stable idle while a producer can still
    // commit. Keep handoffs on the existing short cadence in this slice.
    let ordinary_idle = progress == Progress::Observed
        && !inputs.sources.has_handoffs()
        && matches!(
            header.phase,
            HarnessPhaseTag::Idle | HarnessPhaseTag::TurnCompleted
        )
        && checkpoint.pending.is_empty()
        && checkpoint.active.is_none()
        && checkpoint.receipts.is_empty()
        && header.last_thread_id.is_some()
        && header.wedged_reason.is_none()
        && header.compact_outcome.as_ref().is_none_or(|outcome| {
            !super::super::compact_command::canonical::unresolved_maintenance(&outcome.status)
        })
        && header.interruption_intent.is_none()
        && header.pending_rewind.is_none();
    let delay = if progress == Progress::Waiting {
        #[cfg(feature = "fixtures")]
        crate::test_seams::pause_point("canonical-result-wait", inner.card_id.as_str()).await;
        // Provider owners wake the loop when qualified results become observable.
        // Reconciliation recovers lost hints; real owner deadlines preempt it below.
        Duration::from_secs(30)
    } else if ordinary_idle {
        #[cfg(feature = "fixtures")]
        crate::test_seams::pause_point("canonical-idle-wait", inner.card_id.as_str()).await;
        Duration::from_secs(30)
    } else {
        Duration::from_millis(50)
    };
    let delay = match *inner.issuance_retry_after.lock().await {
        Some(after) if after > Instant::now() => {
            delay.min(after.saturating_duration_since(Instant::now()))
        }
        _ => delay,
    };
    let probe = tokio::time::Instant::now() + delay;
    if progress != Progress::Waiting {
        return probe;
    }
    let state = inner.state.lock().await.clone();
    let budget = super::super::budget::next_deadline(inner, &state).await;
    let compact = super::super::compact_command::observation_deadline(&state, &inner.config);
    let interrupt = super::control::next_deadline(inner).await;
    [budget, compact, interrupt]
        .into_iter()
        .flatten()
        .map(tokio::time::Instant::from_std)
        .fold(probe, std::cmp::min)
}
