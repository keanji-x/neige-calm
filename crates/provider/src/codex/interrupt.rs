//! Native interrupt response evidence.
use super::*;

impl CodexAppServer {
    /// Capture the native response without erasing no-target evidence.
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
#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::{InterruptOutcome, SubmissionEvidence};

    #[test]
    fn m1_mapping_interrupt_response() {
        let cases = [
            (Ok(json!({})), "ack"),
            (
                Err(Error::Refused(
                    "turn/interrupt failed: no active turn to interrupt (code -32600)".into(),
                )),
                "none",
            ),
            (
                Err(Error::Refused(
                    "turn/interrupt failed: no active turn to interrupt (code -32601)".into(),
                )),
                "unknown",
            ),
            (Err(Error::Transport("connection lost".into())), "unknown"),
        ];
        for (input, fact) in cases {
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
        }
    }
}
