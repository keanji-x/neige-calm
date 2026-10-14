//! Maintenance facts in the runtime snapshot, never transcript turns or rewind anchors.
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CompactStatus {
    Started,
    Completed,
    Interrupted,
    Failed,
    Unconfirmed,
    Refused,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct CompactOutcome {
    /// Neige's identifier for this submission, minted before calling the provider.
    pub operation_id: String,
    pub thread_id: String,
    /// Unknown until the provider starts its maintenance turn.
    #[schema(required = true)]
    pub turn_id: Option<String>,
    pub status: CompactStatus,
}
