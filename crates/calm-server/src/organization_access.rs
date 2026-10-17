//! Application organization access: verified identity/deployment authority is
//! interpreted once here; truth owns UserArea membership policy. Ingress owners
//! revalidate this contract in the transaction committing their protected work.
use crate::auth::Principal;
use crate::db::RouteRepo;
use crate::error::{CalmError, Result};
use crate::model::{Area, AreaKind};
use calm_types::organization::{AreaAccess, AreaMemberRole};
use sqlx::SqliteConnection;

/// Preflight before filesystem/process effects, through the declared read contract.
pub(crate) async fn require_area_access(
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
pub(crate) async fn require_area_access_tx(
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

/// Consume membership facts from a single current registered-user/Area read.
/// The snapshot is data only; callers must re-read at their publication boundary.
pub(crate) fn require_area_access_from_membership(
    area: &Area,
    member_role: Option<AreaMemberRole>,
    principal: &Principal,
    access: AreaAccess,
) -> Result<()> {
    if system_area_access(area, principal, access)? {
        return Ok(());
    }
    crate::db::sqlite::area_membership::require_member_access(member_role, access)?;
    Ok(())
}

pub(crate) fn require_deployment_owner(principal: &Principal) -> Result<()> {
    if !principal.is_deployment_owner() {
        return Err(CalmError::Forbidden("deployment owner required".into()));
    }
    Ok(())
}

/// Legacy native execution spends this deployment's credentials. Collaboration
/// membership and ownership of an unrelated user's Private Area do not grant it.
pub(crate) async fn require_legacy_input_execution_tx(
    connection: &mut SqliteConnection,
    area_id: &str,
    principal: &Principal,
) -> Result<()> {
    let area =
        require_area_access_tx(connection, area_id, principal, AreaAccess::Collaborate).await?;
    require_deployment_owner(principal)?;
    if area.kind != AreaKind::System
        && crate::db::sqlite::area_membership::area_member_role(
            connection,
            area_id,
            &principal.user_id,
        )
        .await?
            != Some(calm_types::organization::AreaMemberRole::Owner)
    {
        return Err(CalmError::Forbidden(
            "legacy deployment execution requires ownership of this Area".into(),
        ));
    }
    Ok(())
}
