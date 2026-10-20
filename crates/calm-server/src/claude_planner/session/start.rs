//! Spawn-source facts shared by ordinary turns and compaction, with their distinct projections.
use crate::error::CalmError;
use crate::harness::backend::{StartDiagnostic, TurnStartFailure};
use provider::control::*;

pub(crate) enum SpawnFailure {
    BeforeWrite(BeforeWriteFailure),
    CrossedWrite {
        error: CalmError,
        local_turn: uuid::Uuid,
    },
}
pub(crate) enum BeforeWriteFailure {
    Host(TurnStartFailure),
    Failed(CalmError),
}
impl From<TurnStartFailure> for SpawnFailure {
    fn from(failure: TurnStartFailure) -> Self {
        Self::BeforeWrite(BeforeWriteFailure::Host(failure))
    }
}
impl From<CalmError> for SpawnFailure {
    fn from(error: CalmError) -> Self {
        TurnStartFailure::from(error).into()
    }
}
impl SpawnFailure {
    pub(super) fn failed(error: CalmError) -> Self {
        Self::BeforeWrite(BeforeWriteFailure::Failed(error))
    }
}
pub(super) fn start_result(
    result: Result<uuid::Uuid, SpawnFailure>,
) -> Result<TurnStartOutcome<CalmError, StartDiagnostic>, TurnStartFailure> {
    match result {
        Ok(turn) => Ok(TurnStartOutcome::Started {
            turn: TurnIdentity::Local(CorrelationKey(turn.to_string())),
        }),
        Err(SpawnFailure::BeforeWrite(BeforeWriteFailure::Host(failure))) => Err(failure),
        Err(SpawnFailure::BeforeWrite(BeforeWriteFailure::Failed(error))) => {
            Ok(TurnStartOutcome::Failed { error })
        }
        Err(SpawnFailure::CrossedWrite { error, local_turn }) => {
            tracing::warn!(%local_turn, %error, "Claude invocation crossed stdin before failing");
            Ok(TurnStartOutcome::Failed { error })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn error() -> CalmError {
        CalmError::Conflict("original diagnostic".into())
    }

    #[test]
    fn m1_mapping_claude_spawn_facts_preserve_ordinary_retry_and_compact_evidence() {
        for failure in [
            SpawnFailure::failed(error()),
            SpawnFailure::CrossedWrite {
                error: error(),
                local_turn: uuid::Uuid::new_v4(),
            },
        ] {
            let result = start_result(Err(failure)).unwrap();
            assert!(
                matches!(result, TurnStartOutcome::Failed { error: CalmError::Conflict(message) } if message == "original diagnostic")
            );
        }
        let turn = uuid::Uuid::new_v4();
        let result = start_result(Ok(turn)).unwrap();
        assert!(
            matches!(result, TurnStartOutcome::Started { turn: TurnIdentity::Local(key) } if key.0 == turn.to_string())
        );
        assert!(
            matches!(start_result(Err(TurnStartFailure::Transient(error()).into())), Err(TurnStartFailure::Transient(CalmError::Conflict(message))) if message == "original diagnostic")
        );
        assert!(
            matches!(start_result(Err(TurnStartFailure::Refused { error: error(), reader: "operator wording".into() }.into())), Err(TurnStartFailure::Refused { error: CalmError::Conflict(message), reader }) if message == "original diagnostic" && reader == "operator wording")
        );
        let recovery = TurnStartFailure::AwaitingRecovery {
            error: error(),
            reader: "hold wording".into(),
        };
        assert!(
            matches!(start_result(Err(recovery.into())), Err(TurnStartFailure::AwaitingRecovery { error: CalmError::Conflict(message), reader }) if message == "original diagnostic" && reader == "hold wording")
        );
    }
}
