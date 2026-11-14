//! Accepted execution consumes the live share owner policy without access revocation.
use super::*;
use crate::auth::LegacyPrivateExecution;
use calm_types::organization::AreaAccess;

pub(crate) async fn authorize_accepted_tx(
    tx: &mut Transaction<'_, Sqlite>,
    execution: &LegacyPrivateExecution,
    delegation: &ShareDelegation,
    card: &str,
) -> Result<String> {
    if execution.current_owner()? != delegation.approver_user_id {
        return Err(CalmError::Forbidden("share issuer changed".into()));
    }
    let live = crate::db::sqlite::share_links::get_tx(tx, &delegation.id).await?;
    if live.delegation != *delegation
        || delegation.purpose != SharePurpose::Conversation
        || delegation.resource_policy != ShareResourcePolicy::CurrentDeployment
    {
        return Err(CalmError::Forbidden("share delegation changed".into()));
    }
    let (track, area, purpose): (String, String, Option<String>) = sqlx::query_as(
            "SELECT t.id,t.area_id,t.purpose FROM cards c JOIN tracks t ON t.id=c.track_id WHERE c.id=?1")
            .bind(card).fetch_optional(&mut **tx).await?
            .ok_or_else(|| CalmError::NotFound("shared conversation".into()))?;
    let contained = purpose.as_deref() != Some(crate::AREA_CHAT_PURPOSE)
        && match &delegation.scope {
            ShareScope::Track(root) => {
                let root_area: String =
                    sqlx::query_scalar("SELECT area_id FROM tracks WHERE id=?1")
                        .bind(root)
                        .fetch_one(&mut **tx)
                        .await?;
                root_area == area
                    && sqlite::share_links::track_tree_contains(tx, root, &track).await?
            }
            ShareScope::Area(id) => id == &area,
        };
    if !contained {
        return Err(CalmError::Forbidden(
            "share scope no longer contains conversation".into(),
        ));
    }
    crate::db::sqlite::area_membership::area_require_access(
        tx,
        &area,
        &delegation.approver_user_id,
        AreaAccess::Delete,
    )
    .await?;
    if execution.current_owner()? != delegation.approver_user_id {
        return Err(CalmError::Forbidden("share issuer changed".into()));
    }
    Ok(delegation.approver_user_id.clone())
}
