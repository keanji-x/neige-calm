//! Durable retirement of logical workspace selections. These records reserve paths;
//! neither a selection id nor an observation grants authority to delete a directory.
use sqlx::{Sqlite, SqliteConnection, Transaction};
use std::path::{Path, PathBuf};

use super::path_location::PathLocation;
use crate::{
    error::{CalmError, Result},
    model::{new_id, now_ms},
};

pub const ISOLATION_UNAVAILABLE: &str = "isolation_unavailable";

/// Deferred cleanup inventory, independent of live Track rows and current bindings.
#[derive(Debug, sqlx::FromRow)]
pub struct RetiredWorkspace {
    pub id: String,
    pub source_track_id: String,
    pub root_path: String,
    pub observed_root: Option<String>,
    pub retirement_root: String,
    pub retired_at_ms: i64,
    pub cleanup_reason: String,
}

pub async fn retired_workspaces(conn: &mut SqliteConnection) -> Result<Vec<RetiredWorkspace>> {
    Ok(sqlx::query_as(
        "SELECT id,source_track_id,root_path,observed_root,retirement_root,retired_at_ms,cleanup_reason \
         FROM workspace_resources WHERE retired_at_ms IS NOT NULL ORDER BY retired_at_ms,id",
    )
    .fetch_all(conn)
    .await?)
}

/// Reject using a retired root or its descendants. Different cwd/Track values
/// are not an access sandbox; this prevents supported resource reassignment only.
pub async fn require_active_workspace_path(conn: &mut SqliteConnection, path: &Path) -> Result<()> {
    active_workspace_location(conn, path).await?;
    Ok(())
}

pub(super) async fn active_workspace_location<'a>(
    conn: &mut SqliteConnection,
    path: &'a Path,
) -> Result<PathLocation<'a>> {
    let candidate = PathLocation::allow_missing(path)?;
    let mut roots = retired_location_roots(conn).await?;
    roots.extend(super::workspace_locations::deferred_reclaim_locations(conn).await?);
    for (owner, root) in roots {
        if candidate.overlaps_reserved(&root, false) {
            return Err(retired_refusal(&owner));
        }
    }
    Ok(candidate)
}

fn retired_refusal(owner: &str) -> CalmError {
    CalmError::BadRequest(format!(
        "workspace {owner} is retired; physical cleanup is deferred ({ISOLATION_UNAVAILABLE})"
    ))
}

/// Must run before replacing/removing the binding. Borrowed directories are not ours to retire.
pub async fn retire_workspace_selection_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track: &str,
) -> Result<()> {
    let binding: Option<(String, String, String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT r.id,r.ownership,r.location_root,r.observed_root,r.retirement_root FROM track_workspace_bindings b \
         JOIN workspace_resources r ON r.id=b.workspace_id JOIN tracks t ON t.id=b.track_id \
         WHERE b.track_id=?1 AND r.source_track_id=t.id AND r.root_path=b.selected_path \
         AND r.ownership=b.ownership AND b.selected_path=coalesce(t.workspace_worktree_path,t.workspace_path)",
    ).bind(track).fetch_optional(&mut **tx).await?;
    let (id, ownership, path, observed, reserved) =
        binding.ok_or_else(|| CalmError::Conflict("workspace selection missing at retirement"))?;
    if ownership == "owned" {
        if reserved.is_some() {
            return Ok(());
        }
        let (root, reason) = match observed {
            Some(root) => (root, ISOLATION_UNAVAILABLE),
            None => (path, "unknown_location"),
        };
        sqlx::query(
            "UPDATE workspace_resources SET retired_at_ms=coalesce(retired_at_ms,?2),cleanup_reason=?3,retirement_root=coalesce(retirement_root,?4) WHERE id=?1",
        ).bind(id).bind(now_ms()).bind(reason).bind(root).execute(&mut **tx).await?;
    }

    Ok(())
}

