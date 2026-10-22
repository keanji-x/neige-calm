//! One body-free display order, separate from native rewind anchors.
use crate::error::{CalmError, Result};
use calm_types::conversation::{ConversationMessageId, SystemObservationId};
use sqlx::{Sqlite, Transaction};

#[derive(Clone, Debug)]
pub struct StoredNativePresentation {
    pub raw_row_id: i64,
    pub cursor: calm_types::conversation_display::ConversationCursor,
}

type NativeItemIdentityRow = (String, Option<String>, Option<String>, String, String);

/// Content is a native thread/item observation, never an input acceptance or END
/// proof. Keep its body and display reference in the same erasure transaction.
pub async fn observe_native_item_tx(
    tx: &mut Transaction<'_, Sqlite>,
    carrier: &super::CurrentInputCarrier,
    thread: &str,
    turn: Option<&str>,
    method: &str,
    params: &serde_json::Value,
) -> Result<Option<StoredNativePresentation>> {
    use calm_types::conversation::InputDeliveryFact;
    let frame: calm_types::harness::snapshot::CanonicalHarnessSnapshot =
        serde_json::from_value(carrier.snapshot().clone())?;
    // A crossing is enough to observe content: requiring Accepted or the active
    // native turn loses early replies and historical items after the next claim.
    if frame.header.last_thread_id.as_deref() != Some(thread)
        || !carrier.checkpoint().attempts.iter().any(|attempt| {
            attempt.correlation.thread_id == thread
                && !matches!(attempt.delivery, InputDeliveryFact::NotDispatched { .. })
        })
    {
        return Ok(None);
    }
    let item = params
        .pointer("/item/id")
        .and_then(serde_json::Value::as_str);
    let kind = params
        .pointer("/item/type")
        .and_then(serde_json::Value::as_str);
    // Human/System input presentation is already canonical. A native echo may
    // neither make another body nor restore an erased source payload.
    if calm_types::harness::is_native_user_message_type(kind)
        || params
            .get("_projection")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
    {
        return Ok(None);
    }
    let identity = native_source_identity(thread, item, kind, method, params)?;
    let erased: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM conversation_presentations
         WHERE card_id=?1 AND source_kind='native_item' AND source_identity=?2
         AND erased_by_operation_id IS NOT NULL)",
    )
    .bind(carrier.card_id())
    .bind(&identity)
    .fetch_one(&mut **tx)
    .await?;
    if erased {
        return Ok(None);
    }
    // Receipt settlement can overtake queued live notifications. Replaying an
    // identical frame or a start after its completion must not replace the
    // settled presentation's raw row (including its id and creation time).
    let params_json = serde_json::to_string(params)?;
    let existing: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM harness_items WHERE card_id=?1 AND thread_id=?2
         AND item_uuid=?3 AND ((method=?4 AND params=?5)
         OR (method='item/completed' AND ?4='item/started'))
         ORDER BY (method='item/completed') DESC,id DESC LIMIT 1",
    )
    .bind(carrier.card_id())
    .bind(thread)
    .bind(item)
    .bind(method)
    .bind(&params_json)
    .fetch_optional(&mut **tx)
    .await?;
    let row = if let Some(row) = existing {
        row
    } else {
        let mut proof =
            super::current_native_fact_tx(tx, carrier.card_id(), carrier.worker_session_id(), true)
                .await?;
        super::super::transcript_projection::insert_transcript_row(
            &mut proof,
            thread,
            turn,
            item,
            kind,
            method,
            &params_json,
            None,
        )
        .await?
    };
    let sequence = allocate_presentation_tx(
        tx,
        carrier.card_id(),
        PresentationSource::NativeItem { row_id: row },
    )
    .await?;
    Ok(Some(StoredNativePresentation {
        raw_row_id: row,
        cursor: calm_types::conversation_display::ConversationCursor::new(
            carrier.card_id(),
            calm_types::conversation_display::ConversationCursorSource::Presentation,
            sequence,
        ),
    }))
}

