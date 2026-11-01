//! Finite display reads. No adoption, native calls, authorization grant or extra body storage.
use crate::error::{CalmError, Result};
use calm_types::conversation_display::*;
use sqlx::{FromRow, Row, SqlitePool};

mod human;
pub(super) use human::first_title_on_connection;

pub(super) async fn current_frame(
    db: &super::SqliteDb,
    card: &str,
) -> Result<crate::db::ConversationReadFrame> {
    // The snapshot and immutable identity/plan joins must share one observation.
    // Use the existing writer admission owner so a parked read cannot hold a
    // shared-cache R lock across a waiting writer (#930).
    let mut tx = super::begin_immediate_tx(db).await?;
    let row = sqlx::query(
        "SELECT c.session_id, c.track_id, w.track_id AS runtime_track, w.card_id AS runtime_card, w.state, w.handle_state_json,
                EXISTS(SELECT 1 FROM conversation_presentations WHERE card_id=c.id) AS has_presentations
         FROM cards c LEFT JOIN worker_sessions w ON w.id=c.session_id WHERE c.id=?1",
    ).bind(card).fetch_optional(&mut **tx).await?
        .ok_or_else(|| CalmError::NotFound(format!("card {card}")))?;
    let worker_session_id: Option<String> = row.try_get("session_id")?;
    if worker_session_id.is_some()
        && (row.try_get::<Option<String>, _>("runtime_card")?.as_deref() != Some(card)
            || row.try_get::<Option<String>, _>("runtime_track")? != Some(row.try_get("track_id")?))
    {
        return Err(CalmError::Conflict(
            "current conversation carrier is missing or mismatched",
        ));
    }
    let raw: Option<String> = row.try_get("handle_state_json")?;
    let runtime = worker_session_id
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
    let snapshot: Option<serde_json::Value> =
        raw.as_deref().map(serde_json::from_str).transpose()?;
    let canonical = snapshot
        .as_ref()
        .filter(|value| value.get("canonical_inputs").is_some())
        .map(|value| {
            serde_json::from_value::<calm_types::harness::snapshot::CanonicalHarnessSnapshot>(
                value.clone(),
            )
        })
        .transpose()?;
    let active_input = match canonical.as_ref() {
        Some(frame) => {
            let carrier = runtime
                .as_ref()
                .ok_or(CalmError::Conflict("canonical frame lacks its carrier"))?;
            let ids = frame.canonical_inputs.referenced_submissions();
            let mut identities = Vec::with_capacity(ids.len());
            for id in ids {
                let identity = super::conversation::submission_identity_tx(&mut tx, &id).await?;
                if identity.card_id != card {
                    return Err(CalmError::Conflict(
                        "submission belongs to another conversation",
                    ));
                }
                // Decode and validate the original immutable plan through its sole owner.
                super::conversation::submission_plan_tx(&mut tx, &id).await?;
                identities.push((id, identity.logical_turn_id, identity.ordinal));
            }
            let needs_retry_policy = identities.iter().any(|(id, _, _)| {
                frame.canonical_inputs.attempt(id).is_some_and(|attempt| {
                    !frame.canonical_inputs.is_human_resolved(&attempt.attempt)
                        && matches!(
                            attempt.delivery,
                            calm_types::conversation::InputDeliveryFact::Unknown
                                | calm_types::conversation::InputDeliveryFact::CrossingUnknown
                        )
                })
            });
            let current = if needs_retry_policy
                && (carrier.state.is_active_authority()
                    || carrier.state == calm_types::worker::WorkerSessionState::Failed)
            {
                Some(
                    super::conversation::current_input_fact_carrier_tx(&mut tx, card, &carrier.id)
                        .await?,
                )
            } else {
                None
            };
            let mut availability = Vec::with_capacity(identities.len());
            for (id, _, _) in &identities {
                let reason = match frame.canonical_inputs.attempt(id) {
                    None => Some(ConversationActionUnavailableReason::NotHeld),
                    Some(attempt) if frame.canonical_inputs.is_human_resolved(&attempt.attempt) => {
                        Some(ConversationActionUnavailableReason::AlreadyResolved)
                    }
                    Some(attempt) => match attempt.delivery {
                        calm_types::conversation::InputDeliveryFact::CrossingUnknown
                        | calm_types::conversation::InputDeliveryFact::Unknown => match &current {
                            Some(current) => {
                                super::conversation::retry_availability_tx(&mut tx, current, id)
                                    .await?
                                    .err()
                                    .map(retry_reason)
                            }
                            None => Some(ConversationActionUnavailableReason::UnsupportedBinding),
                        },
                        calm_types::conversation::InputDeliveryFact::NotDispatched { .. }
                        | calm_types::conversation::InputDeliveryFact::Accepted { .. } => {
                            Some(ConversationActionUnavailableReason::NotHeld)
                        }
                    },
                };
                availability.push((
                    id.clone(),
                    match reason {
                        Some(reason) => ConversationActionAvailability::Unavailable { reason },
                        None => ConversationActionAvailability::Available,
                    },
                ));
            }
            ConversationActiveInput::from_checkpoint(
                &frame.canonical_inputs,
                &carrier.id,
                &identities,
                |id| {
                    availability
                        .iter()
                        .find(|(stored, _)| stored == id)
                        .map(|(_, action)| action.clone())
                        .ok_or("submission lacks its checked retry availability")
                },
            )
            .map_err(CalmError::Conflict)?
        }
        None => None,
    };
    let terminal_outcome = match (runtime.as_ref(), canonical.as_ref()) {
        (Some(runtime), Some(frame)) => {
            match (
                frame.header.last_thread_id.as_deref(),
                frame.header.last_turn_id.as_deref(),
            ) {
                (Some(thread), Some(turn)) => {
                    super::terminal_outcome::exact(
                        &mut tx,
                        &runtime.id,
                        card,
                        row.try_get("track_id")?,
                        thread,
                        turn,
                    )
                    .await?
                }
                _ => None,
            }
        }
        _ => None,
    };
    let pending_humans = match canonical.as_ref() {
        Some(frame) => {
            super::conversation::pending_human_messages_tx(
                &mut tx,
                card,
                &frame.canonical_inputs.pending,
            )
            .await?
        }
        None => vec![],
    };
    tx.commit().await?;
    Ok(crate::db::ConversationReadFrame {
        pending_humans,
        terminal_outcome,
        runtime,
        snapshot,
        active_input,
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
    let mut connection = pool.acquire().await?;
    page_on_connection(&mut connection, card, after, limit, descending).await
}

/// The same declared presentation read under an existing storage snapshot.
pub(super) async fn page_on_connection(
    connection: &mut sqlx::SqliteConnection,
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
    // Display provenance outlives an execution claim. A checked Human retirement
    // links its predecessor input to the immutable retry root even while only an
    // ExplicitSuccessor is queued (including restored NotDispatched successors).
    // Select one backing input so retry chains never multiply presentation rows;
    // execution admission continues to require effective claims in its own owner.
    let sql = format!(
        "SELECT p.sequence,p.source_kind,p.source_identity,p.erased_by_operation_id,
                p.native_item_row_id AS backing_native_id, p.imported_provenance_json,p.import_boundary_sequence,
                {human_columns},
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
           ON p.source_kind='system_observation' AND system_input.rowid=(
             SELECT backing.rowid FROM conversation_submission_inputs backing
             JOIN conversation_input_submissions predecessor ON predecessor.id=backing.submission_id
               AND predecessor.card_id=p.card_id
             LEFT JOIN conversation_input_retry_relations retry
               ON retry.retry_intent_id=backing.retired_by_retry_intent_id
               AND retry.predecessor_submission_id=backing.submission_id AND retry.card_id=p.card_id
               AND retry.root_submission_id=COALESCE(predecessor.render_plan_root_submission_id,predecessor.id)
             WHERE backing.observation_id=p.observation_id AND backing.input_kind='system'
               AND (backing.effective=1 OR (backing.retirement_reason='human_resolution'
                    AND retry.retry_intent_id IS NOT NULL))
             ORDER BY backing.effective DESC,backing.rowid LIMIT 1)
         LEFT JOIN conversation_input_retry_relations system_retry
           ON system_retry.retry_intent_id=system_input.retired_by_retry_intent_id
         LEFT JOIN conversation_resolved_submission_plans system_submission
           ON system_submission.id=COALESCE(system_retry.root_submission_id,system_input.submission_id)
           AND system_submission.card_id=p.card_id
         LEFT JOIN json_each(system_submission.render_plan_json,'$.segments') system_claim
           ON json_extract(system_claim.value,'$.kind')='input'
           AND json_extract(system_claim.value,'$.input.kind')='system'
           AND json_extract(system_claim.value,'$.input.observation_id')=p.observation_id
           AND json_extract(system_claim.value,'$.input_position')=system_input.input_position
         {human_joins}
         LEFT JOIN harness_items i ON i.id=p.native_item_row_id AND i.card_id=p.card_id
         WHERE p.card_id=?1 AND p.sequence {comparison} ?2
           AND (p.source_kind!='imported_item' OR p.erased_by_operation_id IS NOT NULL OR NOT EXISTS (
             SELECT 1 FROM conversation_presentations native WHERE native.card_id=p.card_id
             AND native.source_kind='native_item' AND native.source_identity=p.imported_native_identity))
           AND (p.erased_by_operation_id IS NOT NULL OR NOT EXISTS (
             SELECT 1 FROM conversation_presentations erased WHERE erased.card_id=p.card_id
             AND erased.erased_by_operation_id IS NOT NULL AND erased.source_kind='imported_item'
             AND erased.imported_native_identity IS NOT NULL AND erased.imported_native_identity=
               CASE p.source_kind WHEN 'native_item' THEN p.source_identity
                 WHEN 'imported_item' THEN p.imported_native_identity END))
           ORDER BY p.sequence {order} LIMIT ?3",
        human_columns = human::COLUMNS,
        human_joins = human::JOINS,
    );
    let rows = sqlx::query(&sql)
        .bind(card)
        .bind(position)
        .bind(limit)
        .fetch_all(&mut *connection)
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
            "human_message" => human::decode(&row, card)?,
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
            "imported_item" => {
                let facts: ImportedProvenance =
                    serde_json::from_str(row.try_get("imported_provenance_json")?)?;
                let ImportedProvenance::UnknownLegacy {
                    original_row_id,
                    card_id,
                    import_boundary_sequence,
                    ..
                } = &facts;
                if card_id != card
                    || original_row_id.to_string() != row.try_get::<String, _>("source_identity")?
                    || *import_boundary_sequence
                        != row.try_get::<i64, _>("import_boundary_sequence")?
                {
                    return Err(CalmError::Conflict(
                        "imported presentation provenance mismatch",
                    ));
                }
                if removed {
                    ConversationDisplaySource::Removed {
                        identity: ConversationDisplayIdentity::Imported { provenance: facts },
                    }
                } else {
                    let raw = crate::db::rows::HarnessItemRow::from_row(&row)?;
                    if facts
                        != super::conversation::imported::provenance(
                            &raw,
                            *import_boundary_sequence,
                        )?
                    {
                        return Err(CalmError::Conflict("imported original row changed"));
                    }
                    let item = crate::model::HarnessItem::try_from(raw).map_err(|e| {
                        CalmError::Internal(format!("imported transcript decode: {e}"))
                    })?;
                    ConversationDisplaySource::ImportedItem {
                        item,
                        provenance: Box::new(facts),
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

fn retry_reason(
    reason: super::conversation::RetryUnavailableReason,
) -> ConversationActionUnavailableReason {
    use super::conversation::RetryUnavailableReason as R;
    match reason {
        R::PriorInvocationNotRecorded => {
            ConversationActionUnavailableReason::PriorInvocationNotRecorded
        }
        R::InvocationNotFullyFrozen => {
            ConversationActionUnavailableReason::InvocationNotFullyFrozen
        }
        R::AttachmentsNotFrozen => ConversationActionUnavailableReason::AttachmentsNotFrozen,
        R::ErasedOrWithdrawn => ConversationActionUnavailableReason::ErasedOrWithdrawn,
        R::UnsupportedBinding => ConversationActionUnavailableReason::UnsupportedBinding,
        R::UnsupportedSource => ConversationActionUnavailableReason::UnsupportedSource,
        R::AlreadyResolved => ConversationActionUnavailableReason::AlreadyResolved,
        R::NotHeld => ConversationActionUnavailableReason::NotHeld,
    }
}
