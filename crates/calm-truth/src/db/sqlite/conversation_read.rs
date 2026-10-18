//! Finite display reads. No adoption, native calls, authorization grant or extra body storage.
use crate::error::{CalmError, Result};
use calm_types::conversation::{ConversationMessageId, MessageRevisionRef, PrivateHistoryPayload};
use calm_types::conversation_display::*;
use sqlx::{FromRow, Row, SqlitePool};

pub(super) async fn current_frame(
    pool: &SqlitePool,
    card: &str,
) -> Result<crate::db::ConversationReadFrame> {
    let row = sqlx::query(
        "SELECT c.session_id, w.card_id AS runtime_card, w.state, w.handle_state_json,
                EXISTS(SELECT 1 FROM conversation_presentations WHERE card_id=c.id) AS has_presentations
         FROM cards c LEFT JOIN worker_sessions w ON w.id=c.session_id WHERE c.id=?1",
    ).bind(card).fetch_optional(pool).await?
        .ok_or_else(|| CalmError::NotFound(format!("card {card}")))?;
    let runtime_id: Option<String> = row.try_get("session_id")?;
    if runtime_id.is_some()
        && row.try_get::<Option<String>, _>("runtime_card")?.as_deref() != Some(card)
    {
        return Err(CalmError::Conflict(
            "current conversation carrier is missing or mismatched",
        ));
    }
    let raw: Option<String> = row.try_get("handle_state_json")?;
    let runtime = runtime_id
        .map(|id| -> Result<_> {
            Ok(crate::db::CurrentConversationRuntime {
                id,
                state: calm_types::worker::WorkerSessionState::try_from(
                    row.try_get::<String, _>("state")?,
                )
                .map_err(CalmError::Internal)?,
            })
        })
        .transpose()?;
    Ok(crate::db::ConversationReadFrame {
        runtime,
        snapshot: raw.as_deref().map(serde_json::from_str).transpose()?,
        has_presentations: row.try_get("has_presentations")?,
    })
}