pub(in crate::db::sqlite) fn native_source_identity(
    thread: &str,
    item: Option<&str>,
    kind: Option<&str>,
    method: &str,
    params: &serde_json::Value,
) -> Result<String> {
    let item = item.filter(|id| !id.is_empty()).ok_or_else(|| {
        CalmError::Conflict("native presentation requires a persisted native item identity")
    })?;
    if thread.is_empty() {
        return Err(CalmError::Conflict(
            "native presentation requires its persisted thread identity",
        ));
    }
    if calm_types::harness::is_native_user_message_type(kind)
        || kind.is_none_or(str::is_empty)
        || params
            .get("_projection")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
        || !matches!(method, "item/started" | "item/completed")
    {
        return Err(CalmError::Conflict(
            "input projections and echoes use their canonical source presentation, not NativeItem",
        ));
    }
    Ok(serde_json::to_string(&(thread, item))?)
}

pub enum PresentationSource<'a> {
    HumanMessage(&'a ConversationMessageId),
    SystemObservation(&'a SystemObservationId),
    NativeItem { row_id: i64 },
}

/// The same persisted native/source identity reuses its sequence after updates,
/// receipt reconstruction and erasure. Body equality never participates.
pub async fn allocate_presentation_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: &str,
    source: PresentationSource<'_>,
) -> Result<i64> {
    let (kind, identity, message, observation, native_row) = match source {
        PresentationSource::HumanMessage(id) => (
            "human_message",
            id.as_str().to_owned(),
            Some(id.as_str()),
            None,
            None,
        ),
        PresentationSource::SystemObservation(id) => (
            "system_observation",
            id.as_str().to_owned(),
            None,
            Some(id.as_str()),
            None,
        ),
        PresentationSource::NativeItem { row_id } => {
            let (identity, current_row) = native_identity_tx(tx, card_id, row_id).await?;
            ("native_item", identity, None, None, Some(current_row))
        }
    };
    let sequence: Option<i64> = sqlx::query_scalar("INSERT INTO conversation_presentations(card_id,source_kind,source_identity,message_id,observation_id,
         native_item_row_id)
         VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(card_id,source_kind,source_identity) DO UPDATE
         SET native_item_row_id=CASE
         WHEN excluded.source_kind='native_item' THEN excluded.native_item_row_id ELSE
         conversation_presentations.native_item_row_id
         END
         WHERE conversation_presentations.erased_by_operation_id IS NULL
         RETURNING sequence")
        .bind(card_id).bind(kind).bind(&identity).bind(message).bind(observation).bind(native_row)
        .fetch_optional(&mut **tx).await?;
    sequence.ok_or_else(|| {
        CalmError::Conflict("erased or retired presentation identity cannot be resurrected")
    })
}

/// Native identity belongs to the persisted thread/item pair, independent of
/// carrier generation, row replacement, method updates or equal text. A row
/// without a provable native identity cannot receive a canonical presentation.
async fn native_identity_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: &str,
    row_id: i64,
) -> Result<(String, i64)> {
    let row: Option<NativeItemIdentityRow> =
        sqlx::query_as("SELECT thread_id,item_uuid,item_type,method,params FROM harness_items WHERE id=?1 AND card_id=?2")
            .bind(row_id)
            .bind(card_id)
            .fetch_optional(&mut **tx)
            .await?;
    let (thread, item, item_type, method, params) =
        row.ok_or_else(|| CalmError::Conflict("native item does not belong to this conversation"))?;
    let params: serde_json::Value = serde_json::from_str(&params)?;
    let identity = native_source_identity(
        &thread,
        item.as_deref(),
        item_type.as_deref(),
        &method,
        &params,
    )?;
    // Completed is terminal for this native item identity. A late started frame
    // cannot regress the current reference; receipt rebuilds use fresh row IDs.
    let current_row:i64=sqlx::query_scalar("SELECT id
         FROM harness_items
         WHERE card_id=?1 AND thread_id=?2 AND item_uuid=?3 AND item_type IS NOT NULL AND length(item_type)>0
         AND item_type NOT IN ('userMessage','user_message') AND method IN ('item/started','item/completed')
         AND json_valid(params) AND json_extract(params,'$._projection') IS NOT 1 ORDER BY (method=
         'item/completed') DESC,id DESC LIMIT 1")
        .bind(card_id).bind(&thread).bind(&item).fetch_one(&mut **tx).await?;
    Ok((identity, current_row))
}
