//! Native setup facts. Conversation admission maps these into its own diagnostic vocabulary.
use crate::error::CalmError;
#[derive(Debug)]
pub(crate) enum SetupFailure {
    Failed(CalmError),
    LoadUnsupported,
}
impl From<CalmError> for SetupFailure {
    fn from(error: CalmError) -> Self {
        Self::Failed(error)
    }
}
impl From<SetupFailure> for CalmError {
    fn from(error: SetupFailure) -> Self {
        match error {
            SetupFailure::Failed(error) => error,
            SetupFailure::LoadUnsupported => {
                CalmError::Conflict("ACP agent cannot load or resume the native session".into())
            }
        }
    }
}
