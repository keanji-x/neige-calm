//! Person-granted Area admin authority. Eligibility uses only transactional storage facts.
use crate::error::{CalmError, Result};
use crate::model::Track;
use sqlx::{Sqlite, SqlitePool, Transaction};

/// Discovery predicate only; protected writes still require live authority in their transaction.
pub(crate) async fn card_is_admin(pool: &SqlitePool, card_id: &str) -> Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM cards c JOIN tracks t ON t.id=c.track_id WHERE c.id=?1 AND t.admin_granted_at IS NOT NULL)",
    )
    .bind(card_id)
    .fetch_one(pool)
    .await?)
}

/// Every grant writer and the detail affordance share this predicate. Grant writers pass
/// their write transaction's connection; the detail affordance reads through the pool without a write lock.
pub async fn eligible_tx<'e>(
    executor: impl sqlx::Executor<'e, Database = Sqlite>,
    track: &Track,
) -> Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM tracks t JOIN areas a ON a.id = t.area_id \
         WHERE t.id = ?1 AND t.parent_track_id IS NULL AND t.creator_track_id IS NULL \
         AND COALESCE(t.purpose, '') <> 'area-chat' \
         AND a.kind <> 'system' AND NOT EXISTS \
         (SELECT 1 FROM managed_track_identities m WHERE m.track_id = t.id))",
    )
    .bind(track.id.as_str())
    .fetch_one(executor)
    .await?)
}

/// Grant is conditional on live eligibility; revocation is unconditional. Returns the live row.
pub async fn set_tx(tx: &mut Transaction<'_, Sqlite>, track: &Track, admin: bool) -> Result<Track> {
    if admin && !eligible_tx(&mut **tx, track).await? {
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

/// Discovery only: callers still reach each handler's transactional authority check by name.
pub const ADMIN_TOOLS: &[&str] = &["neige_track_add"];

/// Keep authority refusal distinct from a storage failure across write-transaction adapters.
pub enum RequireAdminError {
    NotAdmin,
    Storage(CalmError),
}

/// Require a live, open Admin in the same write transaction as the protected operation.
pub async fn require_admin_tx(
    tx: &mut Transaction<'_, Sqlite>,
    caller_track_id: &str,
) -> std::result::Result<(), RequireAdminError> {
    let admin: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM tracks WHERE id=?1 AND admin_granted_at IS NOT NULL AND closed_at IS NULL)",
    )
    .bind(caller_track_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(|error| RequireAdminError::Storage(error.into()))?;
    if !admin {
        return Err(RequireAdminError::NotAdmin);
    }
    Ok(())
}
