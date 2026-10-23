use crate::db::Repo;
use crate::error::Result;
use crate::harness::{HarnessPhaseTag, HarnessSnapshot, effective_runtime_thread_id, turn_outcome};
use crate::session_projection_repo::WorkerSessionProjection;

/// #1791 §5.1 item 5: no Claude Planner process survives a restart (the boot sweep ends it), so
/// the snapshot's last turn is `interrupted` unless it already settled; the write is idempotent.
/// Only a snapshot whose phase says a turn may still have been running names such a turn: a
/// settled phase means the outcome was recorded (a reset's transcript clear may since have removed
/// it, which is no reason to invent one), and a turn whose id never reached the snapshot commit
/// left the pre-drain snapshot, whose batch re-drains.
pub(crate) async fn record_interrupted_turn(
    repo: &dyn Repo,
    runtime: &WorkerSessionProjection,
    track_id: &str,
    snapshot: &HarnessSnapshot,
) -> Result<()> {
    if !matches!(
        snapshot.phase,
        HarnessPhaseTag::TurnRunning | HarnessPhaseTag::IssuingInterrupt | HarnessPhaseTag::Resumed
    ) {
        return Ok(());
    }
    let (Some(turn_id), Some(thread_id)) = (
        snapshot.last_turn_id.as_deref(),
        effective_runtime_thread_id(runtime),
    ) else {
        return Ok(());
    };
    let turn = serde_json::json!({
        "id": turn_id,
        "status": "interrupted",
        "error": {"message": "neige restarted during this turn"},
    });
    turn_outcome::record(
        repo,
        &runtime.id,
        &runtime.card_id,
        track_id,
        &thread_id,
        turn_id,
        &turn,
        crate::harness::turn_outcome::Confirmation::Unconfirmed,
    )
    .await?;
    Ok(())
}

/// Canonical recovery preserves the original checkpoint, including uncertainty.
/// Stop and settle the registered provider execution before reopening. This does
/// not assert input acceptance; opening is left to the factory.
pub(crate) async fn quiesce_canonical(
    host: &super::config::ClaudePlannerHost,
    worker_session_id: &str,
) -> Result<()> {
    super::lifecycle::stop_session(
        host,
        worker_session_id,
        crate::provider_execution::StopReason::KernelRestarted,
    )
    .await
}
