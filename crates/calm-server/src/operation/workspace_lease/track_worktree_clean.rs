//! #2356 S2 — a closed track's worktree drops its ignored files. The worktree itself stays until
//! track or area delete (`super::track_worktree::remove_track_worktree`), since a track can be
//! reopened and its branch and uncommitted work belong to it; but build output (a Rust `target`
//! is about 9G), `node_modules` and the like are regenerable and would otherwise stay on disk for
//! every closed track. Time-driven and read-only on the database, so every close writer (route,
//! Planner tool, kernel) is covered without a hook in each.

use std::collections::HashSet;
use std::time::Duration;

use sqlx::SqlitePool;

use crate::db::sqlite::{CheckoutOccupancy, checkout_occupancy};
use crate::error::{CalmError, Result};
use crate::workspace_materialize::isolated_git_command;

use super::track_worktree::track_worktree_target;
use super::{
    GitWorktreeRegistration, base, git_repo_available, git_worktree_registration, metadata_lock,
};

const CLEAN_INTERVAL: Duration = Duration::from_secs(10 * 60);

/// Closed tracks with a worktree and no agent session that may be working in it: the Planner and
/// conversation agents run there too, and a closed track's idle Planner must not block the clean.
/// Tasks, leases and deliveries are [`checkout_occupancy`]'s to judge.
pub(crate) const CLOSED_TRACK_WORKTREES_SQL: &str = "SELECT t.id, t.workspace_worktree_path, t.closed_at \
       FROM tracks t \
      WHERE t.closed_at IS NOT NULL AND t.workspace_worktree_path IS NOT NULL \
        AND NOT EXISTS (SELECT 1 FROM worker_sessions ws WHERE ws.track_id = t.id \
                         AND ws.state IN ('starting','running','turn_pending')) \
      ORDER BY t.id";

/// Run [`clean_idle_closed_track_worktrees`] every [`CLEAN_INTERVAL`]. Each close is cleaned once
/// per process, so someone working by hand in a closed track's worktree loses its build output
/// once, not every tick.
pub(crate) fn spawn(pool: SqlitePool) {
    tokio::spawn(async move {
        let mut cleaned = HashSet::new();
        let mut tick = tokio::time::interval(CLEAN_INTERVAL);
        // Skip the immediate boot tick so the server settles first.
        tick.tick().await;
        loop {
            tick.tick().await;
            if let Err(e) = clean_idle_closed_track_worktrees(&pool, &mut cleaned).await {
                tracing::warn!(error = %e, "track worktree clean failed");
            }
        }
    });
}

/// One pass: `git clean -fdX` in each closed track's worktree that nothing occupies and that is not
/// yet in `cleaned`. `cleaned` holds `(track_id, closed_at)`, so a reopened and closed-again track
/// is cleaned again. Returns how many worktrees it cleaned. A worktree that fails is logged and
/// retried next pass.
pub(crate) async fn clean_idle_closed_track_worktrees(
    pool: &SqlitePool,
    cleaned: &mut HashSet<(String, i64)>,
) -> Result<usize> {
    let rows: Vec<(String, String, i64)> = sqlx::query_as(CLOSED_TRACK_WORKTREES_SQL)
        .fetch_all(pool)
        .await?;
    let mut count = 0;
    for (track_id, worktree, closed_at) in rows {
        let key = (track_id, closed_at);
        if cleaned.contains(&key)
            || checkout_occupancy(&mut *pool.acquire().await?, &key.0, "").await?
                != CheckoutOccupancy::Free
        {
            continue;
        }
        let track_id = key.0.clone();
        let result = tokio::task::spawn_blocking(move || clean_ignored(&track_id, &worktree))
            .await
            .map_err(|error| CalmError::Internal(format!("track worktree clean task: {error}")))?;
        match result {
            Ok(true) => count += 1,
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(track_id = %key.0, error = %error, "track worktree clean failed");
                continue;
            }
        }
        cleaned.insert(key);
    }
    Ok(count)
}

/// Remove the ignored files under the worktree; tracked and untracked-unignored files stay. A
/// single `-f`: git keeps an ignored nested repository (someone's worktree inside this one).
/// `Ok(false)` when there is nothing of ours to clean. Under the metadata lock, so a track delete's
/// `worktree remove` is not half done; only a registered, non-symlink worktree is cleaned, and
/// `GIT_CEILING_DIRECTORIES` stops git from finding the main checkout when the worktree's `.git`
/// is missing. A track reopened after the read loses its build output, which is regenerable.
fn clean_ignored(track_id: &str, worktree: &str) -> Result<bool> {
    let target = track_worktree_target(track_id, worktree)?;
    if !git_repo_available(&target.repo_root) {
        return Ok(false);
    }
    let _metadata_lock = metadata_lock::GitMetadataLock::acquire(&target.repo_root)?;
    if base::is_symlink_leaf(&target.path)? || !target.path.is_dir() {
        return Ok(false);
    }
    if git_worktree_registration(&target)? != GitWorktreeRegistration::Present {
        tracing::warn!(
            track_id,
            worktree,
            "track worktree clean skipped: the path is not this track's registered worktree"
        );
        return Ok(false);
    }
    let ceiling = target.path.parent().unwrap_or(&target.path);
    let output = isolated_git_command()
        .env("GIT_CEILING_DIRECTORIES", ceiling)
        .args(["-c", "core.fsmonitor=false", "-C"])
        .arg(&target.path)
        .args(["clean", "-fdXq"])
        .output()
        .map_err(|error| {
            CalmError::Internal(format!(
                "spawn git clean for {}: {error}",
                target.path.display()
            ))
        })?;
    if !output.status.success() {
        return Err(super::git_failed("git clean", &target.path, &output));
    }
    Ok(true)
}
