//! Spawn-source facts shared by ordinary turns and compaction, with their distinct projections.
use crate::error::CalmError;
use crate::harness::backend::{StartDiagnostic, TurnStartFailure};
use provider::control::*;

pub(crate) enum SpawnFailure {
    Host(TurnStartFailure),
    Failed { error: CalmError, line_sent: bool },
}
impl From<TurnStartFailure> for SpawnFailure {
    fn from(failure: TurnStartFailure) -> Self {
        Self::Host(failure)
    }
}
impl From<CalmError> for SpawnFailure {
    fn from(error: CalmError) -> Self {
        Self::Host(error.into())
    }
}
impl SpawnFailure {
    /// Compaction still owns its pre-write restore / possible-write wedge decision.
    pub(crate) fn into_legacy(self) -> (TurnStartFailure, bool) {
        match self {
            Self::Host(failure) => (failure, false),
            Self::Failed { error, line_sent } => (error.into(), line_sent),
        }
    }
}
pub(super) fn start_result(
    result: Result<String, SpawnFailure>,
) -> Result<TurnStartOutcome<CalmError, StartDiagnostic>, TurnStartFailure> {
    match result {
        Ok(turn) => Ok(TurnStartOutcome::Started {
            turn: TurnIdentity::Local(CorrelationKey(turn)),
        }),
        Err(SpawnFailure::Host(failure)) => Err(failure),
        Err(SpawnFailure::Failed { error, .. }) => Ok(TurnStartOutcome::Failed { error }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn m1_mapping_claude_spawn_facts_preserve_ordinary_retry_and_compact_evidence() {
        for case in [
            "host-transient",
            "host-refused",
            "host-recovery",
            "preparation",
            "spawn",
            "line",
            "success",
        ] {
            let failure = || match case {
                "host-transient" => SpawnFailure::Host(TurnStartFailure::Transient(
                    CalmError::Conflict("original".into()),
                )),
                "host-refused" => SpawnFailure::Host(TurnStartFailure::Refused {
                    error: CalmError::Conflict("original".into()),
                    reader: "operator wording".into(),
                }),
                "host-recovery" => SpawnFailure::Host(TurnStartFailure::AwaitingRecovery {
                    error: CalmError::Conflict("original".into()),
                    reader: "hold wording".into(),
                }),
                "preparation" => SpawnFailure::Failed {
                    error: CalmError::Conflict("original".into()),
                    line_sent: false,
                },
                "spawn" | "line" => SpawnFailure::Failed {
                    error: CalmError::Conflict("original".into()),
                    line_sent: case == "line",
                },
                _ => unreachable!(),
            };
            let result = if case == "success" {
                start_result(Ok("local-turn".into()))
            } else {
                start_result(Err(failure()))
            };
            match (&result, case) {
                (
                    Ok(TurnStartOutcome::Started {
                        turn: TurnIdentity::Local(key),
                    }),
                    "success",
                ) => assert_eq!(key.0, "local-turn"),
                (Ok(TurnStartOutcome::Failed { .. }), "preparation") => {}
                (Ok(TurnStartOutcome::Failed { .. }), "spawn" | "line") => {}
                (Err(_), "host-transient" | "host-refused" | "host-recovery") => {}
                other => panic!("wrong fact {other:?}"),
            }
            let old = crate::harness::backend::ordinary_start_result(result);
            match (case, old) {
                ("success", Ok(turn)) => assert_eq!(turn, "local-turn"),
                ("host-refused", Err(TurnStartFailure::Refused { reader, .. })) => {
                    assert_eq!(reader, "operator wording")
                }
                ("host-recovery", Err(TurnStartFailure::AwaitingRecovery { reader, .. })) => {
                    assert_eq!(reader, "hold wording")
                }
                (_, Err(TurnStartFailure::Transient(CalmError::Conflict(message)))) => {
                    assert_eq!(message, "original")
                }
                other => panic!("wrong old projection {other:?}"),
            }
            if case != "success" {
                assert_eq!(failure().into_legacy().1, case == "line");
            }
        }
    }
}
