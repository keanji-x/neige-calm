use sqlx::Sqlite;
use sqlx::Transaction;

use super::infra::next_sort_scoped_in_tx;
use super::session_row::{
    WorkerSessionDeleteScope, clear_track_root_session_refs_for_worker_session_delete_tx,
};
use crate::error::{CalmError, Result};
use crate::model::*;

pub async fn area_create_tx(tx: &mut Transaction<'_, Sqlite>, p: NewArea) -> Result<Area> {
    let sort = match p.sort {
        Some(s) => s,
        None => next_sort_scoped_in_tx(tx, "areas", "", None).await?,
    };
    let now = now_ms();
    let id = new_id();
    // User-facing creates always land as `AreaKind::User`; the system area is minted exclusively via `area_create_system_tx`.
    sqlx::query(
        r#"INSERT INTO areas (id, name, color, sort, kind, created_at, updated_at)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"#,
    )
    .bind(&id)
    .bind(&p.name)
    .bind(&p.color)
    .bind(sort)
    .bind(AreaKind::User.as_db_str())
    .bind(now)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    Ok(Area {
        id: id.into(),
        name: p.name,
        color: p.color,
        sort,
        kind: AreaKind::User,
        default_template_id: None,
        default_cwd: None,
        created_at: now,
        updated_at: now,
    })
}

/// Read a permanent creation binding inside the same immediate transaction as
/// minting. An absent/deleted Area never frees the key for a second creation.
pub async fn area_create_replay_tx(
    tx: &mut Transaction<'_, Sqlite>,
    key: &str,
    fingerprint: &str,
) -> Result<Option<Area>> {
    let binding: Option<(String, String)> = sqlx::query_as(
        "SELECT request_fingerprint, area_id FROM area_create_idempotency WHERE idempotency_key = ?1",
    )
    .bind(key)
    .fetch_optional(&mut **tx)
    .await?;
    let Some((original_fingerprint, area_id)) = binding else {
        return Ok(None);
    };
    if original_fingerprint != fingerprint {
        return Err(CalmError::Conflict(
            "This Area creation key belongs to a different request. Retry the original request or explicitly start a new Area.",
        ));
    }
    let area = sqlx::query_as::<_, crate::db::rows::AreaRow>(
        "SELECT id, name, color, sort, kind, default_template_id, default_cwd, created_at, updated_at FROM areas WHERE id = ?1",
    )
    .bind(&area_id)
    .fetch_optional(&mut **tx)
    .await?
    .map(Area::from)
    .ok_or_else(|| CalmError::Conflict(
        "The Area created by this request was deleted. Discard this draft to explicitly start a new Area.",
    ))?;
    Ok(Some(area))
}

