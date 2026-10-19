//! Failure facts captured before native control errors enter the application error domain.
use crate::error::CalmError;

/// Only the shared daemon's control producers choose these operation facts. The payload is an
/// opaque diagnostic; consumers must never reclassify it by inspecting `CalmError`.
#[derive(Debug)]
pub enum ControlError {
    Refused(CalmError),
    Unanswered(CalmError),
}

impl From<provider::codex::error::Error> for ControlError {
    fn from(error: provider::codex::error::Error) -> Self {
        use provider::codex::error::Error;
        match error {
            error @ Error::Refused(_) => Self::Refused(error.into()),
            error @ (Error::Transport(_) | Error::Serde(_)) => Self::Unanswered(error.into()),
        }
    }
}

impl From<ControlError> for CalmError {
    fn from(error: ControlError) -> Self {
        match error {
            ControlError::Refused(error) | ControlError::Unanswered(error) => error,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn m1_mapping_native_control_errors_are_classified_before_widening() {
        use provider::codex::error::Error;
        let decode = serde_json::from_str::<bool>("invalid").unwrap_err();
        let decode_text = decode.to_string();
        for (native, refused, expected) in [
            (
                Error::Refused("original refusal".into()),
                true,
                "codex refused: original refusal".to_owned(),
            ),
            (
                Error::Transport("original transport".into()),
                false,
                "codex app-server: original transport".to_owned(),
            ),
            (Error::Serde(decode), false, format!("serde: {decode_text}")),
        ] {
            let fact = ControlError::from(native);
            match &fact {
                ControlError::Refused(error) => {
                    assert!(refused);
                    assert!(matches!(error, CalmError::CodexRefused(_)));
                }
                ControlError::Unanswered(error) => {
                    assert!(!refused);
                    assert!(matches!(
                        error,
                        CalmError::CodexAppServer(_) | CalmError::Serde(_)
                    ));
                }
            }
            let diagnostic = CalmError::from(fact);
            assert_eq!(diagnostic.to_string(), expected);
        }
    }
}

/// Start facts captured at the daemon source, before widening to CalmError.
#[derive(Debug)]
pub enum StartError {
    Host(CalmError),
    Refused(CalmError),
    RecoveryRequired(CalmError),
    Unanswered(CalmError),
}
impl From<provider::codex::error::Error> for StartError {
    fn from(error: provider::codex::error::Error) -> Self {
        ControlError::from(error).into()
    }
}
impl From<ControlError> for StartError {
    fn from(error: ControlError) -> Self {
        match error {
            ControlError::Refused(error) => Self::Refused(error),
            ControlError::Unanswered(error) => Self::Unanswered(error),
        }
    }
}
impl From<StartError> for CalmError {
    fn from(error: StartError) -> Self {
        match error {
            StartError::Host(error)
            | StartError::Refused(error)
            | StartError::RecoveryRequired(error)
            | StartError::Unanswered(error) => error,
        }
    }
}

use super::*;

impl SharedCodexAppServer {
    pub async fn interrupt_outcome(
        &self,
        thread_id: &str,
        turn_id: &str,
    ) -> provider::control::InterruptOutcome<CalmError> {
        self.interrupt_control(thread_id, turn_id)
            .await
            .map_error(Into::into)
    }

    pub(super) async fn interrupt_control(
        &self,
        thread_id: &str,
        turn_id: &str,
    ) -> provider::control::InterruptOutcome<control::StartError> {
        #[cfg(feature = "fixtures")]
        if let Some(fake) = self.fake.as_ref() {
            fake.interrupted_turns
                .lock()
                .expect("fake shared codex interrupted turns mutex poisoned")
                .push((thread_id.to_string(), turn_id.to_string()));
            let hold = fake
                .interrupt_hold
                .lock()
                .expect("interrupt hold poisoned")
                .clone();
            if let Some((entered, release)) = hold {
                entered.notify_one();
                release.notified().await;
            }
            if fake.fail_turn_interrupt.load(Ordering::SeqCst) {
                return provider::control::InterruptOutcome::Unconfirmed {
                    error: control::StartError::Unanswered(CalmError::CodexAppServer(
                        "fixture: turn/interrupt failed".into(),
                    )),
                    evidence: provider::control::SubmissionEvidence::Unobserved,
                };
            }
            self.active_turns
                .remove_if(thread_id, |_, active| active == turn_id);
            return provider::control::InterruptOutcome::NativeAck;
        }
        let client = match self.connected_client().await {
            Ok(client) => client,
            Err(error @ (ControlError::Refused(_) | ControlError::Unanswered(_))) => {
                return provider::control::InterruptOutcome::Unconfirmed {
                    error: error.into(),
                    evidence: provider::control::SubmissionEvidence::Unobserved,
                };
            }
        };
        client
            .interrupt_outcome(thread_id, turn_id)
            .await
            .map_error(Into::into)
    }

    pub fn active_turn_id_for_thread(&self, thread_id: &str) -> Option<TurnId> {
        self.active_turns
            .get(thread_id)
            .map(|entry| entry.value().clone())
    }

    pub async fn interrupt_active_outcome(
        &self,
        thread_id: &str,
    ) -> provider::control::InterruptOutcome<CalmError> {
        let Some(turn_id) = self.active_turn_id_for_thread(thread_id) else {
            return provider::control::InterruptOutcome::NoTarget;
        };
        let outcome = self.interrupt_outcome(thread_id, &turn_id).await;
        match &outcome {
            provider::control::InterruptOutcome::LocalSignalDelivered => {
                unreachable!("Codex does not deliver local signals")
            }
            provider::control::InterruptOutcome::Unconfirmed { .. } => {}
            provider::control::InterruptOutcome::NativeAck
            | provider::control::InterruptOutcome::NoTarget => {
                self.active_turns
                    .remove_if(thread_id, |_, active| active == &turn_id);
            }
        }
        outcome
    }

    pub async fn interrupt_active_outcome_for_card(
        &self,
        card_id: &str,
    ) -> Result<InterruptOutcome<CalmError>> {
        let Some(thread_id) = resolve_active_thread_for_card(self.repo.as_ref(), card_id).await?
        else {
            return Ok(InterruptOutcome::NoTarget);
        };
        Ok(self.interrupt_active_outcome(&thread_id).await)
    }
}
