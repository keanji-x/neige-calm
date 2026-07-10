use sqlx::Sqlite;
use sqlx::Transaction;

use crate::error::{CalmError, Result};
use crate::model::*;

pub async fn overlay_upsert_tx(tx: &mut Transaction<'_, Sqlite>, p: NewOverlay) -> Result<Overlay> {
    let now = now_ms();
    let new_id_str = new_id();
    let payload_text = serde_json::to_string(&p.payload)?;
    let row = sqlx::query_as::<_, crate::db::rows::OverlayRow>(
        r#"INSERT INTO overlays
               (id, plugin_id, entity_kind, entity_id, kind, payload, updated_at)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
           ON CONFLICT(plugin_id, entity_kind, entity_id, kind)
             DO UPDATE SET payload = excluded.payload,
                           updated_at = excluded.updated_at
           RETURNING id, plugin_id, entity_kind, entity_id, kind, payload, updated_at"#,
    )
    .bind(&new_id_str)
    .bind(&p.plugin_id)
    .bind(&p.entity_kind)
    .bind(&p.entity_id)
    .bind(&p.kind)
    .bind(&payload_text)
    .bind(now)
    .fetch_one(&mut **tx)
    .await?;
    Ok(Overlay::from(row))
}

pub async fn overlay_delete_tx(
    tx: &mut Transaction<'_, Sqlite>,
    plugin_id: &str,
    entity_kind: &str,
    entity_id: &str,
    kind: &str,
) -> Result<()> {
    let res = sqlx::query(
        r#"DELETE FROM overlays
           WHERE plugin_id = ?1 AND entity_kind = ?2 AND entity_id = ?3 AND kind = ?4"#,
    )
    .bind(plugin_id)
    .bind(entity_kind)
    .bind(entity_id)
    .bind(kind)
    .execute(&mut **tx)
    .await?;
    if res.rows_affected() == 0 {
        return Err(CalmError::NotFound("overlay"));
    }
    Ok(())
}

/// Drop every overlay row addressed at the given `(entity_kind, entity_id)`,
/// across all `plugin_id`s and all `kind`s. Used by the card / wave / cove
/// delete paths to keep the table from growing orphans; the `overlays`
/// schema has no FK because SQLite can't express polymorphic ones.
pub async fn overlay_delete_by_entity_tx(
    tx: &mut Transaction<'_, Sqlite>,
    entity_kind: &str,
    entity_id: &str,
) -> Result<u64> {
    let res = sqlx::query("DELETE FROM overlays WHERE entity_kind = ?1 AND entity_id = ?2")
        .bind(entity_kind)
        .bind(entity_id)
        .execute(&mut **tx)
        .await?;
    Ok(res.rows_affected())
}

/// Drop every card-scoped overlay for cards still belonging to the given
/// wave, in the caller's transaction. Race-safe replacement for "read
/// cards_by_wave, loop, sweep" — the IN subquery sees the same DB state
/// the subsequent `wave_delete_tx` cascade will see, so a card+overlay
/// created between an outside-tx snapshot and the txn commit is still
/// caught.
pub async fn overlay_delete_card_overlays_by_wave_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &str,
) -> Result<u64> {
    let res = sqlx::query(
        r#"DELETE FROM overlays
           WHERE entity_kind = 'card'
             AND entity_id IN (SELECT id FROM cards WHERE wave_id = ?1)"#,
    )
    .bind(wave_id)
    .execute(&mut **tx)
    .await?;
    Ok(res.rows_affected())
}

/// Drop every card-scoped and wave-scoped (`'wave'` + `'view'`) overlay
/// under the cove, in the caller's transaction. Same race-safe shape as
/// `overlay_delete_card_overlays_by_wave_tx`. Caller still needs to
/// sweep `('cove', cove_id)` separately — plugins may address overlays
/// at the cove kind.
pub async fn overlay_delete_subtree_by_cove_tx(
    tx: &mut Transaction<'_, Sqlite>,
    cove_id: &str,
) -> Result<u64> {
    // Three statements, accumulated count. Could be one nested query but
    // splitting keeps each delete legible and individually optimizable.
    let cards = sqlx::query(
        r#"DELETE FROM overlays
           WHERE entity_kind = 'card'
             AND entity_id IN (
               SELECT c.id FROM cards c
               JOIN waves w ON w.id = c.wave_id
               WHERE w.cove_id = ?1
             )"#,
    )
    .bind(cove_id)
    .execute(&mut **tx)
    .await?
    .rows_affected();
    let waves = sqlx::query(
        r#"DELETE FROM overlays
           WHERE entity_kind IN ('wave', 'view')
             AND entity_id IN (SELECT id FROM waves WHERE cove_id = ?1)"#,
    )
    .bind(cove_id)
    .execute(&mut **tx)
    .await?
    .rows_affected();
    Ok(cards + waves)
}
