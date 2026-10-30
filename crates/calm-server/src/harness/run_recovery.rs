//! Owner-declared recovery actions are separate from historical edit capability.
use serde::Serialize;
use utoipa::ToSchema;
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunRecoveryCause {
    UnknownOutcome,
    Wedged,
    NativeCompletionPending,
}
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunHeldReason {
    PendingInputs,
    NativeCompletionUnavailable,
    CleanupRequired,
    /// A recorded provider launch has no confirmed native identity; no action is declared.
    IdentityUnconfirmed,
}
#[derive(Debug, Serialize, ToSchema)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum RunRecovery {
    RestartSession {
        cause: RunRecoveryCause,
    },
    AbandonSession {
        cause: RunRecoveryCause,
        request: calm_types::conversation::AbandonConversationRequest,
    },
    Held {
        cause: RunRecoveryCause,
        reason: RunHeldReason,
    },
}
