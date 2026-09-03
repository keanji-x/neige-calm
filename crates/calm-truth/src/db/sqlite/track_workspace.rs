//! Issue #1147 S1/S3 — the single writer of a track's workspace, and the freeze
//! latch that makes it stop being writable.
//!
//! `track_workspace_write_tx` is the only function that writes
//! `tracks.workspace_kind` / `workspace_path` / `workspace_frozen_at` as a
//! whole value. That is a plain organizational fact rather than an invariant
//! needing enforcement: after migration 0077 there is exactly one stored copy
//! of the path, so there is nothing for a second writer to disagree with. The
//! earlier draft kept `tracks.cwd` as a duplicate and tried to police the pair
//! with a source-text scanner; three rounds of red-teaming defeated three
//! scanners, and the column was deleted instead.
//!
//! A whole-value write (rather than a patch) is still the right shape: kind,
//! path and freeze stamp describe one decision and are always decided together.
//!
//! S3 adds the second half — [`track_workspace_freeze_tx`], the one-way latch —
//! and enforces it inside [`track_workspace_write_tx`].

use sqlx::{Sqlite, Transaction};

use crate::error::{CalmError, Result};
use crate::model::{TrackWorkspace, TrackWorkspaceKind};

/// Write a track's workspace — kind, path and freeze stamp — in one statement.
///
/// # The freeze latch is enforced here, not at the call sites
///
/// S1 deliberately left this unguarded: the only track S1 could re-point was
/// the kernel-owned launchpad, which is never frozen, so a guard would have had
/// no legal caller to reject. S3 introduces the caller that *can* trip it
/// (`PATCH /api/tracks/{id}`), so the latch moves in here rather than into the
/// route — this is the bottom of every workspace write, and design
/// §更换与冻结 requires the freeze to live at the real low-level write entry
/// rather than depend on an enumeration of call sites.
///
/// A frozen row is refused with `Conflict`, distinguished from "no such track"
/// (`NotFound`) by a second read: `rows_affected == 0` alone cannot tell those
/// apart, and returning `NotFound` for a frozen track would make the PATCH route
/// answer 404 for a track the caller can plainly see.
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
        // Zero rows means either "no such track" or "the latch is closed".
        // Read once more to say which; the row is already under this
        // transaction's writer lock, so the answer cannot change underneath.
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

/// Read a track's workspace inside the transaction that is about to act on it.
///
/// The PATCH route reads the track row once outside the transaction to answer
/// 404 and to scope the event, but every guard that decides whether a workspace
/// may move re-reads through this function inside the `BEGIN IMMEDIATE` — the
/// unlocked read is a convenience, never the authority.
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

/// #1147 S3 — close the latch: after this the track's `kind` and `path` are
/// permanent.
///
/// Idempotent and monotonic by construction. `WHERE workspace_frozen_at IS
/// NULL` means a second call never moves an existing stamp, so "frozen at the
/// moment the first durable cwd consumer appeared" survives every later
/// consumer; and a caller cannot accidentally un-freeze, because this function
/// has no way to write `NULL`.
///
/// # Why it is a no-op in the system area
///
/// Design §数据模型: `frozen_at` is monotonic **except for the system area's
/// Today/launchpad track**, whose path the kernel maintains
/// (`today_launchpad_ensure_tx` re-points it on every `ensure`). That track also
/// owns a terminal card and takes worker leases, i.e. it hits two of the freeze
/// points below on an ordinary boot. Freezing it there would make the very next
/// `ensure` fail the latch in [`track_workspace_write_tx`] and take the Today
/// panel down permanently.
///
/// The exclusion is a single SQL clause rather than a caller-side `if`, for the
/// same reason the latch itself lives here: the freeze points are low-level
/// write entries in three different modules, and an exception replicated across
/// three call sites is an exception that will be forgotten at the fourth.
///
/// Returns `true` when this call is the one that closed the latch.
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
