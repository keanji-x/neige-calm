//! ACP start keeps durable admission separate from native setup facts.
use crate::error::CalmError;
use crate::harness::backend::{StartDiagnostic, TurnStartFailure};
use provider::control::*;

#[derive(Debug)]
pub(super) enum StartFailure {
    Host(TurnStartFailure),
    Failed(CalmError),
}
impl From<CalmError> for StartFailure {
    fn from(error: CalmError) -> Self {
        Self::Host(error.into())
    }
}
impl From<TurnStartFailure> for StartFailure {
    fn from(error: TurnStartFailure) -> Self {
        Self::Host(error)
    }
}
pub(super) fn start_result(
    result: Result<String, StartFailure>,
) -> Result<TurnStartOutcome<CalmError, StartDiagnostic>, TurnStartFailure> {
    match result {
        // Includes exact receipt replay (zero prompt writes) and a failed post-receipt dispatch.
        Ok(turn) => Ok(TurnStartOutcome::Started {
            turn: TurnIdentity::Local(CorrelationKey(turn)),
        }),
        Err(StartFailure::Host(error)) => Err(error),
        Err(StartFailure::Failed(error)) => Ok(TurnStartOutcome::Failed { error }),
    }
}

// Readiness/catalog discovery keeps its existing CalmError result. Managed issuance instead
// splits this source-local domain into the outer host failure and the neutral operation fact.
impl From<StartFailure> for CalmError {
    fn from(failure: StartFailure) -> Self {
        match failure {
            StartFailure::Host(
                TurnStartFailure::Transient(error)
                | TurnStartFailure::Refused { error, .. }
                | TurnStartFailure::AwaitingRecovery { error, .. },
            )
            | StartFailure::Failed(error) => error,
        }
    }
}

/// Preserve the existing durable ACP receipt codec independently of native prompt encoding.
pub(super) fn receipt_input(items: &[provider::InputItem]) -> serde_json::Value {
    serde_json::Value::Array(
        items
            .iter()
            .map(|item| match item {
                provider::InputItem::Text { text } => {
                    serde_json::json!({"type":"text","text":text})
                }
                provider::InputItem::LocalImage { path } => {
                    serde_json::json!({"type":"localImage","path":path})
                }
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn m1_mapping_acp_receipt_preserves_input_codec() {
        assert_eq!(
            receipt_input(&[
                provider::InputItem::text("before"),
                provider::InputItem::local_image("/w/a.png"),
                provider::InputItem::text("after"),
            ]),
            serde_json::json!([
                {"type":"text","text":"before"},
                {"type":"localImage","path":"/w/a.png"},
                {"type":"text","text":"after"},
            ])
        );
    }
    #[test]
    fn m1_mapping_acp_start_keeps_receipt_ownership_and_setup_policy() {
        for case in [
            "registered",
            "receipt-replay",
            "dispatch-failed",
            "host-refused",
            "host-transient",
            "setup",
            "spawn",
        ] {
            let result = match case {
                "registered" | "receipt-replay" | "dispatch-failed" => Ok("durable-turn".into()),
                "host-refused" => Err(StartFailure::Host(TurnStartFailure::Refused {
                    error: CalmError::Conflict("reset needed".into()),
                    reader: "no resend".into(),
                })),
                "host-transient" => Err(StartFailure::Host(
                    CalmError::Conflict("original".into()).into(),
                )),
                "setup" => Err(StartFailure::Failed(CalmError::Conflict("original".into()))),
                "spawn" => Err(StartFailure::Failed(CalmError::Conflict("original".into()))),
                _ => unreachable!(),
            };
            let mapped = start_result(result);
            match (&mapped, case) {
                (
                    Ok(TurnStartOutcome::Started {
                        turn: TurnIdentity::Local(key),
                    }),
                    "registered" | "receipt-replay" | "dispatch-failed",
                ) => assert_eq!(key.0, "durable-turn"),
                (Ok(TurnStartOutcome::Failed { .. }), "setup") => {}
                (Ok(TurnStartOutcome::Failed { .. }), "spawn") => {}
                (Err(_), "host-refused" | "host-transient") => {}
                other => panic!("wrong fact {other:?}"),
            }
            match (case, crate::harness::backend::ordinary_start_result(mapped)) {
                ("registered" | "receipt-replay" | "dispatch-failed", Ok(turn)) => {
                    assert_eq!(turn, "durable-turn")
                }
                (
                    "host-refused",
                    Err(TurnStartFailure::Refused {
                        reader,
                        error: CalmError::Conflict(message),
                    }),
                ) => {
                    assert_eq!(reader, "no resend");
                    assert_eq!(message, "reset needed");
                }
                (
                    "host-transient" | "setup" | "spawn",
                    Err(TurnStartFailure::Transient(CalmError::Conflict(message))),
                ) => assert_eq!(message, "original"),
                other => panic!("wrong old result {other:?}"),
            }
        }
    }
}
