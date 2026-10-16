//! HTTP ingress translates authenticated deployment authority; truth owns user
//! membership policy. All protected Area mutations recheck in their write tx.
use crate::auth::Principal;
use crate::db::RouteRepo;
use crate::error::{CalmError, Result};
use crate::model::{Area, AreaKind};
use calm_types::organization::AreaAccess;
use sqlx::SqliteConnection;

/// Preflight before filesystem/process effects, through the declared read contract.
pub(super) async fn require_area_access(
    repo: &dyn RouteRepo,
    area_id: &str,
    principal: &Principal,
    access: AreaAccess,
) -> Result<Area> {
    repo.application_user_require_registered(&principal.user_id)
        .await?;
    let area = repo
        .area_get(area_id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("area {area_id}")))?;
    if system_area_access(&area, principal, access)? {
        return Ok(area);
    }
    Ok(repo
        .area_require_access(area_id, &principal.user_id, access)
        .await?)
}

/// Revalidate the same policy in the transaction committing the protected write.
pub(super) async fn require_area_access_tx(
    connection: &mut SqliteConnection,
    area_id: &str,
    principal: &Principal,
    access: AreaAccess,
) -> Result<Area> {
    use crate::db::sqlite::area_membership;
    area_membership::application_user_require_registered(connection, &principal.user_id).await?;
    let area = area_membership::area_get(connection, area_id).await?;
    if system_area_access(&area, principal, access)? {
        return Ok(area);
    }
    Ok(
        area_membership::area_require_access(connection, area_id, &principal.user_id, access)
            .await?,
    )
}

fn system_area_access(area: &Area, principal: &Principal, access: AreaAccess) -> Result<bool> {
    if area.kind != AreaKind::System {
        return Ok(false);
    }
    require_deployment_owner(principal)?;
    if access == AreaAccess::Delete {
        return Err(CalmError::Forbidden("system Area cannot be deleted".into()));
    }
    Ok(true)
}

pub(super) fn require_deployment_owner(principal: &Principal) -> Result<()> {
    if !principal.is_deployment_owner() {
        return Err(CalmError::Forbidden("deployment owner required".into()));
    }
    Ok(())
}
