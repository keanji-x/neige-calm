//! Person-granted Area admin authority. Eligibility uses only transactional storage facts.
use crate::error::{CalmError, Result};
use crate::model::Track;
use sqlx::{Sqlite, Transaction};

/// Every grant writer and the detail affordance share this predicate. No plugin host is needed.
pub async fn eligible_tx(tx: &mut Transaction<'_, Sqlite>, track: &Track) -> Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM tracks t JOIN areas a ON a.id = t.area_id \
         WHERE t.id = ?1 AND t.parent_track_id IS NULL AND t.creator_track_id IS NULL \
         AND COALESCE(t.purpose, '') <> 'area-chat' AND t.plugin_scope IS NULL \
         AND a.kind <> 'system' AND NOT EXISTS \
         (SELECT 1 FROM managed_track_identities m WHERE m.track_id = t.id))",
    )
    .bind(track.id.as_str())
    .fetch_one(&mut **tx)
    .await?)
}

/// Grant is conditional on live eligibility; revocation is unconditional. Returns the live row.
pub async fn set_tx(tx: &mut Transaction<'_, Sqlite>, track: &Track, admin: bool) -> Result<Track> {
    if admin && !eligible_tx(tx, track).await? {
        return Err(CalmError::Forbidden(
            "ineligible: track cannot be an Area admin".into(),
        ));
    }
    sqlx::query("UPDATE tracks SET admin_granted_at = ?2, updated_at = ?3 WHERE id = ?1")
        .bind(track.id.as_str())
        .bind(if admin {
            Some(track.admin_granted_at.unwrap_or_else(crate::model::now_ms))
        } else {
            None
        })
        .bind(crate::model::now_ms())
        .execute(&mut **tx)
        .await?;
    crate::db::sqlite::track_get_tx(tx, &track.id).await
}
