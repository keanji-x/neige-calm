//! Native interrupt response evidence.
use super::*;

impl CodexAppServer {
    /// Capture the native response without erasing no-target evidence.
    pub async fn interrupt_outcome(
        &self,
        thread_id: &str,
        turn_id: &str,
    ) -> crate::control::NativeInterruptOutcome<Error> {
        self.interrupt_outcome_gated(
            thread_id,
            turn_id,
            &PreSendGate::new(gated_requests::allow_send),
        )
        .await
    }

    pub async fn interrupt_outcome_gated(
        &self,
        thread_id: &str,
        turn_id: &str,
        gate: &PreSendGate<'_>,
    ) -> crate::control::NativeInterruptOutcome<Error> {
        let response: Result<Value> = self
            .request_gated(
                "turn/interrupt",
                json!({ "threadId": thread_id, "turnId": turn_id }),
                gate,
            )
            .await;
        interrupt_response(response)
    }
}

fn interrupt_response(response: Result<Value>) -> crate::control::NativeInterruptOutcome<Error> {
    use crate::control::NativeInterruptOutcome;
    match response {
        Ok(_) => NativeInterruptOutcome::NativeAck,
        Err(Error::Refused(message))
            if message == "turn/interrupt failed: no active turn to interrupt (code -32600)" =>
        {
            NativeInterruptOutcome::NoTarget
        }
        Err(error @ (Error::Refused(_) | Error::Transport(_) | Error::Serde(_))) => {
            NativeInterruptOutcome::Unconfirmed { error }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::NativeInterruptOutcome;

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
                (NativeInterruptOutcome::NativeAck, "ack")
                | (NativeInterruptOutcome::NoTarget, "none") => {}
                (NativeInterruptOutcome::Unconfirmed { error }, "unknown") => {
                    assert_eq!(Some(error.to_string()), original);
                }
                other => panic!("wrong mapping: {other:?}"),
            }
        }
    }
}
