//! One body-free display order, separate from native rewind anchors.
use crate::error::{CalmError, Result};
use calm_types::conversation::{ConversationMessageId, SystemObservationId};
use sqlx::{Sqlite, Transaction};

type NativeItemIdentityRow = (String, Option<String>, Option<String>, String, String);

pub enum PresentationSource<'a> {
    HumanMessage(&'a ConversationMessageId),
    SystemObservation(&'a SystemObservationId),
    NativeItem {
        row_id: i64,
    },
    Legacy {
        source_locator: &'a str,
        segment: u32,
    },
}

/// The same persisted native/source identity reuses its sequence after updates,
/// receipt reconstruction and erasure. Body equality never participates.
pub async fn allocate_presentation_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: &str,
    source: PresentationSource<'_>,
) -> Result<i64> {
    let (kind, identity, message, observation, native_row, locator, segment) = match source {
        PresentationSource::HumanMessage(id) => (
            "human_message",
            id.as_str().to_owned(),
            Some(id.as_str()),
            None,
            None,
            None,
            None,
        ),
        PresentationSource::SystemObservation(id) => (
            "system_observation",
            id.as_str().to_owned(),
            None,
            Some(id.as_str()),
            None,
            None,
            None,
        ),
        PresentationSource::NativeItem { row_id } => {
            let (identity, current_row) = native_identity_tx(tx, card_id, row_id).await?;
            (
                "native_item",
                identity,
                None,
                None,
                Some(current_row),
                None,
                None,
            )
        }
        PresentationSource::Legacy {
            source_locator,
            segment,
        } => (
            "legacy",
            serde_json::to_string(&(source_locator, segment))?,
            None,
            None,
            None,
            Some(source_locator),
            Some(i64::from(segment)),
        ),
    };
    let sequence: Option<i64> = sqlx::query_scalar("INSERT INTO conversation_presentations(card_id,source_kind,source_identity,message_id,observation_id,
         native_item_row_id,legacy_source_locator,legacy_segment)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(card_id,source_kind,source_identity) DO UPDATE
         SET native_item_row_id=CASE
         WHEN excluded.source_kind='native_item' THEN excluded.native_item_row_id ELSE
         conversation_presentations.native_item_row_id
         END
         WHERE conversation_presentations.erased_by_operation_id IS NULL
         RETURNING sequence")
        .bind(card_id).bind(kind).bind(&identity).bind(message).bind(observation).bind(native_row).bind(locator).bind(segment)
        .fetch_optional(&mut **tx).await?;
    sequence.ok_or_else(|| {
        CalmError::Conflict("erased or retired presentation identity cannot be resurrected")
    })
}

/// Native identity belongs to the persisted thread/item pair, independent of
/// carrier generation, row replacement, method updates or equal text. A row
/// without a provable native identity must use the explicit legacy locator path.
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
    let item = item.filter(|id| !id.is_empty()).ok_or_else(|| {
        CalmError::Conflict("native presentation requires a persisted native item identity")
    })?;
    if thread.is_empty() {
        return Err(CalmError::Conflict(
            "native presentation requires its persisted thread identity",
        ));
    }
    let params: serde_json::Value = serde_json::from_str(&params)?;
    if calm_types::harness::is_native_user_message_type(item_type.as_deref())
        || item_type.as_deref().is_none_or(str::is_empty)
        || params
            .get("_projection")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
        || !matches!(method.as_str(), "item/started" | "item/completed")
    {
        return Err(CalmError::Conflict(
            "input projections and echoes use their canonical source presentation, not NativeItem",
        ));
    }
    // Completed is terminal for this native item identity. A late started frame
    // cannot regress the current reference; receipt rebuilds use fresh row IDs.
    let current_row:i64=sqlx::query_scalar("SELECT id
         FROM harness_items
         WHERE card_id=?1 AND thread_id=?2 AND item_uuid=?3 AND item_type IS NOT NULL AND length(item_type)>0
         AND item_type NOT IN ('userMessage','user_message') AND method IN ('item/started','item/completed')
         AND json_valid(params) AND json_extract(params,'$._projection') IS NOT 1 ORDER BY (method=
         'item/completed') DESC,id DESC LIMIT 1")
        .bind(card_id).bind(&thread).bind(&item).fetch_one(&mut **tx).await?;
    Ok((serde_json::to_string(&(thread, item))?, current_row))
}
