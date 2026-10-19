//! Historical host diagnostics. Delivery facts travel separately in provider operation outcomes.
use crate::error::CalmError;

/// C0 preserves these host decision categories independently of NotWritten/Unknown delivery.
/// Consumers must never reclassify the opaque payload by inspecting `CalmError`.
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

/// Historical start diagnostics, including host preparation and authentication recovery.
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
    ) -> provider::control::NativeInterruptOutcome<CalmError> {
        self.interrupt_control(thread_id, turn_id)
            .await
            .map_error(Into::into)
    }

    pub(super) async fn interrupt_control(
        &self,
        thread_id: &str,
        turn_id: &str,
    ) -> provider::control::NativeInterruptOutcome<control::StartError> {
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
                return provider::control::NativeInterruptOutcome::Unconfirmed {
                    error: control::StartError::Unanswered(CalmError::CodexAppServer(
                        "fixture: turn/interrupt failed".into(),
                    )),
                };
            }
            self.native_turns
                .acknowledge_control_for_fixture(thread_id, turn_id);
            return provider::control::NativeInterruptOutcome::NativeAck;
        }
        let client = match self.connected_client().await {
            Ok(client) => client,
            Err(error @ (ControlError::Refused(_) | ControlError::Unanswered(_))) => {
                return provider::control::NativeInterruptOutcome::Unconfirmed {
                    error: error.into(),
                };
            }
        };
        let outcome = client
            .interrupt_outcome(thread_id, turn_id)
            .await
            .map_error(Into::into);
        if !matches!(
            outcome,
            provider::control::NativeInterruptOutcome::Unconfirmed { .. }
        ) {
            self.native_turns.acknowledge_control_from_source(
                &Arc::downgrade(&client),
                thread_id,
                turn_id,
            );
        }
        outcome
    }

    /// Control one admitted canonical execution.
    pub(crate) async fn interrupt_execution<'a>(
        &self,
        target: &crate::harness::backend::ExecutionEndTarget,
        admission: &NativePromptAdmission<'a>,
    ) -> Result<()> {
        let client = self.connected_client().await?;
        let ticket = self.native_turns.begin_control(client, target)?;
        match ticket
            .client
            .interrupt_outcome_gated(&ticket.thread, &ticket.turn, admission.gate())
            .await
        {
            NativeInterruptOutcome::NativeAck | NativeInterruptOutcome::NoTarget => {}
            NativeInterruptOutcome::Unconfirmed { error } => {
                return Err(admission.take_refusal().unwrap_or_else(|| error.into()));
            }
        }
        self.native_turns.acknowledge_control_from_source(
            &Arc::downgrade(&ticket.client),
            &ticket.thread,
            &ticket.turn,
        );
        Ok(())
    }

    pub fn active_turn_id_for_thread(&self, thread_id: &str) -> Option<TurnId> {
        self.native_turns.active(thread_id)
    }

    pub async fn interrupt_active_outcome(
        &self,
        thread_id: &str,
    ) -> provider::control::NativeInterruptOutcome<CalmError> {
        match self.interrupt_active_snapshot(thread_id).await {
            Ok(true) => provider::control::NativeInterruptOutcome::NativeAck,
            Ok(false) => provider::control::NativeInterruptOutcome::NoTarget,
            Err(error) => provider::control::NativeInterruptOutcome::Unconfirmed { error },
        }
    }

    async fn interrupt_active_snapshot(&self, thread_id: &str) -> Result<bool> {
        // One bounded snapshot of the actual owner: at most known A and candidate B.
        // Never reread a replacement connection to send an old target's control.
        let snapshot = self.current_interrupt_snapshot(thread_id).await?;
        let mut failure = None;
        let mut acknowledged = false;
        for turn_id in &snapshot.turns {
            match self
                .interrupt_native_source_outcome(snapshot.source.as_ref(), thread_id, turn_id)
                .await
            {
                provider::control::NativeInterruptOutcome::NativeAck => acknowledged = true,
                provider::control::NativeInterruptOutcome::NoTarget => {}
                provider::control::NativeInterruptOutcome::Unconfirmed { error, .. } => {
                    if failure.is_none() {
                        failure = Some(error);
                    }
                }
            }
        }
        if let Some(error) = failure {
            return Err(error);
        }
        let remaining =
            self.interrupt_snapshot_from_original(snapshot.source.as_ref(), thread_id)?;
        if remaining.unresolved || !remaining.turns.is_empty() {
            return Err(CalmError::Conflict(
                "Codex native activity remains unresolved after interrupt".into(),
            ));
        }
        Ok(acknowledged)
    }

    async fn current_interrupt_snapshot(
        &self,
        thread_id: &str,
    ) -> Result<native_turns::InterruptSnapshot> {
        #[cfg(feature = "fixtures")]
        if self.fake.is_some() {
            return Ok(self.native_turns.interrupt_snapshot(thread_id));
        }
        // An empty in-process cache says nothing about a persisted thread on a
        // daemon awaiting hot takeover. Require the actual Running connection.
        let core = self.core.lock().await;
        let SupervisorState::Running { client, .. } = &core.state else {
            return Err(CalmError::Conflict(
                "Codex native source is unavailable for interrupt".into(),
            ));
        };
        self.native_turns.interrupt_snapshot_for(client, thread_id)
    }

    fn interrupt_snapshot_from_original(
        &self,
        source: Option<&Arc<CodexAppServer>>,
        thread_id: &str,
    ) -> Result<native_turns::InterruptSnapshot> {
        #[cfg(feature = "fixtures")]
        if self.fake.is_some() {
            return Ok(self.native_turns.interrupt_snapshot(thread_id));
        }
        let source = source
            .ok_or_else(|| CalmError::Conflict("Codex interrupt source is missing".into()))?;
        self.native_turns.interrupt_snapshot_for(source, thread_id)
    }

    pub(super) async fn interrupt_native_source_outcome(
        &self,
        source: Option<&Arc<CodexAppServer>>,
        thread_id: &str,
        turn_id: &str,
    ) -> provider::control::NativeInterruptOutcome<CalmError> {
        #[cfg(feature = "fixtures")]
        if self.fake.is_some() {
            return self.interrupt_outcome(thread_id, turn_id).await;
        }
        let Some(client) = source else {
            return provider::control::NativeInterruptOutcome::Unconfirmed {
                error: CalmError::Conflict("Codex native source is no longer connected".into()),
            };
        };
        let outcome = client
            .interrupt_outcome(thread_id, turn_id)
            .await
            .map_error(Into::into);
        if !matches!(
            outcome,
            provider::control::NativeInterruptOutcome::Unconfirmed { .. }
        ) {
            self.native_turns.acknowledge_control_from_source(
                &Arc::downgrade(client),
                thread_id,
                turn_id,
            );
        }
        outcome
    }

    pub async fn interrupt_active_outcome_for_card(
        &self,
        card_id: &str,
    ) -> Result<NativeInterruptOutcome<CalmError>> {
        let Some(thread_id) = resolve_active_thread_for_card(self.repo.as_ref(), card_id).await?
        else {
            return Ok(NativeInterruptOutcome::NoTarget);
        };
        Ok(self.interrupt_active_outcome(&thread_id).await)
    }
}

