//! Start/abort cleanup retires retained driver handles and registered owner claims.
use super::*;

/// One exit for process-free preparation. Unconfirmed file cleanup discards the
/// preceding handle; process settlement is read from the provider claim and boot proof.
pub(super) fn finish_preparation<T, E>(
    slot: &mut Option<execution_end::DriverSlot>,
    result: std::result::Result<T, spawn::PreparationFailure<E>>,
) -> std::result::Result<T, E> {
    match result {
        Ok(prepared) => Ok(prepared),
        Err(spawn::PreparationFailure::Clean(error)) => Err(error),
        Err(spawn::PreparationFailure::Unconfirmed(error)) => {
            *slot = None;
            Err(error)
        }
    }
}

impl ClaudePlannerSession {
    /// Refusal still stops and settles the admitted execution before clearing its owner claim.
    pub(super) async fn abort_before_ok(
        &self,
        claim: &crate::provider_execution::Claim,
        instructions: InstructionsFile,
        error: CalmError,
        reason: crate::provider_execution::StopReason,
    ) -> (CalmError, bool) {
        use crate::provider_execution::{self as execution, KnownEnd};
        let params = &self.shared.params;
        let cleanup = async {
            let pool = params
                .repo
                .sqlite_db()
                .ok_or(CalmError::ExecutionUnavailable)?;
            let engine = execution::engine(params.engine.as_ref())?;
            let owner = &params.host.provider_execution;
            let id = execution::execution_id(claim.record())?;
            owner
                .stop(
                    &pool,
                    Some(engine),
                    &params.worker_session_id,
                    claim,
                    reason,
                )
                .await?;
            execution::wait_settled(engine, &id, SETTLE_AFTER_STOP).await?;
            instructions.remove()?;
            owner
                .finish(
                    &pool,
                    Some(engine),
                    &params.worker_session_id,
                    claim,
                    Some(KnownEnd::ClaudeRefused),
                )
                .await
        }
        .await;
        if let Err(error) = &cleanup {
            tracing::warn!(%error, "Claude refusal cleanup retained its claim");
        }
        (error, cleanup.is_ok())
    }
}
