//! Provider-owned recovery facts, available without opening a model session.
use serde::Serialize;
use utoipa::ToSchema;

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunRecoveryCause {
    UnknownOutcome,
    Wedged,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunRecoveryAction {
    RestartSession,
}

/// An explicit action; lifecycle phase and human-readable block notices are not recovery policy.
#[derive(Debug, Serialize, ToSchema)]
pub struct RunRecovery {
    pub cause: RunRecoveryCause,
    pub action: RunRecoveryAction,
}
