//! Human controls bind the current carrier and exact queued message revision.
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utoipa::ToSchema;

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema, TS)]
#[serde(deny_unknown_fields)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct QueuedMessageRequest {
    pub carrier_id: String,
    pub if_revision: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema, TS)]
#[serde(deny_unknown_fields)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct EditQueuedMessageRequest {
    pub carrier_id: String,
    pub if_revision: u32,
    pub text: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema, TS)]
#[serde(deny_unknown_fields)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct SteerQueuedMessageRequest {
    pub carrier_id: String,
    pub if_revision: u32,
    pub turn_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct QueuedMessageSteerResult {
    pub submission_id: String,
    pub turn_id: String,
    pub delivery: crate::conversation_display::ConversationDelivery,
}
