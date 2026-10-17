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

/// Every grant writer and the detail affordance share the same live storage facts.
/// Writers pass their transaction's connection; discovery reads through the pool.
pub async fn eligible_tx<'e>(
    executor: impl sqlx::Executor<'e, Database = Sqlite>,
    track: &Track,
) -> Result<bool> {
    Ok(ineligibility_reason_tx(executor, track).await?.is_none())
}

pub async fn require_eligible_tx<'e>(
    executor: impl sqlx::Executor<'e, Database = Sqlite>,
    track: &Track,
) -> Result<()> {
    if let Some(reason) = ineligibility_reason_tx(executor, track).await? {
        return Err(CalmError::Forbidden(format!("ineligible: {reason}")));
    }
    Ok(())
}

#[derive(sqlx::FromRow)]
struct EligibilityFacts {
    child: bool,
    created_by_track: bool,
    area_chat: bool,
    plugin_scope: Option<String>,
    template_id: Option<String>,
    system_area: bool,
    managed: bool,
}

async fn ineligibility_reason_tx<'e>(
    executor: impl sqlx::Executor<'e, Database = Sqlite>,
    track: &Track,
) -> Result<Option<String>> {
    let facts: Option<EligibilityFacts> = sqlx::query_as(
        "SELECT t.parent_track_id IS NOT NULL AS child, \
         t.creator_track_id IS NOT NULL AS created_by_track, \
         COALESCE(t.purpose, '') = 'area-chat' AS area_chat, t.plugin_scope, t.template_id, \
         a.kind = 'system' AS system_area, EXISTS \
         (SELECT 1 FROM managed_track_identities m WHERE m.track_id = t.id) AS managed \
         FROM tracks t JOIN areas a ON a.id = t.area_id WHERE t.id = ?1",
    )
    .bind(track.id.as_str())
    .fetch_optional(executor)
    .await?;
    let Some(facts) = facts else {
        return Ok(Some(
            "Track or Area no longer exists. Refresh the Area and choose another Track.".into(),
        ));
    };
    let (reason, action): (String, &str) = if facts.child {
        (
            "child Tracks cannot be Area admins.".into(),
            "Create a new top-level Track directly in this Area, then grant admin.",
        )
    } else if facts.created_by_track {
        (
            "Tracks created by another Track cannot be Area admins.".into(),
            "Create a new top-level Track yourself in this Area, then grant admin.",
        )
    } else if facts.area_chat {
        (
            "Area chat Tracks cannot be Area admins.".into(),
            "Create a separate top-level Track in this Area, then grant admin.",
        )
    } else if let Some(plugin) = facts.plugin_scope {
        let template = facts
            .template_id
            .map(|id| format!(" (template `{id}`)"))
            .unwrap_or_default();
        (
            format!("Track is bound to plugin `{plugin}`{template} and cannot be an Area admin."),
            "Create a new top-level Track without a plugin-bound template, then grant admin.",
        )
    } else if facts.system_area {
        (
            "Tracks in a system Area cannot be Area admins.".into(),
            "Create a top-level Track in a non-system Area, then grant admin.",
        )
    } else if facts.managed {
        (
            "Tracks with a managed identity cannot be Area admins.".into(),
            "Create a separate top-level Track without a managed identity, then grant admin.",
        )
    } else {
        return Ok(None);
    };
    Ok(Some(format!("{reason} {action}")))
}

/// Grant is conditional on live eligibility; revocation is unconditional. Returns the live row.
pub async fn set_tx(tx: &mut Transaction<'_, Sqlite>, track: &Track, admin: bool) -> Result<Track> {
    if admin {
        require_eligible_tx(&mut **tx, track).await?;
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
