use sqlx::Sqlite;
use sqlx::Transaction;

use super::infra::next_sort_scoped_in_tx;
use super::session_row::{
    WorkerSessionDeleteScope, clear_wave_root_session_refs_for_worker_session_delete_tx,
};
use crate::error::{CalmError, Result};
use crate::model::*;

// ---------------------------------------------------------------------------
// `_tx` helpers — composable inside `Repo::write_with_event` closures.
// ---------------------------------------------------------------------------

pub async fn cove_create_tx(tx: &mut Transaction<'_, Sqlite>, p: NewCove) -> Result<Cove> {
    let sort = match p.sort {
        Some(s) => s,
        None => next_sort_scoped_in_tx(tx, "coves", "", None).await?,
    };
    let now = now_ms();
    let id = new_id();
    // Issue #175: user-facing creates always land as `CoveKind::User`.
    // The `coves.kind` column was added in migration 0009 with DEFAULT
    // 'user'; we bind the variant explicitly here (mirroring the
    // `card_create_with_id_tx` pattern that binds `CardRole::Worker`)
    // so the storage shape stays self-documenting and a future kind
    // addition surfaces here as a compile error rather than silently
    // accepting the DB default. The system cove is minted exclusively
    // via `cove_create_system_tx` below.
    sqlx::query(
        r#"INSERT INTO coves (id, name, color, sort, kind, created_at, updated_at)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"#,
    )
    .bind(&id)
    .bind(&p.name)
    .bind(&p.color)
    .bind(sort)
    .bind(CoveKind::User.as_db_str())
    .bind(now)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    Ok(Cove {
        id: id.into(),
        name: p.name,
        color: p.color,
        sort,
        kind: CoveKind::User,
        created_at: now,
        updated_at: now,
    })
}

/// Issue #175 — mint the singleton system cove that hosts the default
/// Today terminal's wave + card. The unique partial index on
/// `coves(kind) WHERE kind = 'system'` from migration 0009 enforces the
/// at-most-one invariant DB-side; the upsert endpoint
/// (`POST /api/coves/system`) checks for existence before calling this
/// helper, so a healthy production path never trips the index. We
/// don't translate a uniqueness violation into a typed conflict here
/// — if two callers race past the existence check we want the txn to
/// roll back and the loser to retry via the upsert endpoint, which
/// will re-read the now-existing row.
///
/// `name`, `color`, and `sort` are sentinel values the user never sees
/// (system coves are filtered out of `GET /api/coves`). They exist
/// because the underlying columns are `NOT NULL`; the chosen sentinels
/// (`name = 'system'`, `color = '#000'`, `sort = -1.0`) are documented
/// here so a debugger landing on a system row knows it's looking at
/// scaffolding, not user data.
pub async fn cove_create_system_tx(tx: &mut Transaction<'_, Sqlite>) -> Result<Cove> {
    let now = now_ms();
    let id = new_id();
    // Sort sentinel: -1.0 places the system cove below any user cove
    // (which start at 1.0 via `next_sort_scoped_in_tx`) if a debugger
    // ever asks for `coves ORDER BY sort`. Hidden from `GET /api/coves`
    // either way; this is just a debugger-friendly default.
    let sort = -1.0_f64;
    sqlx::query(
        r#"INSERT INTO coves (id, name, color, sort, kind, created_at, updated_at)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"#,
    )
    .bind(&id)
    .bind("system")
    .bind("#000")
    .bind(sort)
    .bind(CoveKind::System.as_db_str())
    .bind(now)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    Ok(Cove {
        id: id.into(),
        name: "system".into(),
        color: "#000".into(),
        sort,
        kind: CoveKind::System,
        created_at: now,
        updated_at: now,
    })
}

