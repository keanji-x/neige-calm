//! Exactly one issuance path, with explicit storage ownership on each invocation.
use super::*;
use crate::db::sqlite::{AcpHistoricalInput, AcpSubmissionInput};
use crate::harness::{backend::ExecutionInput, native_admission::NativePromptAdmission};

pub(super) enum InputContract<'a, 'g> {
    Historical(Value),
    Canonical {
        execution: ExecutionInput,
        admission: &'a NativePromptAdmission<'g>,
    },
}
impl InputContract<'_, '_> {
    pub(super) fn registration_failure(
        &self,
        error: crate::db::sqlite::AcpRegistrationError,
    ) -> super::super::start::StartFailure {
        match self {
            Self::Canonical { .. } => super::super::start::registration_error(error),
            Self::Historical(_) => {
                CalmError::from(calm_truth::error::TruthError::from(error)).into()
            }
        }
    }
    pub(super) fn unresolved_failure(&self) -> super::super::start::StartFailure {
        match self {
            Self::Canonical { .. } => refused(
                "Previous ACP outcome is unknown. Execution remains held; nothing will be resent.",
            ),
            Self::Historical(_) => refused(
                "Previous ACP outcome is unknown. Start a fresh session; nothing will be resent.",
            ),
        }
    }
    pub(super) fn association(&self) -> super::execution_end::Association {
        match self {
            Self::Historical(_) => super::execution_end::Association::Historical,
            Self::Canonical { execution, .. } => {
                super::execution_end::Association::Canonical(execution.acp_association())
            }
        }
    }
    pub(super) fn receipt(
        &self,
        items: &[InputItem],
        blocks: &[protocol::ContentBlock],
    ) -> Result<String> {
        match self {
            Self::Historical(queue) => AcpSubmissionInput::Historical(AcpHistoricalInput {
                input: super::super::start::receipt_input(items),
                prompt: serde_json::to_value(blocks)?,
                queue: queue.clone(),
            }),
            Self::Canonical { execution, .. } => {
                AcpSubmissionInput::Canonical(execution.acp_association())
            }
        }
        .encode()
        .map_err(Into::into)
    }
    pub(super) fn matches(&self, receipt: &AcpSubmissionInput) -> bool {
        match (self, receipt) {
            (Self::Historical(queue), AcpSubmissionInput::Historical(stored)) => {
                queue == &stored.queue
            }
            (Self::Canonical { execution, .. }, AcpSubmissionInput::Canonical(stored)) => {
                execution.acp_association() == *stored
            }
            (Self::Historical(_), AcpSubmissionInput::Canonical(_))
            | (Self::Canonical { .. }, AcpSubmissionInput::Historical(_)) => false,
        }
    }
    pub(super) async fn check_source(&self) -> Result<()> {
        match self {
            Self::Historical(_) => Ok(()),
            Self::Canonical { admission, .. } => admission.check_local().await,
        }
    }
    pub(super) fn admission<'b, 'g>(
        &'b self,
        owner: &'b NativePromptAdmission<'g>,
    ) -> NativePromptAdmission<'b>
    where
        'g: 'b,
    {
        match self {
            Self::Canonical { admission, .. } => admission.combined(owner),
            Self::Historical(_) => {
                NativePromptAdmission::new(|| Ok(()), move || Box::pin(owner.check_local()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn acp_historical_registration_failure_keeps_its_existing_host_contract() {
        let input = InputContract::Historical(json!([]));
        let failure = input.registration_failure(crate::db::sqlite::AcpRegistrationError::Changed);
        let super::super::super::start::StartFailure::Host(TurnStartFailure::Transient(
            CalmError::Conflict(reason),
        )) = failure
        else {
            panic!("historical contract changed");
        };
        assert_eq!(
            reason,
            "ACP registration changed; reset the conversation explicitly"
        );
        let failure = super::super::super::start::registration_error(
            crate::db::sqlite::AcpRegistrationError::Changed,
        );
        assert!(matches!(
            failure,
            super::super::super::start::StartFailure::Host(TurnStartFailure::Refused { .. })
        ));
    }
}
