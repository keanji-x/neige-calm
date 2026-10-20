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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HumanKeepReceipt {
    pub key: String,
    pub principal: String,
    pub fingerprint: String,
    pub request: KeepConversationInputRequest,
    pub result: KeepConversationInputResult,
}
