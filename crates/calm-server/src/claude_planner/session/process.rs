//! Start/abort cleanup keeps settlement in the session's one driver slot.
use super::*;

/// Temporarily move the sole result out of the slot while stop is uncertain.
/// Error or cancellation drops it; successful stop restores it before any
/// process-free preparation, under the same issuance and driver locks.
pub(super) async fn stop_for_start(
    shared: &Shared,
    slot: &mut Option<execution_end::DriverSlot>,
) -> Result<()> {
    let previous = slot.take();
    stop(
        &shared.params.host.instance,
        &shared.params.worker_session_id,
    )
    .await?;
    *slot = previous;
    Ok(())
}

/// No child has been attempted. Only failed instructions cleanup can invalidate
/// the preceding driver's evidence; another marker stop is unnecessary.
pub(super) fn abort_preparation(
    slot: &mut Option<execution_end::DriverSlot>,
    instructions: InstructionsFile,
    error: CalmError,
) -> CalmError {
    if instructions.remove().is_err() {
        *slot = None;
    }
    drop(instructions);
    error
}

impl ClaudePlannerSession {
    /// Stop the marker (which ends a spawned child and anything it started), reap the child, and
    /// drop the instructions guard; the caller's error is what `turn_start` returns.
    pub(super) async fn abort_before_ok(
        &self,
        child: Option<Child>,
        instructions: InstructionsFile,
        error: CalmError,
    ) -> (CalmError, bool) {
        let params = &self.shared.params;
        let stopped = stop(&params.host.instance, &params.worker_session_id).await;
        if let Err(stop_error) = &stopped {
            tracing::warn!(
                worker_session_id = %params.worker_session_id,
                error = %stop_error,
                "claude planner: stop after a failed turn start did not confirm"
            );
        }
        let reaped = match child {
            Some(mut child) => {
                let _ = child.start_kill();
                matches!(
                    tokio::time::timeout(Duration::from_secs(5), child.wait()).await,
                    Ok(Ok(_))
                )
            }
            None => true,
        };
        let removed = instructions.remove().is_ok();
        drop(instructions);
        (error, stopped.is_ok() && reaped && removed)
    }
}
