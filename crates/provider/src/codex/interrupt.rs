//! Native interrupt evidence and the legacy projection.
use super::*;

impl CodexAppServer {
    pub async fn turn_interrupt(&self, thread_id: &str, turn_id: &str) -> Result<()> {
        legacy_result(self.interrupt_outcome(thread_id, turn_id).await)
    }

    /// Capture the native response before the legacy no-target success projection erases it.
    pub async fn interrupt_outcome(
        &self,
        thread_id: &str,
        turn_id: &str,
    ) -> crate::control::InterruptOutcome<Error> {
        let response: Result<Value> = self
            .request(
                "turn/interrupt",
                json!({ "threadId": thread_id, "turnId": turn_id }),
            )
            .await;
        interrupt_response(response)
    }
}

fn interrupt_response(response: Result<Value>) -> crate::control::InterruptOutcome<Error> {
    use crate::control::{InterruptOutcome, SubmissionEvidence};
    match response {
        Ok(_) => InterruptOutcome::NativeAck,
        Err(Error::Refused(message))
            if message == "turn/interrupt failed: no active turn to interrupt (code -32600)" =>
        {
            InterruptOutcome::NoTarget
        }
        Err(error @ (Error::Refused(_) | Error::Transport(_) | Error::Serde(_))) => {
            InterruptOutcome::Unconfirmed {
                error,
                evidence: SubmissionEvidence::Unobserved,
            }
        }
    }
}

/// The old Codex contract folds only ack and no-target to success; it propagates the same error.
pub fn legacy_result<E>(
    outcome: crate::control::InterruptOutcome<E>,
) -> std::result::Result<(), E> {
    match outcome {
        crate::control::InterruptOutcome::Unconfirmed { error, .. } => Err(error),
        crate::control::InterruptOutcome::NativeAck
        | crate::control::InterruptOutcome::NoTarget
        | crate::control::InterruptOutcome::LocalSignalDelivered => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::{InterruptOutcome, SubmissionEvidence};

    #[test]
    fn m1_mapping_interrupt_response_and_old_projection() {
        let cases = [
            (Ok(json!({})), "ack", true),
            (
                Err(Error::Refused(
                    "turn/interrupt failed: no active turn to interrupt (code -32600)".into(),
                )),
                "none",
                true,
            ),
            (
                Err(Error::Refused(
                    "turn/interrupt failed: no active turn to interrupt (code -32601)".into(),
                )),
                "unknown",
                false,
            ),
            (
                Err(Error::Transport("connection lost".into())),
                "unknown",
                false,
            ),
        ];
        for (input, fact, old_ok) in cases {
            let original = input.as_ref().err().map(ToString::to_string);
            let outcome = interrupt_response(input);
            match (&outcome, fact) {
                (InterruptOutcome::NativeAck, "ack") | (InterruptOutcome::NoTarget, "none") => {}
                (InterruptOutcome::Unconfirmed { error, evidence }, "unknown") => {
                    assert_eq!(*evidence, SubmissionEvidence::Unobserved);
                    assert_eq!(Some(error.to_string()), original);
                }
                other => panic!("wrong mapping: {other:?}"),
            }
            assert_eq!(legacy_result(outcome).is_ok(), old_ok);
        }
    }
}
