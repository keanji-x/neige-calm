//! The ordinary idle path sleeps between inputs. Unconverted active and
//! maintenance paths retain their existing cadence until their deadlines and
//! result-ready contracts are moved to this same loop.
use super::{HarnessPhaseTag, Inner};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Pacing from an actual pass, never a saved state or execution grant.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::harness::run_loop) enum Progress {
    Deferred,
    Observed,
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
        && header.compact_outcome.is_none()
        && header.interruption_intent.is_none()
        && header.pending_rewind.is_none();
    let delay = if ordinary_idle {
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
    tokio::time::Instant::now() + delay
}
