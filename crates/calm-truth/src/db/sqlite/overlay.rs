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

/// The `overlays` schema has no FK (SQLite can't express polymorphic ones), so
/// entity delete paths sweep here explicitly.
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

/// The IN subquery sees the same DB state the subsequent cascade will see, so a
/// card+overlay created after an outside-tx snapshot is still caught.
pub async fn overlay_delete_card_overlays_by_track_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track_id: &str,
) -> Result<u64> {
    let res = sqlx::query(
        r#"DELETE FROM overlays
           WHERE entity_kind = 'card'
             AND entity_id IN (SELECT id FROM cards WHERE track_id = ?1)"#,
    )
    .bind(track_id)
    .execute(&mut **tx)
    .await?;
    Ok(res.rows_affected())
}

/// Caller still sweeps `('area', area_id)` separately.
pub async fn overlay_delete_subtree_by_area_tx(
    tx: &mut Transaction<'_, Sqlite>,
    area_id: &str,
) -> Result<u64> {
    let cards = sqlx::query(
        r#"DELETE FROM overlays
           WHERE entity_kind = 'card'
             AND entity_id IN (
               SELECT c.id FROM cards c
               JOIN tracks w ON w.id = c.track_id
               WHERE w.area_id = ?1
             )"#,
    )
    .bind(area_id)
    .execute(&mut **tx)
    .await?
    .rows_affected();
    let tracks = sqlx::query(
        r#"DELETE FROM overlays
           WHERE entity_kind IN ('track', 'view')
             AND entity_id IN (SELECT id FROM tracks WHERE area_id = ?1)"#,
    )
    .bind(area_id)
    .execute(&mut **tx)
    .await?
    .rows_affected();
    Ok(cards + tracks)
}