pub async fn cove_update_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    p: CovePatch,
) -> Result<Cove> {
    let mut c = sqlx::query_as::<_, crate::db::rows::CoveRow>(
        r#"SELECT id, name, color, sort, kind, created_at, updated_at
           FROM coves WHERE id = ?1"#,
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?
    .map(Cove::from)
    .ok_or_else(|| CalmError::NotFound(format!("cove {id}")))?;

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

    // `kind` is intentionally absent from `CovePatch` — issue #175
    // forbids re-tagging a cove between user/system through the regular
    // PATCH surface. The system cove is minted exactly once via
    // `cove_create_system_tx` and never demoted; user coves stay user.
    sqlx::query(
        r#"UPDATE coves SET name = ?1, color = ?2, sort = ?3, updated_at = ?4
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

pub async fn cove_delete_tx(tx: &mut Transaction<'_, Sqlite>, id: &str) -> Result<()> {
    let wave_ids: Vec<(String,)> = sqlx::query_as("SELECT id FROM waves WHERE cove_id = ?1")
        .bind(id)
        .fetch_all(&mut **tx)
        .await?;
    for (wave_id,) in wave_ids {
        sqlx::query("DELETE FROM wave_vcs_refs WHERE wave_id = ?1")
            .bind(&wave_id)
            .execute(&mut **tx)
            .await?;
        sqlx::query("DELETE FROM wave_vcs_commits WHERE wave_id = ?1")
            .bind(&wave_id)
            .execute(&mut **tx)
            .await?;
        // #644 — `tasks` has no FK to `waves`; mirror `wave_delete_tx`.
        sqlx::query("DELETE FROM tasks WHERE wave_id = ?1")
            .bind(&wave_id)
            .execute(&mut **tx)
            .await?;
        clear_wave_root_session_refs_for_worker_session_delete_tx(
            tx,
            WorkerSessionDeleteScope::Wave { wave_id: &wave_id },
        )
        .await?;
        sqlx::query("DELETE FROM worker_sessions WHERE wave_id = ?1")
            .bind(&wave_id)
            .execute(&mut **tx)
            .await?;
    }
    let res = sqlx::query("DELETE FROM coves WHERE id = ?1")
        .bind(id)
        .execute(&mut **tx)
        .await?;
    if res.rows_affected() == 0 {
        return Err(CalmError::NotFound(format!("cove {id}")));
    }
    Ok(())
}

/// Issue #250 PR 2 — in-tx variant of
/// [`SqlxRepo::cove_folder_create`](crate::db::RepoOutOfDomain::cove_folder_create).
///
/// Needed because the wave-create path with `attach_folder = true`
/// claims a folder and writes the wave row in the **same** transaction:
/// either both land or neither does. The route layer
/// (`routes::waves::create_wave`) hands path normalization +
/// conflict-classification responsibilities here (mirror of the route
/// layer in `routes::cove_folders::create_folder`), but the conflict
/// scan reuses the existing in-memory pass over `cove_folders_list_all`
/// inside the same tx so a concurrent claim from another connection is
/// detected by the UNIQUE constraint at INSERT time. Returns the
/// inserted row; the caller emits whatever event/cache write it needs.
pub async fn cove_folder_create_tx(
    tx: &mut Transaction<'_, Sqlite>,
    cove_id: &str,
    path: &str,
) -> Result<CoveFolder> {
    let exists: Option<(String,)> = sqlx::query_as("SELECT id FROM coves WHERE id = ?1")
        .bind(cove_id)
        .fetch_optional(&mut **tx)
        .await?;
    if exists.is_none() {
        return Err(CalmError::NotFound(format!("cove {cove_id}")));
    }
    let now = now_ms();
    let res =
        sqlx::query("INSERT INTO cove_folders (cove_id, path, created_at) VALUES (?1, ?2, ?3)")
            .bind(cove_id)
            .bind(path)
            .bind(now)
            .execute(&mut **tx)
            .await;
    match res {
        Ok(out) => Ok(CoveFolder {
            id: out.last_insert_rowid(),
            cove_id: cove_id.to_string().into(),
            path: path.to_string(),
            created_at: now,
        }),
        Err(sqlx::Error::Database(dbe)) if dbe.message().contains("UNIQUE") => Err(
            CalmError::Conflict(format!("cove_folders.path already claims `{path}`")),
        ),
        Err(e) => Err(e.into()),
    }
}

/// Issue #250 PR 2 — in-tx variant of `cove_folders_list_all`. Used by
/// the wave-create `attach_folder = true` path so the conflict scan
/// reads consistent state alongside the row insert. SQLite serializes
/// writers anyway, but routing through the same tx future-proofs the
/// path against per-connection isolation surprises.
pub async fn cove_folders_list_all_tx(tx: &mut Transaction<'_, Sqlite>) -> Result<Vec<CoveFolder>> {
    let rows = sqlx::query_as::<_, crate::db::rows::CoveFolderRow>(
        r#"SELECT id, cove_id, path, created_at
           FROM cove_folders ORDER BY path ASC"#,
    )
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows.into_iter().map(CoveFolder::from).collect())
}
