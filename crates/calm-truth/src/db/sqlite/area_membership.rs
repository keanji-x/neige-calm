//! Authoritative registry and membership policy. Connections can be a protected
//! write transaction or a read snapshot; callers do not cache membership grants.
use sqlx::{FromRow, Row, SqliteConnection, SqlitePool};

use crate::error::{CalmError, Result};
use crate::model::{Area, AreaKind, now_ms};
use calm_types::organization::{AreaAccess, AreaMemberRole};

/// A single autocommit snapshot of registration, the actual Track parent and
/// that user's membership. Never select or decode the Track's opaque fields.
pub async fn track_area_membership(
    pool: &SqlitePool,
    track_id: &str,
    user_id: &str,
) -> Result<Option<crate::db::TrackAreaMembership>> {
    let row = sqlx::query(
        "SELECT EXISTS(SELECT 1 FROM application_users WHERE id=?2) AS registered,
                t.id AS track_id, a.id, a.name, a.color, a.sort, a.kind,
                a.default_template_id, a.default_cwd, a.created_at, a.updated_at,
                m.role AS member_role
         FROM (SELECT 1)
         LEFT JOIN tracks t ON t.id=?1
         LEFT JOIN areas a ON a.id=t.area_id
         LEFT JOIN area_members m ON m.area_id=a.id AND m.user_id=?2",
    )
    .bind(track_id)
    .bind(user_id)
    .fetch_one(pool)
    .await?;
    if !row.try_get::<bool, _>("registered")? {
        return Err(CalmError::Forbidden(
            "application user is not registered".into(),
        ));
    }
    if row.try_get::<Option<String>, _>("track_id")?.is_none() {
        return Ok(None);
    }
    if row.try_get::<Option<String>, _>("id")?.is_none() {
        return Err(CalmError::Internal("Track parent Area is missing".into()));
    }
    Ok(Some(crate::db::TrackAreaMembership {
        area: crate::db::rows::AreaRow::from_row(&row)?.into(),
        member_role: decode_member_role(row.try_get("member_role")?)?,
    }))
}

fn decode_member_role(role: Option<String>) -> Result<Option<AreaMemberRole>> {
    match role.as_deref() {
        Some("owner") => Ok(Some(AreaMemberRole::Owner)),
        Some("member") => Ok(Some(AreaMemberRole::Member)),
        None => Ok(None),
        Some(value) => Err(CalmError::Internal(format!(
            "invalid Area member role {value}"
        ))),
    }
}

/// UserArea membership policy, shared by transactional and snapshot readers.
/// Registration and actual Area scope are checked by the owning factual read.
pub fn require_member_access(role: Option<AreaMemberRole>, access: AreaAccess) -> Result<()> {
    let role = role.ok_or_else(|| CalmError::Forbidden("Area membership required".into()))?;
    if !role.permits(access) {
        return Err(CalmError::Forbidden("Area owner required".into()));
    }
    Ok(())
}

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
    require_member_access(decode_member_role(role)?, access)?;
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
