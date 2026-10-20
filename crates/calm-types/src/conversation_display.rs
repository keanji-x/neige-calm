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
    System { observation_id: String },
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
    System {
        observation_id: String,
        presentation: crate::model::SystemInputPresentation,
        text: String,
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
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ConversationPendingInput {
    Human {
        message_id: String,
        revision: u32,
        queue_position: u64,
        queued_at_ms: i64,
    },
    System {
        observation_id: String,
        queue_position: u64,
        queued_at_ms: i64,
    },
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
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum ConversationActionAvailability {
    Unavailable {
        reason: ConversationActionUnavailableReason,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum ConversationActionUnavailableReason {
    NotImplementedYet,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct ConversationResolutionActions {
    pub keep: ConversationActionAvailability,
    pub retry: ConversationActionAvailability,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct ConversationInputTarget {
    pub provider: crate::runtime::AgentProvider,
    pub thread_id: String,
    #[schema(required = true, nullable = true)]
    pub native_session_id: Option<String>,
    #[schema(required = true, nullable = true)]
    pub local_turn_id: Option<String>,
    #[schema(required = true, nullable = true)]
    pub native_turn_id: Option<String>,
    #[schema(required = true, nullable = true)]
    pub rpc_request_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct ConversationAttemptDelivery {
    pub attempt_id: String,
    /// Decimal text preserves the full persisted u64 CAS identity in browsers.
    pub revision: String,
    pub target: ConversationInputTarget,
    pub delivery: ConversationDelivery,
    pub invocation: crate::conversation::InputInvocation,
    pub actions: ConversationResolutionActions,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct ConversationSubmissionDelivery {
    pub carrier_id: String,
    pub logical_turn_id: String,
    pub submission_id: String,
    /// Null explicitly means no crossing has been recorded for this submission.
    #[schema(required = true, nullable = true)]
    pub attempt: Option<ConversationAttemptDelivery>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct ConversationActiveInput {
    /// Execution and held submissions are independent facts.
    #[schema(required = true, nullable = true)]
    pub current_execution_id: Option<String>,
    pub submissions: Vec<ConversationSubmissionDelivery>,
}
impl ConversationActiveInput {
    /// One factual projection. The Truth reader supplies original immutable
    /// logical-turn identities; neither current execution nor list order infers them.
    pub fn from_checkpoint(
        value: &crate::conversation::ConversationInputCheckpoint,
        carrier_id: &str,
        identities: &[(
            crate::conversation::InputSubmissionId,
            crate::conversation::LogicalTurnId,
        )],
    ) -> Result<Option<Self>, &'static str> {
        use crate::conversation::InputDeliveryFact;
        value.validate()?;
        if carrier_id.is_empty() {
            return Err("projection requires its exact carrier");
        }
        let submissions = value.referenced_submissions();
        if submissions.is_empty() {
            return Ok(None);
        }
        let submissions = submissions
            .iter()
            .map(|submission| {
                let logical = identities
                    .iter()
                    .find(|(id, _)| id == submission)
                    .ok_or("submission lacks its original logical turn identity")?;
                Ok(ConversationSubmissionDelivery {
                    carrier_id: carrier_id.into(),
                    logical_turn_id: logical.1.as_str().into(),
                    submission_id: submission.as_str().into(),
                    attempt: value
                        .attempt(submission)
                        .map(|attempt| ConversationAttemptDelivery {
                            attempt_id: attempt.attempt.attempt_id.as_str().into(),
                            revision: attempt.revision.to_string(),
                            target: ConversationInputTarget {
                                provider: attempt.correlation.provider.clone(),
                                thread_id: attempt.correlation.thread_id.clone(),
                                native_session_id: attempt.correlation.native_session_id.clone(),
                                local_turn_id: attempt.correlation.local_turn_id.clone(),
                                native_turn_id: attempt.correlation.native_turn_id.clone(),
                                rpc_request_id: attempt
                                    .correlation
                                    .rpc_request_id
                                    .map(|id| id.to_string()),
                            },
                            delivery: match &attempt.delivery {
                                InputDeliveryFact::CrossingUnknown => {
                                    ConversationDelivery::CrossingUnknown
                                }
                                InputDeliveryFact::Unknown => ConversationDelivery::Unknown,
                                InputDeliveryFact::NotDispatched { .. } => {
                                    ConversationDelivery::NotDispatched
                                }
                                InputDeliveryFact::Accepted { .. } => {
                                    ConversationDelivery::Accepted
                                }
                            },
                            invocation: attempt.invocation.clone(),
                            actions: ConversationResolutionActions {
                                keep: ConversationActionAvailability::Unavailable {
                                    reason: ConversationActionUnavailableReason::NotImplementedYet,
                                },
                                retry: ConversationActionAvailability::Unavailable {
                                    reason: ConversationActionUnavailableReason::NotImplementedYet,
                                },
                            },
                        }),
                })
            })
            .collect::<Result<Vec<_>, &'static str>>()?;
        Ok(Some(Self {
            current_execution_id: value
                .active
                .as_ref()
                .map(|active| active.logical_turn_id.as_str().into()),
            submissions,
        }))
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
