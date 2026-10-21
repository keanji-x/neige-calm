//! Human decisions are distinct from native delivery evidence.
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utoipa::ToSchema;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(deny_unknown_fields)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct KeepConversationInputRequest {
    pub carrier_id: String,
    pub submission_id: String,
    pub attempt_id: String,
    /// Exact decimal revision from the run read, never a JavaScript number.
    pub revision: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(deny_unknown_fields)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct KeepConversationInputResult {
    pub carrier_id: String,
    pub submission_id: String,
    pub attempt_id: String,
    pub revision: String,
    pub waiting_for_settlement: bool,
}

/// Exact predecessor CAS for an explicit Human repeat; never a resend command.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(deny_unknown_fields)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct RetryConversationInputRequest {
    pub carrier_id: String,
    pub submission_id: String,
    pub attempt_id: String,
    pub revision: String,
}
impl From<&RetryConversationInputRequest> for KeepConversationInputRequest {
    fn from(value: &RetryConversationInputRequest) -> Self {
        Self {
            carrier_id: value.carrier_id.clone(),
            submission_id: value.submission_id.clone(),
            attempt_id: value.attempt_id.clone(),
            revision: value.revision.clone(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(deny_unknown_fields)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct RetryConversationInputResult {
    pub carrier_id: String,
    pub submission_id: String,
    pub attempt_id: String,
    pub revision: String,
    pub retry_intent_id: String,
    pub waiting_for_settlement: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum ConversationInputResolutionResult {
    Keep {
        result: KeepConversationInputResult,
    },
    Retry {
        result: RetryConversationInputResult,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HumanInputResolutionReceipt {
    pub key: String,
    pub principal: String,
    pub fingerprint: String,
    pub decision: HumanInputResolutionDecision,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum HumanInputResolutionDecision {
    Retry {
        predecessor: KeepConversationInputRequest,
        retry_intent_id: super::RetryIntentId,
        revision: String,
        waiting_for_settlement: bool,
    },
    Keep {
        request: KeepConversationInputRequest,
        result: KeepConversationInputResult,
    },
}

impl HumanInputResolutionDecision {
    /// Exhaustive public view of the sole persisted decision authority.
    pub fn public_result(&self) -> ConversationInputResolutionResult {
        match self {
            Self::Keep { result, .. } => ConversationInputResolutionResult::Keep {
                result: result.clone(),
            },
            Self::Retry {
                predecessor,
                retry_intent_id,
                revision,
                waiting_for_settlement,
            } => ConversationInputResolutionResult::Retry {
                result: RetryConversationInputResult {
                    carrier_id: predecessor.carrier_id.clone(),
                    submission_id: predecessor.submission_id.clone(),
                    attempt_id: predecessor.attempt_id.clone(),
                    revision: revision.clone(),
                    retry_intent_id: retry_intent_id.as_str().into(),
                    waiting_for_settlement: *waiting_for_settlement,
                },
            },
        }
    }
}