/// Extra worktree identity comes from Neige's provision receipt, never a path lookup.
/// Repeated close/delete/recovery registration converges on the same reservation.
pub async fn retire_extra_worktree_tx(
    tx: &mut Transaction<'_, Sqlite>,
    provision: i64,
    track: &str,
    path: &Path,
) -> Result<bool> {
    let Some(current) = super::worktree_events::current_provision(tx, provision).await? else {
        return Ok(false);
    };
    if current.track_id != track || Path::new(&current.path) != path {
        return Err(CalmError::Conflict("workspace provision identity changed"));
    }
    // An extra receipt must not retire the primary retained for reopen, nor
    // reserve an ancestor containing it. Compare recorded locations only; close
    // must not depend on the health of the old directory.
    let primary: Option<(String, String, Option<String>)> = sqlx::query_as(
        "SELECT b.selected_path,r.location_root,r.observed_root FROM track_workspace_bindings b \
         JOIN workspace_resources r ON r.id=b.workspace_id WHERE b.track_id=?",
    )
    .bind(track)
    .fetch_optional(&mut **tx)
    .await?;
    if let Some((selected, location, observed)) = primary
        && [Some(selected), Some(location), observed]
            .into_iter()
            .flatten()
            .any(|primary| Path::new(&primary).starts_with(path))
    {
        return Ok(false);
    }
    let key = format!("provision:{provision}");
    let stored: Option<(String, String)> = sqlx::query_as(
        "SELECT source_track_id,root_path FROM workspace_resources WHERE retirement_key=?",
    )
    .bind(&key)
    .fetch_optional(&mut **tx)
    .await?;
    if let Some((owner, stored_path)) = stored {
        if owner != track || Path::new(&stored_path) != path {
            return Err(CalmError::Conflict("workspace retirement identity changed"));
        }
        return Ok(true);
    }
    // Receipts do not attest an observed physical identity. Fresh Forge
    // targets are canonicalized before execution, but historical aliases cannot
    // be reconstructed safely from the receipt. Keep them explicitly unlocated.
    let reserved_root = path.to_string_lossy().into_owned();
    let reason = "unknown_location";
    let path = path
        .to_str()
        .ok_or_else(|| CalmError::BadRequest("workspace path is not UTF-8"))?;
    sqlx::query(
        "INSERT INTO workspace_resources \
         (id,source_track_id,root_path,ownership,observation_version,created_at_ms, \
          retirement_key,retired_at_ms,cleanup_reason,retirement_root,location_root) VALUES (?1,?2,?3,'owned',0,?4,?5,?4,?6,?7,?7) \
         ON CONFLICT(retirement_key) DO NOTHING",
    )
    .bind(new_id())
    .bind(track)
    .bind(path)
    .bind(now_ms())
    .bind(&key)
    .bind(reason)
    .bind(reserved_root)
    .execute(&mut **tx)
    .await?;
    let stored: (String, String) = sqlx::query_as(
        "SELECT source_track_id,root_path FROM workspace_resources WHERE retirement_key=?",
    )
    .bind(&key)
    .fetch_one(&mut **tx)
    .await?;
    if stored != (track.to_owned(), path.to_owned()) {
        return Err(CalmError::Conflict("workspace retirement identity changed"));
    }
    Ok(true)
}

pub(super) async fn retired_location_roots(
    conn: &mut SqliteConnection,
) -> Result<Vec<(String, PathBuf)>> {
    Ok(retired_workspaces(conn)
        .await?
        .into_iter()
        .flat_map(|r| {
            let mut roots = vec![
                (r.source_track_id.clone(), PathBuf::from(r.root_path)),
                (r.source_track_id.clone(), PathBuf::from(r.retirement_root)),
            ];
            if let Some(root) = r.observed_root {
                roots.push((r.source_track_id, root.into()));
            }
            roots
        })
        .collect())
}

/// Closing selects extra-worktree retirement in the same transaction for every
/// close writer. Primary selection stays available for reopen.
pub async fn retire_track_extras_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track: &str,
) -> Result<Vec<PathBuf>> {
    let claims = super::worktree_events::provisioned_for_track(tx, track).await?;
    let mut paths = Vec::new();
    for claim in claims {
        let path = PathBuf::from(claim.path);
        if retire_extra_worktree_tx(tx, claim.event_id, track, &path).await? {
            paths.push(path);
        }
    }
    Ok(paths)
}
