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
    Compacting,
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

impl HarnessInterruptionIntent {
    pub fn from_request(turn_id: &str, reason: &str) -> Option<Self> {
        if reason != "max_turn_duration" {
            return None;
        }
        Some(Self {
            turn_id: turn_id.into(),
            reason: HarnessInterruptionReason::MaxTurnDuration,
        })
    }
}

/// `GET /api/cards/{id}/harness/live`: reply text the running turn is streaming and has not stored
/// yet (#1923). Each item leaves this answer once its `item/completed` row is in the transcript.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct HarnessLiveReplies {
    /// The turn the items belong to: the turn that started last, until it ends with nothing left to
    /// store; `null` before any turn of this harness and after one that ended so. A turn that ends
    /// without settling (`turn/aborted`, an interrupt that times out and wedges) or whose partial
    /// reply failed to store keeps its id here, with the replies it still holds, until the next turn
    /// starts.
    #[schema(required = true, nullable = true)]
    pub turn_id: Option<String>,
    pub items: Vec<HarnessLiveReply>,
}

/// One reply as far as it has streamed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct HarnessLiveReply {
    /// The `item.id` its `item/completed` row will carry as `item_uuid`.
    pub item_id: String,
    pub text: String,
}

/// Whether a Planner card's provider may ask the person before it acts outside its sandbox
/// (#2348). Stored on the card as the server-owned payload key `permission_mode`; every Planner
/// card is created `never`, and only `PUT /api/cards/{id}/planner/permission-mode` changes it.
// Only the bare strings `"never"` and `"ask"` deserialize: serde's derived enum also accepts the
// object form `{"ask": null}`, which would read a corrupt stored value as permission to ask.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(rename_all = "snake_case", try_from = "String")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum PlannerPermissionMode {
    /// The provider never asks: an action outside the sandbox fails.
    Never,
    /// The provider asks the person and waits for the answer.
    Ask,
}

impl TryFrom<String> for PlannerPermissionMode {
    type Error = String;

    fn try_from(name: String) -> Result<Self, Self::Error> {
        match name.as_str() {
            "never" => Ok(Self::Never),
            "ask" => Ok(Self::Ask),
            _ => Err(format!("unknown permission mode `{name}`")),
        }
    }
}

/// `PUT /api/cards/{id}/planner/permission-mode`'s answer: the mode now stored on the card.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct SetPlannerPermissionModeResponse {
    #[schema(value_type = String)]
    pub card_id: crate::ids::CardId,
    /// The stored mode, echoed rather than assumed.
    pub permission_mode: PlannerPermissionMode,
}
