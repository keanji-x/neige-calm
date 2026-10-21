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
