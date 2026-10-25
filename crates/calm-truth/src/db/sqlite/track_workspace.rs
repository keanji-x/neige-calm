//! The single writer of a track's workspace (kind, path, freeze stamp and track
//! worktree are one decision, always written together) and the one-way freeze latch.

use sqlx::{Sqlite, Transaction};

use crate::error::{CalmError, Result};
use crate::model::{TrackWorkspace, TrackWorkspaceKind};

/// Managed track owners whose retired workspace trash must remain recoverable.
/// The caller holds the lifecycle fence while sampling this read inventory.
pub async fn track_workspace_trash_retention_owners(
    pool: &sqlx::SqlitePool,
) -> Result<Vec<String>> {
    Ok(
        sqlx::query_scalar("SELECT id FROM tracks WHERE workspace_kind = 'managed'")
            .fetch_all(pool)
            .await?,
    )
}

/// Closed tracks with a worktree and no agent session that may be working in it: the Planner and
/// conversation agents run there too, and a closed track's idle Planner must not block the clean.
/// Tasks, leases and deliveries are [`super::checkout_occupancy`]'s to judge.
const CLOSED_TRACK_WORKTREES_SQL: &str = "SELECT t.* \
       FROM tracks t \
      WHERE t.closed_at IS NOT NULL \
        AND NOT EXISTS (SELECT 1 FROM worker_sessions ws WHERE ws.track_id = t.id \
                         AND ws.state IN ('starting','running','turn_pending')) \
      ORDER BY t.id";

/// Closed-track candidates for the workspace owner's close tick.
pub async fn closed_worktree_candidates(
    pool: &sqlx::SqlitePool,
) -> Result<Vec<super::super::rows::TrackRow>> {
    Ok(sqlx::query_as(CLOSED_TRACK_WORKTREES_SQL)
        .fetch_all(pool)
        .await?)
}

/// Re-read the same eligibility under the caller's lifecycle fence.
pub async fn closed_worktree_candidate(
    pool: &sqlx::SqlitePool,
    track: &str,
) -> Result<Option<super::super::rows::TrackRow>> {
    Ok(sqlx::query_as(&format!(
        "SELECT * FROM ({CLOSED_TRACK_WORKTREES_SQL}) WHERE id=?"
    ))
    .bind(track)
    .fetch_optional(pool)
    .await?)
}

/// Write a track's workspace — kind, path, freeze stamp and track worktree — in one statement.
/// The freeze latch is enforced here, at the bottom of every workspace write.
/// A frozen row is `Conflict`, told apart from `NotFound` by a second read.
pub async fn track_workspace_write_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track_id: &str,
    workspace: &TrackWorkspace,
) -> Result<()> {
    let res = sqlx::query(
        r#"UPDATE tracks
           SET workspace_path = ?1, workspace_kind = ?2, workspace_frozen_at = ?3,
               workspace_worktree_path = ?4
           WHERE id = ?5 AND workspace_frozen_at IS NULL"#,
    )
    .bind(&workspace.path)
    .bind(workspace.kind.as_db_str())
    .bind(workspace.frozen_at)
    .bind(workspace.worktree.as_deref())
    .bind(track_id)
    .execute(&mut **tx)
    .await?;
    if res.rows_affected() == 0 {
        // Zero rows means "no such track" or "the latch is closed"; the row is under
        // this transaction's writer lock, so a second read cannot change underneath.
        let frozen_at: Option<Option<i64>> =
            sqlx::query_scalar("SELECT workspace_frozen_at FROM tracks WHERE id = ?1")
                .bind(track_id)
                .fetch_optional(&mut **tx)
                .await?;
        return match frozen_at {
            None => Err(CalmError::NotFound(format!("track {track_id}"))),
            Some(None) => Err(CalmError::Internal(format!(
                "track {track_id} workspace write affected no rows while unfrozen"
            ))),
            Some(Some(at)) => Err(CalmError::Conflict(format!(
                "track {track_id} workspace was frozen at {at} and can no longer be changed"
            ))),
        };
    }
    super::workspace_resource::workspace_bind_selection_tx(tx, track_id, workspace).await?;
    Ok(())
}

/// Every guard that decides whether a workspace may move re-reads through this
/// inside the `BEGIN IMMEDIATE`; the route's unlocked read is never the authority.
pub async fn track_workspace_read_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track_id: &str,
) -> Result<TrackWorkspace> {
    let row: Option<(String, String, Option<i64>, Option<String>)> = sqlx::query_as(
        "SELECT workspace_kind, workspace_path, workspace_frozen_at, workspace_worktree_path \
         FROM tracks WHERE id = ?1",
    )
    .bind(track_id)
    .fetch_optional(&mut **tx)
    .await?;
    let (kind, path, frozen_at, worktree) =
        row.ok_or_else(|| CalmError::NotFound(format!("track {track_id}")))?;
    Ok(TrackWorkspace {
        kind: TrackWorkspaceKind::try_from(kind).map_err(CalmError::Internal)?,
        path,
        frozen_at,
        worktree,
    })
}

/// Close the latch: `kind` and `path` become permanent. Idempotent and
/// monotonic; a caller cannot un-freeze because this cannot write `NULL`.
/// A no-op for the system area's launchpad track, whose path the kernel
/// re-points on every `ensure`. Returns `true` when this call closed the latch.
pub async fn track_workspace_freeze_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track_id: &str,
    now_ms: i64,
) -> Result<bool> {
    let res = sqlx::query(
        r#"UPDATE tracks
           SET workspace_frozen_at = ?1
           WHERE id = ?2
             AND workspace_frozen_at IS NULL
             AND (SELECT c.kind FROM areas AS c WHERE c.id = tracks.area_id) <> 'system'"#,
    )
    .bind(now_ms)
    .bind(track_id)
    .execute(&mut **tx)
    .await?;
    if res.rows_affected() > 0 {
        sqlx::query(
            "UPDATE track_workspace_bindings SET frozen_at_ms = ?1 \
             WHERE track_id = ?2 AND frozen_at_ms IS NULL",
        )
        .bind(now_ms)
        .bind(track_id)
        .execute(&mut **tx)
        .await?;
    }
    Ok(res.rows_affected() > 0)
}
