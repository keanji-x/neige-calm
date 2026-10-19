//! #2356 S2 — a closed track's worktree drops its ignored files. The worktree itself stays until
//! track or area delete (`crate::track_worktree::remove_track_worktree`), since a track can be
//! reopened and its branch and uncommitted work belong to it; but build output (a Rust `target`
//! is about 9G), `node_modules` and the like are regenerable and would otherwise stay on disk for
//! every closed track. Time-driven and read-only on the database, so every close writer (route,
//! Planner tool, kernel) is covered without a hook in each.

use std::collections::HashSet;
use std::time::Duration;

use async_trait::async_trait;
use calm_types::model::{TrackWorkspace, WorkspaceIsolation};

use crate::error::WorkspaceError;
use crate::track_worktree::ObservePoint;

/// A closed track row, retaining workspace ownership for the runtime's clean policy.
pub struct ClosedWorktree {
    pub track_id: String,
    pub workspace: TrackWorkspace,
    pub closed_at: i64,
}

#[async_trait]
pub trait ClosedWorktreeFacts: Send + Sync + 'static {
    type Error: std::fmt::Display + Send + 'static;

    async fn closed_idle_worktrees(&self) -> Result<Vec<ClosedWorktree>, Self::Error>;
    /// Claim the existing lifecycle fence and recheck the sampled close and occupancy.
    /// Return only the track guard: the global drive guard must end before filesystem I/O.
    async fn fence(
        &self,
        track: &str,
        closed_at: i64,
    ) -> Result<Option<Box<dyn Send>>, Self::Error>;
}

const CLEAN_INTERVAL: Duration = Duration::from_secs(10 * 60);

/// Run [`clean_idle_closed_track_worktrees`] every [`CLEAN_INTERVAL`]. Each close is cleaned once
/// per process, so someone working by hand in a closed track's worktree loses its build output
/// once, not every tick.
pub fn spawn<P: ClosedWorktreeFacts>(facts: P, observe: ObservePoint)
where
    P::Error: From<WorkspaceError>,
{
    tokio::spawn(async move {
        let mut cleaned = HashSet::new();
        let mut tick = tokio::time::interval(CLEAN_INTERVAL);
        // Skip the immediate boot tick so the server settles first.
        tick.tick().await;
        loop {
            tick.tick().await;
            if let Err(e) = clean_idle_closed_track_worktrees(&facts, &mut cleaned, observe).await {
                tracing::warn!(error = %e, "track worktree clean failed");
            }
        }
    });
}

/// One pass: `git clean -fdX` in each closed track's worktree that nothing occupies and that is not
/// yet in `cleaned`. `cleaned` holds `(track_id, closed_at)`, so a reopened and closed-again track
/// is cleaned again. Returns how many worktrees it cleaned. A worktree that fails is logged and
/// retried next pass.
pub async fn clean_idle_closed_track_worktrees<P: ClosedWorktreeFacts>(
    facts: &P,
    cleaned: &mut HashSet<(String, i64)>,
    observe: ObservePoint,
) -> Result<usize, P::Error>
where
    P::Error: From<WorkspaceError>,
{
    let rows = facts.closed_idle_worktrees().await?;
    let mut count = 0;
    for row in rows {
        let worktree = match row.workspace.isolation() {
            WorkspaceIsolation::Managed => continue,
            // Guarantee G: never git-clean a borrowed source directory.
            WorkspaceIsolation::None => continue,
            WorkspaceIsolation::Worktree => row.workspace.worktree.expect("decoded worktree"),
        };
        let key = (row.track_id, row.closed_at);
        if cleaned.contains(&key) {
            continue;
        }
        let Some(track) = facts.fence(&key.0, key.1).await? else {
            continue;
        };
        let track_id = key.0.clone();
        let (_track, result) = tokio::task::spawn_blocking(move || {
            let result = workspace::worktree::clean_ignored(&track_id, &worktree, observe);
            (track, result)
        })
        .await
        .map_err(|error| WorkspaceError::Internal(format!("track worktree clean task: {error}")))?;
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
