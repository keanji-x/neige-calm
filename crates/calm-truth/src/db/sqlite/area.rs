use sqlx::Sqlite;
use sqlx::Transaction;

use super::infra::next_sort_scoped_in_tx;
use super::session_row::{
    WorkerSessionDeleteScope, clear_track_root_session_refs_for_worker_session_delete_tx,
};
use crate::error::{CalmError, Result};
use crate::model::*;

// ---------------------------------------------------------------------------
// `_tx` helpers — composable inside `Repo::write_with_event` closures.
// ---------------------------------------------------------------------------

pub async fn area_create_tx(tx: &mut Transaction<'_, Sqlite>, p: NewArea) -> Result<Area> {
    let sort = match p.sort {
        Some(s) => s,
        None => next_sort_scoped_in_tx(tx, "areas", "", None).await?,
    };
    let now = now_ms();
    let id = new_id();
    // Issue #175: user-facing creates always land as `AreaKind::User`.
    // The `areas.kind` column was added in migration 0009 with DEFAULT
    // 'user'; we bind the variant explicitly here (mirroring the
    // `card_create_with_id_tx` pattern that binds `CardRole::Worker`)
    // so the storage shape stays self-documenting and a future kind
    // addition surfaces here as a compile error rather than silently
    // accepting the DB default. The system area is minted exclusively
    // via `area_create_system_tx` below.
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
        created_at: now,
        updated_at: now,
    })
}

/// Issue #175 — mint the singleton system area that hosts the default
/// Today terminal's track + card. The unique partial index on
/// `areas(kind) WHERE kind = 'system'` from migration 0009 enforces the
/// at-most-one invariant DB-side; the upsert endpoint
/// (`POST /api/areas/system`) checks for existence before calling this
/// helper, so a healthy production path never trips the index. We
/// don't translate a uniqueness violation into a typed conflict here
/// — if two callers race past the existence check we want the txn to
/// roll back and the loser to retry via the upsert endpoint, which
/// will re-read the now-existing row.
///
/// `name`, `color`, and `sort` are sentinel values the user never sees
/// (system areas are filtered out of `GET /api/areas`). They exist
/// because the underlying columns are `NOT NULL`; the chosen sentinels
/// (`name = 'system'`, `color = '#000'`, `sort = -1.0`) are documented
/// here so a debugger landing on a system row knows it's looking at
/// scaffolding, not user data.
pub async fn area_create_system_tx(tx: &mut Transaction<'_, Sqlite>) -> Result<Area> {
    let now = now_ms();
    let id = new_id();
    // Sort sentinel: -1.0 places the system area below any user area
    // (which start at 1.0 via `next_sort_scoped_in_tx`) if a debugger
    // ever asks for `areas ORDER BY sort`. Hidden from `GET /api/areas`
    // either way; this is just a debugger-friendly default.
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
        r#"SELECT id, name, color, sort, kind, created_at, updated_at
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
    c.updated_at = now_ms();

    // `kind` is intentionally absent from `AreaPatch` — issue #175
    // forbids re-tagging an area between user/system through the regular
    // PATCH surface. The system area is minted exactly once via
    // `area_create_system_tx` and never demoted; user areas stay user.
    sqlx::query(
        r#"UPDATE areas SET name = ?1, color = ?2, sort = ?3, updated_at = ?4
           WHERE id = ?5"#,
    )
    .bind(&c.name)
    .bind(&c.color)
    .bind(c.sort)
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
        sqlx::query("DELETE FROM track_vcs_refs WHERE track_id = ?1")
            .bind(&track_id)
            .execute(&mut **tx)
            .await?;
        sqlx::query("DELETE FROM track_vcs_commits WHERE track_id = ?1")
            .bind(&track_id)
            .execute(&mut **tx)
            .await?;
        // #644 — `tasks` has no FK to `tracks`; mirror `track_delete_tx`.
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

/// Issue #250 PR 2 — in-tx variant of
/// [`SqlxRepo::area_folder_create`](crate::db::RepoOutOfDomain::area_folder_create).
///
/// Needed because the track-create path with `attach_folder = true`
/// claims a folder and writes the track row in the **same** transaction:
/// either both land or neither does. The route layer
/// (`routes::tracks::create_track`) hands path normalization +
/// conflict-classification responsibilities here (mirror of the route
/// layer in `routes::area_folders::create_folder`), but the conflict
/// scan reuses the existing in-memory pass over `area_folders_list_all`
/// inside the same tx so a concurrent claim from another connection is
/// detected by the UNIQUE constraint at INSERT time. Returns the
/// inserted row; the caller emits whatever event/cache write it needs.
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

/// Issue #250 PR 2 — in-tx variant of `area_folders_list_all`. Used by
/// the track-create `attach_folder = true` path so the conflict scan
/// reads consistent state alongside the row insert. SQLite serializes
/// writers anyway, but routing through the same tx future-proofs the
/// path against per-connection isolation surprises.
pub async fn area_folders_list_all_tx(tx: &mut Transaction<'_, Sqlite>) -> Result<Vec<AreaFolder>> {
    let rows = sqlx::query_as::<_, crate::db::rows::AreaFolderRow>(
        r#"SELECT id, area_id, path, created_at
           FROM area_folders ORDER BY path ASC"#,
    )
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows.into_iter().map(AreaFolder::from).collect())
}
