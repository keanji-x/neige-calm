//! Cumulative template capabilities; callers own the Track creation or binding transaction.
use crate::error::Result;
use calm_types::ids::TrackId;
use calm_types::track_capability::TrackCapability;
use sqlx::{Sqlite, Transaction};

pub async fn stamp_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track: &TrackId,
    capabilities: &[TrackCapability],
) -> Result<()> {
    for capability in capabilities {
        // The immutable-row trigger fires before SQLite's conflict handling. Existing facts
        // must be left untouched, including on a repeated template binding.
        if has_tx(tx, track, *capability).await? {
            continue;
        }
        sqlx::query("INSERT INTO track_capabilities(track_id,capability) VALUES(?1,?2)")
            .bind(track.as_str())
            .bind(capability.as_db_str())
            .execute(&mut **tx)
            .await?;
    }
    Ok(())
}

pub async fn has_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track: &TrackId,
    capability: TrackCapability,
) -> Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM track_capabilities WHERE track_id=?1 AND capability=?2)",
    )
    .bind(track.as_str())
    .bind(capability.as_db_str())
    .fetch_one(&mut **tx)
    .await?)
}
