//! Harness vocabulary shared across crates: the phase tag and persisted header constants.

/// Persisted harness header and failure vocabulary used by the codec and transactional recovery guards.
pub const HARNESS_SNAPSHOT_SCHEMA_VERSION: u32 = 1;
pub const HARNESS_MODE: &str = "harness";
pub const HARNESS_SYSTEM_ERROR_REASON: &str = "system_error";
pub const HARNESS_INTERRUPT_TIMEOUT_REASON: &str = "interrupt_timeout";
pub const HARNESS_INTERRUPT_TIMEOUT_MESSAGE: &str =
    "The stop request timed out before the model confirmed that this turn had stopped.";

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utoipa::ToSchema;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum HarnessPhaseTag {
    PendingThreadStart,
    Idle,
    IssuingTurn,
    IssuingInterrupt,
    TurnRunning,
    TurnCompleted,
    Resumed,
    Wedged,
}

/// Kernel-owned cause recorded only for a confirmed interrupted turn.
/// This metadata is separate from the provider's original error payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessInterruptionReason {
    MaxTurnDuration,
}

impl HarnessInterruptionReason {
    pub fn message(self) -> &'static str {
        match self {
            Self::MaxTurnDuration => {
                "This turn exceeded its execution time limit and was interrupted."
            }
        }
    }
}

/// A persisted kernel timeout request; the target and cause must both be known.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessInterruptionIntent {
    pub turn_id: String,
    pub reason: HarnessInterruptionReason,
}
