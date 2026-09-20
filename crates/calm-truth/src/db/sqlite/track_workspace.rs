//! The single writer of a track's workspace (kind, path and freeze stamp are
//! one decision, always written together) and the one-way freeze latch.

use sqlx::{Sqlite, Transaction};

use crate::error::{CalmError, Result};
use crate::model::{TrackWorkspace, TrackWorkspaceKind};

/// Write a track's workspace — kind, path and freeze stamp — in one statement.
/// The freeze latch is enforced here, at the bottom of every workspace write.
/// A frozen row is `Conflict`, told apart from `NotFound` by a second read.
pub async fn track_workspace_write_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track_id: &str,
    workspace: &TrackWorkspace,
) -> Result<()> {
    let res = sqlx::query(
        r#"UPDATE tracks
           SET workspace_path = ?1, workspace_kind = ?2, workspace_frozen_at = ?3
           WHERE id = ?4 AND workspace_frozen_at IS NULL"#,
    )
    .bind(&workspace.path)
    .bind(workspace.kind.as_db_str())
    .bind(workspace.frozen_at)
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
    Ok(())
}

/// Every guard that decides whether a workspace may move re-reads through this
/// inside the `BEGIN IMMEDIATE`; the route's unlocked read is never the authority.
pub async fn track_workspace_read_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track_id: &str,
) -> Result<TrackWorkspace> {
    let row: Option<(String, String, Option<i64>)> = sqlx::query_as(
        "SELECT workspace_kind, workspace_path, workspace_frozen_at FROM tracks WHERE id = ?1",
    )
    .bind(track_id)
    .fetch_optional(&mut **tx)
    .await?;
    let (kind, path, frozen_at) =
        row.ok_or_else(|| CalmError::NotFound(format!("track {track_id}")))?;
    Ok(TrackWorkspace {
        kind: TrackWorkspaceKind::try_from(kind).map_err(CalmError::Internal)?,
        path,
        frozen_at,
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
    Ok(res.rows_affected() > 0)
}