// RPC response facts are classified before entering the application diagnostic domain.
// Remote errors remain Unknown delivery; their old refusal diagnostic is retained for C0 policy.
impl From<provider::codex::request_attempt::RpcAttemptFailure> for ControlError {
    fn from(failure: provider::codex::request_attempt::RpcAttemptFailure) -> Self {
        use provider::codex::request_attempt::{FailureCause, RpcResponseError};
        let (method, cause) = failure.into_parts();
        match cause {
            FailureCause::Local(error) => error.into(),
            FailureCause::Response(RpcResponseError::Remote(error)) => {
                Self::Refused(CalmError::CodexRefused(format!(
                    "{} failed: {} (code {})",
                    method, error.message, error.code
                )))
            }
            FailureCause::Response(RpcResponseError::Malformed(raw)) => {
                Self::Refused(CalmError::CodexRefused(format!(
                    "{} failed: malformed error frame: {raw} (code 0)",
                    method
                )))
            }
        }
    }
}
impl From<provider::codex::request_attempt::RpcAttemptFailure> for StartError {
    fn from(failure: provider::codex::request_attempt::RpcAttemptFailure) -> Self {
        ControlError::from(failure).into()
    }
}

/// Historical result contract for unmanaged callers. Managed policy projects in harness/backend.
pub(super) fn start_result(
    delivery: provider::codex::delivery::StartDelivery<StartError>,
) -> std::result::Result<String, StartError> {
    use provider::codex::delivery::StartDelivery;
    match delivery {
        StartDelivery::NativeAck {
            turn,
            cleanup: None,
        }
        | StartDelivery::Unknown {
            response_turn: Some(turn),
            ..
        } => Ok(turn),
        StartDelivery::NativeAck {
            cleanup: Some(error),
            ..
        }
        | StartDelivery::NotWritten { error }
        | StartDelivery::Unknown {
            error,
            response_turn: None,
        } => Err(error),
    }
}
