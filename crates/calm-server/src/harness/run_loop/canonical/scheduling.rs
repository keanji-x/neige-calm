//! Wait for source/result readiness and the nearest owner deadline. Deferred
//! admission or recovery retains its short probe until that path is converted.
use super::{HarnessPhaseTag, Inner};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Spread recovery reads across a restarted cohort; normal progress still uses
/// signals and real owner deadlines. The maximum missed-hint wait stays 30 s.
fn recovery_interval(worker: &str) -> Duration {
    let bucket = worker.bytes().fold(0u64, |hash, byte| {
        hash.wrapping_mul(31).wrapping_add(u64::from(byte))
    }) % 5_001;
    Duration::from_millis(25_000 + bucket)
}

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
        && header.last_thread_id.is_some()
        && header.wedged_reason.is_none()
        && header.compact_outcome.as_ref().is_none_or(|outcome| {
            !super::super::compact_command::canonical::unresolved_maintenance(&outcome.status)
        })
        && header.interruption_intent.is_none()
        && header.pending_rewind.is_none();
    let parked = inner.parked.is_parked();
    // Deferred work can live outside the input checkpoint (e.g. maintenance).
    // Only an actual observation or an explicitly parked owner may sleep.
    let short_retry = !parked
        && (inputs.sources.has_handoffs() || (progress != Progress::Waiting && !ordinary_idle));
    if progress == Progress::Waiting {
        #[cfg(feature = "fixtures")]
        crate::test_seams::pause_point("canonical-result-wait", inner.card_id.as_str()).await;
    } else if ordinary_idle {
        #[cfg(feature = "fixtures")]
        crate::test_seams::pause_point("canonical-idle-wait", inner.card_id.as_str()).await;
    }
    let now = Instant::now();
    let retry = inner
        .issuance_retry_after
        .lock()
        .await
        .filter(|after| *after > now);
    let probe = match retry {
        Some(after) => tokio::time::Instant::from_std(after),
        None => {
            tokio::time::Instant::now()
                + if short_retry {
                    Duration::from_millis(50)
                } else {
                    recovery_interval(&inner.worker_session_id)
                }
        }
    };
    #[cfg(feature = "fixtures")]
    crate::test_seams::pause_point("canonical-recovery-probe-ready", inner.card_id.as_str()).await;
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
        // A failed attempt has its own retry, and a park cannot act on expiry.
        // Future deadlines still preempt either wait; expired ones do not spin.
        .filter(|deadline| !((retry.is_some() || parked) && *deadline <= now))
        .map(tokio::time::Instant::from_std)
        .fold(probe, std::cmp::min)
}
