// Harness vocabulary shared across crates: phase tags and persisted header constants.

pub mod budget;
pub mod snapshot;

/// Persisted harness header and failure vocabulary used by the codec and transactional recovery guards.
pub const HARNESS_SNAPSHOT_SCHEMA_VERSION: u32 = 1;
pub const HARNESS_MODE: &str = "harness";
pub const HARNESS_SYSTEM_ERROR_REASON: &str = "system_error";
pub const HARNESS_RUNTIME_FAILED_REASON: &str = "runtime_failed";
pub const HARNESS_INTERRUPT_TIMEOUT_REASON: &str = "interrupt_timeout";
pub const HARNESS_INTERRUPT_TIMEOUT_MESSAGE: &str =
    "The stop request timed out before the model confirmed that this turn had stopped.";

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utoipa::ToSchema;

/// Persisted start-operation profile vocabulary. Admission and shape checks stay
/// in the server; moving this IO-free enum does not change its serialized names.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessProfile {
    #[default]
    #[serde(rename = "spec")]
    Planner,
    PlainChat,
    Assistant,
}

/// Provider-native input vocabulary; both spellings occur in persisted rows.
/// Attribution is decided from the accepted source, never inferred from text.
pub fn is_native_user_message_type(item_type: Option<&str>) -> bool {
    matches!(item_type, Some("userMessage" | "user_message"))
}

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

/// Stable protocol identity shared by next-input delivery and Track dismissals.
pub const PLANNER_TURN_TIMEOUT_KEY_PREFIX: &str = "planner_turn_timeout:";

pub fn planner_timeout_key(row_id: i64) -> String {
    format!("{PLANNER_TURN_TIMEOUT_KEY_PREFIX}{row_id}")
}

/// Kernel-owned cause recorded only for a confirmed interrupted turn.
/// This metadata is separate from the provider's original error payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
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

/// Explicit compatibility: legacy intents establish only a cause, never time or budget.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum HarnessInterruptionIntent {
    Versioned(HarnessTimeoutIntent),
    Legacy(HarnessLegacyInterruptionIntent),
    Control(HarnessControlIntent),
}

/// A person's Stop has no execution-budget cause. Its request clock and exact
/// owner survive an RPC timeout; recording it never claims native completion.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "version", rename_all = "snake_case")]
pub enum HarnessControlIntent {
    UserStopV1(HarnessStopRequest),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessStopRequest {
    pub session_id: String,
    pub card_id: String,
    pub track_id: String,
    pub thread_id: String,
    pub turn_id: String,
    pub requested_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessLegacyInterruptionIntent {
    pub turn_id: String,
    pub reason: HarnessInterruptionReason,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "version", rename_all = "snake_case")]
pub enum HarnessTimeoutIntent {
    V1(HarnessTimeoutRequest),
}

/// All facts are sampled by the watchdog before issuing the interrupt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(deny_unknown_fields)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct HarnessTimeoutRequest {
    pub session_id: String,
    pub card_id: String,
    pub track_id: String,
    pub thread_id: String,
    pub turn_id: String,
    pub reason: HarnessInterruptionReason,
    pub requested_at_ms: i64,
    pub effective_elapsed_ms: u64,
    pub budget_ms: u64,
}

/// Written exclusively at the kernel terminal-outcome boundary after confirmation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(tag = "version", rename_all = "snake_case")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum HarnessTimeoutEvidence {
    V1 {
        request: HarnessTimeoutRequest,
        ended_at_ms: i64,
    },
}

