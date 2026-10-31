//! The ordinary idle path sleeps between inputs. Unconverted active and
//! maintenance paths retain their existing cadence until their deadlines and
//! result-ready contracts are moved to this same loop.
use super::*;

pub(in crate::harness::run_loop) async fn next_probe(inner: &Inner) -> tokio::time::Instant {
    let frame = inner
        .inputs
        .canonical()
        .expect("canonical consumer")
        .committed()
        .await;
    let checkpoint = &frame.snapshot.canonical_inputs;
    let header = &frame.snapshot.header;
    let ordinary_idle = matches!(
        header.phase,
        HarnessPhaseTag::Idle | HarnessPhaseTag::TurnCompleted
    ) && checkpoint.pending.is_empty()
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