pub(super) async fn page(
    pool: &SqlitePool,
    card: &str,
    after: i64,
    limit: i64,
    descending: bool,
) -> Result<ConversationPage> {
    let (comparison, order, position) = if descending {
        ("<", "DESC", if after == 0 { i64::MAX } else { after })
    } else {
        (">", "ASC", after)
    };
    // One autocommit statement sees each message's current revision/tombstone and backing row together.
    let sql = format!(
        "SELECT p.sequence,p.source_kind,p.source_identity,p.erased_by_operation_id,
                p.native_item_row_id AS backing_native_id,
                m.id AS message_id,m.current_revision,m.author_kind,m.author_user_id,m.withdrawn_at_ms,
                u.display_name,r.body_sha256,r.payload_json,r.created_at_ms AS revision_created,
                system_pending.value AS system_pending, system_claim.value AS system_claim,
                system_input.queued_at_ms AS system_claim_at, i.*
         FROM conversation_presentations p
         LEFT JOIN cards current_card ON current_card.id=p.card_id
         LEFT JOIN worker_sessions current_worker ON current_worker.id=current_card.session_id
           AND current_worker.card_id=current_card.id AND current_worker.track_id=current_card.track_id
         LEFT JOIN json_each(CASE WHEN json_valid(current_worker.handle_state_json)
           THEN current_worker.handle_state_json ELSE '{{}}' END,'$.canonical_inputs.pending') system_pending
           ON p.source_kind='system_observation' AND json_extract(system_pending.value,'$.kind')='system'
           AND json_extract(system_pending.value,'$.observation_id')=p.observation_id
         LEFT JOIN conversation_submission_inputs system_input
           ON p.source_kind='system_observation' AND system_input.observation_id=p.observation_id AND system_input.effective=1
         LEFT JOIN conversation_input_submissions system_submission
           ON system_submission.id=system_input.submission_id AND system_submission.card_id=p.card_id
         LEFT JOIN json_each(system_submission.render_plan_json,'$.segments') system_claim
           ON json_extract(system_claim.value,'$.kind')='input'
           AND json_extract(system_claim.value,'$.input.kind')='system'
           AND json_extract(system_claim.value,'$.input.observation_id')=p.observation_id
           AND json_extract(system_claim.value,'$.input_position')=system_input.input_position
         LEFT JOIN conversation_messages m ON m.id=p.message_id AND m.card_id=p.card_id
         LEFT JOIN application_users u ON u.id=m.author_user_id
         LEFT JOIN conversation_message_revisions r ON r.message_id=m.id AND r.revision=m.current_revision
         LEFT JOIN harness_items i ON i.id=p.native_item_row_id AND i.card_id=p.card_id
         WHERE p.card_id=?1 AND p.sequence {comparison} ?2 ORDER BY p.sequence {order} LIMIT ?3"
    );
    let rows = sqlx::query(&sql)
        .bind(card)
        .bind(position)
        .bind(limit)
        .fetch_all(pool)
        .await?;
    let next_cursor = (rows.len() == limit as usize && limit > 0).then(|| {
        ConversationCursor::new(
            card,
            ConversationCursorSource::Presentation,
            rows.last().expect("nonempty page").get("sequence"),
        )
    });
    let mut items = Vec::with_capacity(rows.len());
    for row in rows {
        let cursor = ConversationCursor::new(
            card,
            ConversationCursorSource::Presentation,
            row.try_get("sequence")?,
        );
        let removed = row
            .try_get::<Option<String>, _>("erased_by_operation_id")?
            .is_some();
        let source = match row.try_get::<&str, _>("source_kind")? {
            "human_message" => {
                let message_id: String = row.try_get("message_id")?;
                let identity = ConversationDisplayIdentity::Human {
                    message_id: message_id.clone(),
                };
                if row.try_get::<&str, _>("author_kind")? != "authenticated_human" {
                    return Err(CalmError::Conflict(
                        "canonical Legacy display requires its declared adoption",
                    ));
                }
                if removed || row.try_get::<Option<i64>, _>("withdrawn_at_ms")?.is_some() {
                    ConversationDisplaySource::Removed { identity }
                } else {
                    let revision = u32::try_from(row.try_get::<i64, _>("current_revision")?)
                        .ok()
                        .and_then(std::num::NonZeroU32::new)
                        .ok_or_else(|| CalmError::Internal("invalid message revision".into()))?;
                    let reference = MessageRevisionRef {
                        message_id: ConversationMessageId::parse(&message_id)
                            .map_err(|e| CalmError::Internal(e.to_string()))?,
                        revision,
                    };
                    let stored = super::conversation::messages::decode_revision(
                        reference,
                        row.try_get("body_sha256")?,
                        row.try_get("payload_json")?,
                        row.try_get("revision_created")?,
                    )?;
                    match stored.payload {
                        PrivateHistoryPayload::Present { value } => {
                            ConversationDisplaySource::Human {
                                message_id,
                                revision: revision.get(),
                                author: ConversationHumanAuthor {
                                    user_id: row.try_get("author_user_id")?,
                                    display_name: row.try_get("display_name")?,
                                },
                                text: value.text,
                                attachments: value
                                    .bound_attachments
                                    .iter()
                                    .map(|a| a.wire(&card.into()))
                                    .collect(),
                                created_at_ms: stored.created_at_ms,
                            }
                        }
                        PrivateHistoryPayload::ErasedByPrivateHistoryOperation { .. } => {
                            ConversationDisplaySource::Removed { identity }
                        }
                    }
                }
            }
            "system_observation" => {
                let observation_id: String = row.try_get("source_identity")?;
                let identity = ConversationDisplayIdentity::System {
                    observation_id: observation_id.clone(),
                };
                if removed {
                    ConversationDisplaySource::Removed { identity }
                } else {
                    use calm_types::conversation::{
                        PendingInput, PrivateHistoryPayload, SubmissionRenderInput,
                        SubmissionRenderSegment,
                    };
                    let pending: Option<String> = row.try_get("system_pending")?;
                    let claimed: Option<String> = row.try_get("system_claim")?;
                    let (payload, at) = if let Some(pending) = pending {
                        let PendingInput::System {
                            observation_id: stored_id,
                            observation,
                            queued_at_ms,
                            ..
                        } = serde_json::from_str(&pending)?
                        else {
                            return Err(CalmError::Conflict(
                                "System presentation backing kind changed",
                            ));
                        };
                        if stored_id.as_str() != observation_id {
                            return Err(CalmError::Conflict(
                                "System presentation identity changed",
                            ));
                        }
                        (
                            PrivateHistoryPayload::Present { value: observation },
                            queued_at_ms,
                        )
                    } else if let Some(claimed) = claimed {
                        let SubmissionRenderSegment::Input {
                            input:
                                SubmissionRenderInput::System {
                                    observation_id: stored_id,
                                    payload,
                                },
                            ..
                        } = serde_json::from_str(&claimed)?
                        else {
                            return Err(CalmError::Conflict(
                                "System presentation claim kind changed",
                            ));
                        };
                        if stored_id.as_str() != observation_id {
                            return Err(CalmError::Conflict(
                                "System presentation claim identity changed",
                            ));
                        }
                        (payload, row.try_get("system_claim_at")?)
                    } else {
                        return Err(CalmError::Conflict(
                            "System presentation has no accepted or claimed input",
                        ));
                    };
                    match payload {
                        PrivateHistoryPayload::Present { value } => {
                            ConversationDisplaySource::System {
                                observation_id,
                                presentation: value
                                    .observation()
                                    .input_segment(None)
                                    .presentation
                                    .try_into()
                                    .map_err(|error| {
                                        CalmError::Internal(format!("System presentation: {error}"))
                                    })?,
                                text: value.observation().to_turn_text(),
                                created_at_ms: at,
                            }
                        }
                        PrivateHistoryPayload::ErasedByPrivateHistoryOperation { .. } => {
                            ConversationDisplaySource::Removed { identity }
                        }
                    }
                }
            }
            "native_item" => {
                let (thread_id, item_id): (String, String) =
                    serde_json::from_str(row.try_get("source_identity")?)?;
                if !removed
                    && row
                        .try_get::<Option<i64>, _>("backing_native_id")?
                        .is_some()
                    && row.try_get::<Option<i64>, _>("id")?.is_none()
                {
                    return Err(CalmError::Internal(
                        "native presentation backing row scope mismatch".into(),
                    ));
                }
                if removed || row.try_get::<Option<i64>, _>("id")?.is_none() {
                    ConversationDisplaySource::Removed {
                        identity: ConversationDisplayIdentity::Native { thread_id, item_id },
                    }
                } else {
                    let item = crate::model::HarnessItem::try_from(
                        crate::db::rows::HarnessItemRow::from_row(&row)?,
                    )
                    .map_err(|e| CalmError::Internal(format!("native transcript decode: {e}")))?;
                    super::conversation::presentation::native_source_identity(
                        &item.thread_id,
                        item.item_uuid.as_deref(),
                        item.item_type.as_deref(),
                        &item.method,
                        &serde_json::from_str(&item.params)?,
                    )?;
                    if item.thread_id != thread_id
                        || item.item_uuid.as_deref() != Some(item_id.as_str())
                    {
                        return Err(CalmError::Internal(
                            "native presentation source mismatch".into(),
                        ));
                    }
                    ConversationDisplaySource::Native { item }
                }
            }
            _ => {
                return Err(CalmError::Conflict(
                    "this canonical display source requires its owning reader",
                ));
            }
        };
        items.push(ConversationDisplayItem { cursor, source });
    }
    if descending {
        items.reverse();
    }
    Ok(ConversationPage {
        contract: ConversationReadContract::ConversationV1,
        source: ConversationCursorSource::Presentation,
        items,
        next_cursor,
    })
}