impl HarnessInterruptionIntent {
    pub fn turn_id(&self) -> &str {
        match self {
            Self::Legacy(intent) => &intent.turn_id,
            Self::Versioned(HarnessTimeoutIntent::V1(request)) => &request.turn_id,
            Self::Control(HarnessControlIntent::UserStopV1(request)) => &request.turn_id,
        }
    }
    pub fn timeout_reason(&self) -> Option<HarnessInterruptionReason> {
        match self {
            Self::Legacy(intent) => Some(intent.reason),
            Self::Versioned(HarnessTimeoutIntent::V1(request)) => Some(request.reason),
            Self::Control(_) => None,
        }
    }
    pub fn requested_at_ms(&self) -> Option<i64> {
        match self {
            Self::Versioned(HarnessTimeoutIntent::V1(request)) => Some(request.requested_at_ms),
            Self::Control(HarnessControlIntent::UserStopV1(request)) => {
                Some(request.requested_at_ms)
            }
            Self::Legacy(_) => None,
        }
    }
    /// Old snapshot construction has no trusted clock; retain only the legacy facts.
    pub fn from_request(turn_id: &str, reason: &str) -> Option<Self> {
        (reason == "max_turn_duration").then(|| {
            Self::Legacy(HarnessLegacyInterruptionIntent {
                turn_id: turn_id.into(),
                reason: HarnessInterruptionReason::MaxTurnDuration,
            })
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

/// What a Planner card's provider may do outside its sandbox (#2348, #2441): nothing, what the
/// person allows when asked, or anything, with no sandbox at all. Stored on the card as the server-owned payload key `permission_mode`; new Planner
/// cards use their ACP registration’s declared default, or `never` without one; delegated children inherit their parent’s mode. Only `PUT /api/cards/{id}/planner/permission-mode` changes it.
// Only the bare strings `"never"`, `"ask"` and `"full"` deserialize: serde's derived enum also
// accepts the object form `{"ask": null}`, which would read a corrupt stored value as permission.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(rename_all = "snake_case", try_from = "String")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum PlannerPermissionMode {
    /// The provider never asks: an action outside the sandbox fails.
    Never,
    /// The provider asks the person and waits for the answer.
    Ask,
    /// The provider runs without its sandbox and never asks (#2441): the Planner acts with the
    /// account's own access.
    Full,
}

impl TryFrom<String> for PlannerPermissionMode {
    type Error = String;

    fn try_from(name: String) -> Result<Self, Self::Error> {
        match name.as_str() {
            "never" => Ok(Self::Never),
            "ask" => Ok(Self::Ask),
            "full" => Ok(Self::Full),
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

#[cfg(test)]
mod timeout_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn versioned_timeout_requires_all_facts_and_legacy_does_not_invent_them() {
        let old = json!({"turn_id":"t", "reason":"max_turn_duration"});
        let legacy: HarnessInterruptionIntent = serde_json::from_value(old.clone()).unwrap();
        assert!(matches!(legacy, HarnessInterruptionIntent::Legacy(_)));
        assert_eq!(serde_json::to_value(&legacy).unwrap(), old);
        let current = json!({"version":"v1","session_id":"s","card_id":"c","track_id":"track","thread_id":"th","turn_id":"t","reason":"max_turn_duration","requested_at_ms":100,"effective_elapsed_ms":61000,"budget_ms":60000});
        assert!(matches!(
            serde_json::from_value::<HarnessInterruptionIntent>(current.clone()).unwrap(),
            HarnessInterruptionIntent::Versioned(_)
        ));
        for key in current.as_object().unwrap().keys() {
            let mut missing = current.clone();
            missing.as_object_mut().unwrap().remove(key);
            assert!(
                serde_json::from_value::<HarnessInterruptionIntent>(missing).is_err(),
                "missing {key}"
            );
        }
        let mut future = current;
        future["version"] = json!("v2");
        assert!(serde_json::from_value::<HarnessInterruptionIntent>(future).is_err());
    }
}

#[cfg(test)]
mod profile_tests {
    use super::HarnessProfile;
    use serde_json::json;

    /// The default Planner name is pinned by the start-operation payload test in calm-server.
    #[test]
    fn harness_profile_keeps_persisted_names() {
        for (profile, wire) in [
            (HarnessProfile::PlainChat, json!("plain_chat")),
            (HarnessProfile::Assistant, json!("assistant")),
        ] {
            assert_eq!(serde_json::to_value(profile).unwrap(), wire);
            assert_eq!(
                serde_json::from_value::<HarnessProfile>(wire).unwrap(),
                profile
            );
        }
        let planner = serde_json::to_value(HarnessProfile::Planner).unwrap();
        assert_eq!(
            serde_json::from_value::<HarnessProfile>(planner).unwrap(),
            HarnessProfile::Planner
        );
        assert_eq!(HarnessProfile::default(), HarnessProfile::Planner);
    }
}
