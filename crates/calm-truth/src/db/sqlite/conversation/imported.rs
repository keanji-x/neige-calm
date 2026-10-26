//! Explicit single-row display primitive. No scanner, population caller or input acceptance.
use crate::db::rows::HarnessItemRow as HistoricalRow;
use crate::error::{CalmError, Result};
use calm_types::conversation_display::ImportedProvenance;
use sqlx::{Row, Sqlite, Transaction};

pub(in crate::db::sqlite) fn provenance(
    row: &HistoricalRow,
    boundary: i64,
) -> Result<ImportedProvenance> {
    Ok(ImportedProvenance::UnknownLegacy {
        original_row_id: row.id,
        worker_session_id: row.worker_session_id.clone(),
        card_id: row.card_id.to_string(),
        track_id: row.track_id.to_string(),
        thread_id: row.thread_id.clone(),
        turn_id: row.turn_id.clone(),
        item_uuid: row.item_uuid.clone(),
        item_type: row.item_type.clone(),
        method: row.method.clone(),
        created_at_ms: row.created_at_ms,
        original_digest: format!(
            "sha256-v1:{}",
            super::digest(&(&row.params, &row.input_segments))?
        ),
        import_boundary_sequence: boundary,
    })
}

/// Exclusion uses the native owner's contract. Echoes/missing IDs have no native identity.
fn native_identity(row: &HistoricalRow) -> Result<Option<String>> {
    let params = serde_json::from_str(&row.params)?;
    Ok(super::presentation::native_source_identity(
        &row.thread_id,
        row.item_uuid.as_deref(),
        row.item_type.as_deref(),
        &row.method,
        &params,
    )
    .ok())
}

/// Caller supplies the exact pre-import boundary in its transaction. Imports append
/// by original row ID; retries preserve the frozen identity, even after erasure.
pub async fn import_item_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card: &str,
    row_id: i64,
    boundary: i64,
) -> Result<Option<i64>> {
    let existing = sqlx::query(
        "SELECT sequence,import_boundary_sequence FROM conversation_presentations
        WHERE card_id=?1 AND source_kind='imported_item' AND source_identity=?2",
    )
    .bind(card)
    .bind(row_id.to_string())
    .fetch_optional(&mut **tx)
    .await?;
    if let Some(existing) = existing {
        if existing.try_get::<i64, _>("import_boundary_sequence")? != boundary {
            return Err(CalmError::Conflict("import boundary changed"));
        }
        return Ok(Some(existing.try_get("sequence")?));
    }
    let raw: HistoricalRow =
        sqlx::query_as("SELECT * FROM harness_items WHERE id=?1 AND card_id=?2")
            .bind(row_id)
            .bind(card)
            .fetch_optional(&mut **tx)
            .await?
            .ok_or(CalmError::Conflict(
                "import requires its original conversation row",
            ))?;
    let native = native_identity(&raw)?;
    if let Some(identity) = &native
        && super::presentation::native_identity_erased_tx(tx, card, identity).await?
    {
        return Ok(None);
    }
    let indexed: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM conversation_presentations
        WHERE card_id=?1 AND source_kind='native_item' AND source_identity=?2)",
    )
    .bind(card)
    .bind(&native)
    .fetch_one(&mut **tx)
    .await?;
    if indexed {
        return Ok(None);
    }
    let bounds: (i64, Option<i64>, Option<i64>, Option<i64>) = sqlx::query_as(
        "SELECT COALESCE(MAX(CASE WHEN source_kind!='imported_item' THEN sequence END),0),
         MIN(import_boundary_sequence),MAX(import_boundary_sequence),
         MAX(CASE WHEN source_kind='imported_item' THEN CAST(source_identity AS INTEGER) END)
         FROM conversation_presentations WHERE card_id=?1",
    )
    .bind(card)
    .fetch_one(&mut **tx)
    .await?;
    if boundary < 0
        || bounds.0 > boundary
        || bounds.1.is_some_and(|b| b != boundary)
        || bounds.2.is_some_and(|b| b != boundary)
        || bounds.3.is_some_and(|id| id >= row_id)
        || (bounds.1.is_none() && bounds.0 != boundary)
    {
        return Err(CalmError::Conflict(
            "import requires stable row order and explicit current boundary",
        ));
    }
    let facts = serde_json::to_string(&provenance(&raw, boundary)?)?;
    Ok(Some(sqlx::query_scalar("INSERT INTO conversation_presentations(card_id,source_kind,source_identity,
        native_item_row_id,imported_provenance_json,import_boundary_sequence,imported_native_identity)
        VALUES(?1,'imported_item',?2,?3,?4,?5,?6) RETURNING sequence")
        .bind(card).bind(row_id.to_string()).bind(row_id).bind(facts).bind(boundary).bind(native)
        .fetch_one(&mut **tx).await?))
}