/// Must commit with the Area and its creation event; never claim a key in a
/// separate transaction, or a lost response could leave unbound side effects.
pub async fn area_create_bind_tx(
    tx: &mut Transaction<'_, Sqlite>,
    key: &str,
    fingerprint: &str,
    area_id: &str,
) -> Result<()> {
    sqlx::query("INSERT INTO area_create_idempotency (idempotency_key, request_fingerprint, area_id) VALUES (?1, ?2, ?3)")
        .bind(key)
        .bind(fingerprint)
        .bind(area_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Mint the singleton system area. The partial unique index on `areas(kind) WHERE kind = 'system'` enforces at-most-one;
/// a uniqueness violation is deliberately not translated into a typed conflict — the loser retries via the upsert endpoint.
/// `name`/`color`/`sort` are sentinels the user never sees (the columns are `NOT NULL`).
pub async fn area_create_system_tx(tx: &mut Transaction<'_, Sqlite>) -> Result<Area> {
    let now = now_ms();
    let id = new_id();
    // -1.0 places the system area below any user area (which start at 1.0).
    let sort = -1.0_f64;
    sqlx::query(
        r#"INSERT INTO areas (id, name, color, sort, kind, created_at, updated_at)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"#,
    )
    .bind(&id)
    .bind("system")
    .bind("#000")
    .bind(sort)
    .bind(AreaKind::System.as_db_str())
    .bind(now)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    Ok(Area {
        id: id.into(),
        name: "system".into(),
        color: "#000".into(),
        sort,
        kind: AreaKind::System,
        default_template_id: None,
        default_cwd: None,
        created_at: now,
        updated_at: now,
    })
}

pub async fn area_update_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    p: AreaPatch,
) -> Result<Area> {
    let mut c = sqlx::query_as::<_, crate::db::rows::AreaRow>(
        r#"SELECT id, name, color, sort, kind, default_template_id, default_cwd,
                  created_at, updated_at
           FROM areas WHERE id = ?1"#,
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?
    .map(Area::from)
    .ok_or_else(|| CalmError::NotFound(format!("area {id}")))?;

    if let Some(v) = p.name {
        c.name = v;
    }
    if let Some(v) = p.color {
        c.color = v;
    }
    if let Some(v) = p.sort {
        c.sort = v;
    }
    if let Some(v) = p.default_template_id {
        c.default_template_id = v;
    }
    if let Some(v) = p.default_cwd {
        c.default_cwd = v;
    }
    // Area responses and `area.updated` events race on the client: make this a strict row version, not merely a
    // wall-clock sample, so an older HTTP response cannot overwrite the later event.
    let next_version = c
        .updated_at
        .checked_add(1)
        .ok_or_else(|| CalmError::Internal(format!("area {id} updated_at overflow")))?;
    c.updated_at = now_ms().max(next_version);

    // `kind` is intentionally absent from `AreaPatch`: an area is never re-tagged between user/system through PATCH.
    sqlx::query(
        r#"UPDATE areas
           SET name = ?1, color = ?2, sort = ?3, default_template_id = ?4,
               default_cwd = ?5, updated_at = ?6
           WHERE id = ?7"#,
    )
    .bind(&c.name)
    .bind(&c.color)
    .bind(c.sort)
    .bind(&c.default_template_id)
    .bind(&c.default_cwd)
    .bind(c.updated_at)
    .bind(c.id.as_str())
    .execute(&mut **tx)
    .await?;
    Ok(c)
}

pub async fn area_delete_tx(tx: &mut Transaction<'_, Sqlite>, id: &str) -> Result<()> {
    let track_ids: Vec<(String,)> = sqlx::query_as("SELECT id FROM tracks WHERE area_id = ?1")
        .bind(id)
        .fetch_all(&mut **tx)
        .await?;
    for (track_id,) in track_ids {
        super::track::track_require_candidate_verification_settled_tx(tx, &track_id).await?;
        sqlx::query("DELETE FROM track_vcs_refs WHERE track_id = ?1")
            .bind(&track_id)
            .execute(&mut **tx)
            .await?;
        sqlx::query("DELETE FROM track_vcs_commits WHERE track_id = ?1")
            .bind(&track_id)
            .execute(&mut **tx)
            .await?;
        // `tasks` has no FK to `tracks`; mirror `track_delete_tx`.
        sqlx::query(
            "DELETE FROM task_ref_index WHERE task_id IN (SELECT id FROM tasks WHERE track_id = ?1)",
        )
        .bind(&track_id)
        .execute(&mut **tx)
        .await?;
        sqlx::query("DELETE FROM tasks WHERE track_id = ?1")
            .bind(&track_id)
            .execute(&mut **tx)
            .await?;
        // Explicit domain deletion removes allocation metadata after execution rows.
        sqlx::query("DELETE FROM task_attempt_allocations WHERE track_id = ?1")
            .bind(&track_id)
            .execute(&mut **tx)
            .await?;
        clear_track_root_session_refs_for_worker_session_delete_tx(
            tx,
            WorkerSessionDeleteScope::Track {
                track_id: &track_id,
            },
        )
        .await?;
        sqlx::query("DELETE FROM worker_sessions WHERE track_id = ?1")
            .bind(&track_id)
            .execute(&mut **tx)
            .await?;
    }
    let res = sqlx::query("DELETE FROM areas WHERE id = ?1")
        .bind(id)
        .execute(&mut **tx)
        .await?;
    if res.rows_affected() == 0 {
        return Err(CalmError::NotFound(format!("area {id}")));
    }
    Ok(())
}

/// In-tx variant of `area_folder_create`: the track-create path with `attach_folder = true` claims a folder and
/// writes the track row in the **same** transaction, so either both land or neither does.
pub async fn area_folder_create_tx(
    tx: &mut Transaction<'_, Sqlite>,
    area_id: &str,
    path: &str,
) -> Result<AreaFolder> {
    let exists: Option<(String,)> = sqlx::query_as("SELECT id FROM areas WHERE id = ?1")
        .bind(area_id)
        .fetch_optional(&mut **tx)
        .await?;
    if exists.is_none() {
        return Err(CalmError::NotFound(format!("area {area_id}")));
    }
    let now = now_ms();
    let res =
        sqlx::query("INSERT INTO area_folders (area_id, path, created_at) VALUES (?1, ?2, ?3)")
            .bind(area_id)
            .bind(path)
            .bind(now)
            .execute(&mut **tx)
            .await;
    match res {
        Ok(out) => Ok(AreaFolder {
            id: out.last_insert_rowid(),
            area_id: area_id.to_string().into(),
            path: path.to_string(),
            created_at: now,
        }),
        Err(sqlx::Error::Database(dbe)) if dbe.message().contains("UNIQUE") => Err(
            CalmError::Conflict(format!("area_folders.path already claims `{path}`")),
        ),
        Err(e) => Err(e.into()),
    }
}

/// In-tx variant of `area_folders_list_all`, so the conflict scan reads consistent state alongside the row insert.
pub async fn area_folders_list_all_tx(tx: &mut Transaction<'_, Sqlite>) -> Result<Vec<AreaFolder>> {
    let rows = sqlx::query_as::<_, crate::db::rows::AreaFolderRow>(
        r#"SELECT id, area_id, path, created_at
           FROM area_folders ORDER BY path ASC"#,
    )
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows.into_iter().map(AreaFolder::from).collect())
}
