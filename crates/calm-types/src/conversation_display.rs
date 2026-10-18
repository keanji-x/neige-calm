//! Public conversation display facts. Cursors name an index, never an execution grant.
use crate::model::HarnessItem as TranscriptRow;
use crate::planner_attachment::PlannerAttachment;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utoipa::ToSchema;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum ConversationReadContract {
    ConversationV1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum ConversationCursorSource {
    Presentation,
    HistoricalRaw,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(deny_unknown_fields)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct ConversationCursor {
    pub contract: ConversationReadContract,
    pub card_id: String,
    pub source: ConversationCursorSource,
    #[schema(minimum = 1)]
    pub position: i64,
}
impl ConversationCursor {
    pub fn new(card_id: &str, source: ConversationCursorSource, position: i64) -> Self {
        Self {
            contract: ConversationReadContract::ConversationV1,
            card_id: card_id.into(),
            source,
            position,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema, TS)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum ConversationDisplayIdentity {
    Human { message_id: String },
    Native { thread_id: String, item_id: String },
}

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct ConversationHumanAuthor {
    pub user_id: String,
    pub display_name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema, TS)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum ConversationDisplaySource {
    Human {
        message_id: String,
        revision: u32,
        author: ConversationHumanAuthor,
        text: String,
        attachments: Vec<PlannerAttachment>,
        created_at_ms: i64,
    },
    Native {
        item: TranscriptRow,
    },
    // Original V1 row. Native user text has no proven application author.
    HistoricalRaw {
        item: TranscriptRow,
    },
    // No body or invented author; exact identities retire acknowledged optimistic text.
    Removed {
        identity: ConversationDisplayIdentity,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct ConversationDisplayItem {
    pub cursor: ConversationCursor,
    pub source: ConversationDisplaySource,
}

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct ConversationPage {
    pub contract: ConversationReadContract,
    pub source: ConversationCursorSource,
    pub items: Vec<ConversationDisplayItem>,
    #[schema(required = true, nullable = true)]
    pub next_cursor: Option<ConversationCursor>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum ConversationInputContract {
    Canonical,
    Historical,
}

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct ConversationPendingMessage {
    pub message_id: String,
    pub revision: u32,
    pub queue_position: u64,
    pub queued_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum ConversationDelivery {
    CrossingUnknown,
    Unknown,
    NotDispatched,
    Accepted,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct ConversationSubmissionDelivery {
    pub submission_id: String,
    #[schema(required = true, nullable = true)]
    pub attempt_id: Option<String>,
    // Null means this submission has no recorded attempt, not proof of no dispatch.
    #[schema(required = true, nullable = true)]
    pub delivery: Option<ConversationDelivery>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct ConversationActiveInput {
    pub logical_turn_id: String,
    pub submissions: Vec<ConversationSubmissionDelivery>,
}
impl ConversationActiveInput {
    // A projection of current stored facts; never interprets retry/terminal permission.
    pub fn from_checkpoint(
        value: &crate::conversation::ConversationInputCheckpoint,
    ) -> Option<Self> {
        use crate::conversation::InputDeliveryFact;
        value.active.as_ref().map(|active| Self {
            logical_turn_id: active.logical_turn_id.as_str().into(),
            submissions: active
                .submission_ids
                .iter()
                .map(|submission| {
                    let attempt = value.attempt(submission);
                    ConversationSubmissionDelivery {
                        submission_id: submission.as_str().into(),
                        attempt_id: attempt.map(|a| a.attempt.attempt_id.as_str().into()),
                        delivery: attempt.map(|a| match &a.delivery {
                            InputDeliveryFact::CrossingUnknown => {
                                ConversationDelivery::CrossingUnknown
                            }
                            InputDeliveryFact::Unknown => ConversationDelivery::Unknown,
                            InputDeliveryFact::NotDispatched { .. } => {
                                ConversationDelivery::NotDispatched
                            }
                            InputDeliveryFact::Accepted { .. } => ConversationDelivery::Accepted,
                        }),
                    }
                })
                .collect(),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct ConversationLiveTurn {
    pub thread_id: String,
    pub turn_id: String,
    pub items: Vec<crate::harness::HarnessLiveReply>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct ConversationLiveReplies {
    pub contract: ConversationReadContract,
    #[schema(required = true, nullable = true)]
    pub turn: Option<ConversationLiveTurn>,
}
