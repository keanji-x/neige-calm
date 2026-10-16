//! Authoritative registry and membership policy. Connections can be a protected
//! write transaction or a read snapshot; callers do not cache membership grants.
use sqlx::SqliteConnection;

use crate::error::{CalmError, Result};
use crate::model::{Area, AreaKind, now_ms};
use calm_types::organization::{AreaAccess, AreaMemberRole};

/// Profile reconciliation is explicit startup work. This does not issue a
/// credential, assign a deployment role, or repair any Area membership.
pub async fn application_user_register(
    connection: &mut SqliteConnection,
    user_id: &str,
    display_name: &str,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO application_users(id, display_name, created_at)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(id) DO UPDATE SET display_name=excluded.display_name",
    )
    .bind(user_id)
    .bind(display_name)
    .bind(now_ms())
    .execute(connection)
    .await?;
    Ok(())
}

pub async fn application_user_require_registered(
    connection: &mut SqliteConnection,
    user_id: &str,
) -> Result<()> {
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM application_users WHERE id=?1)")
            .bind(user_id)
            .fetch_one(connection)
            .await?;
    if !exists {
        return Err(CalmError::Forbidden(
            "application user is not registered".into(),
        ));
    }
    Ok(())
}

/// System Areas deliberately have no membership. Their access is an explicit
/// deployment-authority decision at the owning ingress, not an Area-owner role.
pub async fn area_require_access(
    connection: &mut SqliteConnection,
    area_id: &str,
    user_id: &str,
    access: AreaAccess,
) -> Result<Area> {
    application_user_require_registered(connection, user_id).await?;
    let area = area_get(connection, area_id).await?;
    if area.kind == AreaKind::System {
        return Err(CalmError::Forbidden(
            "system Area requires deployment authority".into(),
        ));
    }
    let role: Option<String> =
        sqlx::query_scalar("SELECT role FROM area_members WHERE area_id=?1 AND user_id=?2")
            .bind(area_id)
            .bind(user_id)
            .fetch_optional(connection)
            .await?;
    let role = match role.as_deref() {
        Some("owner") => AreaMemberRole::Owner,
        Some("member") => AreaMemberRole::Member,
        None => return Err(CalmError::Forbidden("Area membership required".into())),
        Some(value) => {
            return Err(CalmError::Internal(format!(
                "invalid Area member role {value}"
            )));
        }
    };
    if !role.permits(access) {
        return Err(CalmError::Forbidden("Area owner required".into()));
    }
    Ok(area)
}

pub async fn area_get(connection: &mut SqliteConnection, area_id: &str) -> Result<Area> {
    sqlx::query_as::<_, crate::db::rows::AreaRow>(
        "SELECT id, name, color, sort, kind, default_template_id, default_cwd, created_at, updated_at FROM areas WHERE id=?1",
    ).bind(area_id).fetch_optional(connection).await?.map(Area::from)
        .ok_or_else(|| CalmError::NotFound(format!("area {area_id}")))
}

pub async fn areas_list_for_member(
    connection: &mut SqliteConnection,
    user_id: &str,
) -> Result<Vec<Area>> {
    application_user_require_registered(connection, user_id).await?;
    let rows = sqlx::query_as::<_, crate::db::rows::AreaRow>(
        "SELECT a.id, a.name, a.color, a.sort, a.kind,
                a.default_template_id, a.default_cwd, a.created_at, a.updated_at
         FROM areas a JOIN area_members m ON m.area_id=a.id
         WHERE m.user_id=?1 AND a.kind='user'
         ORDER BY a.sort, a.id",
    )
    .bind(user_id)
    .fetch_all(connection)
    .await?;
    Ok(rows.into_iter().map(Area::from).collect())
}
