//! Preserve native mint non-delivery through retained activation preparation.
use super::*;
use provider::codex::{RequestCrossing, RpcAttemptFailure};

#[derive(Debug)]
pub(crate) enum ThreadMintFailure {
    BeforeSend(CalmError),
    Unknown(CalmError),
}

impl From<CalmError> for ThreadMintFailure {
    fn from(error: CalmError) -> Self {
        Self::BeforeSend(error)
    }
}

impl From<ControlError> for ThreadMintFailure {
    fn from(error: ControlError) -> Self {
        Self::BeforeSend(error.into())
    }
}

impl From<ThreadMintFailure> for CalmError {
    fn from(failure: ThreadMintFailure) -> Self {
        match failure {
            ThreadMintFailure::BeforeSend(error) | ThreadMintFailure::Unknown(error) => error,
        }
    }
}

impl NativePromptAdmission<'_> {
    pub(super) fn finish_mint<T>(
        &self,
        result: std::result::Result<T, RpcAttemptFailure>,
    ) -> std::result::Result<T, ThreadMintFailure> {
        result.map_err(|failure| {
            let before_send = failure.crossing() == RequestCrossing::BeforeSend;
            let error = self
                .take_refusal()
                .unwrap_or_else(|| failure.into_legacy_error().into());
            if before_send {
                ThreadMintFailure::BeforeSend(error)
            } else {
                ThreadMintFailure::Unknown(error)
            }
        })
    }
}
