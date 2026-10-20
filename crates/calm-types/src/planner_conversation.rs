//! A Planner card's transcript split into conversations (#2774). Conversation 0 is implicit; each
//! later one starts at a persisted floor, and the highest ordinal is the current conversation.
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utoipa::ToSchema;

use crate::ids::CardId;

/// The answer of `POST /api/cards/{id}/planner/conversations`, the same body on every replay of its key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct PlannerConversationStarted {
    pub card_id: CardId,
    /// The ordinal of the conversation this request started, now the current one.
    pub conversation: i64,
    /// The provider thread of the new conversation's session.
    pub new_thread_id: String,
}

/// One earlier, read-only conversation that still has transcript rows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct PlannerConversation {
    pub ordinal: i64,
    /// When its floor was written; for conversation 0, its first transcript row.
    pub started_at_ms: i64,
    /// When the next conversation started.
    pub ended_at_ms: i64,
}
