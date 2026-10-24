//! Closed-track maintenance: retain the default worktree for reopen, clean its ignored output
//! once per close, and retry event-owned extra worktrees through the shared safe reclaimer.
//! Every close writer is covered by the existing tick; there is no separate garbage timer.

use std::collections::HashSet;
use std::time::Duration;

use async_trait::async_trait;
use calm_types::model::{TrackWorkspace, TrackWorkspaceKind};

use crate::error::WorkspaceError;
use crate::track_worktree::ObservePoint;

/// A closed track row, retaining workspace ownership for the runtime's clean policy.
pub struct ClosedWorktree {
    pub track_id: String,
    pub workspace: TrackWorkspace,
    pub closed_at: i64,
}

#[async_trait]
pub trait ClosedWorktreeFacts:
    crate::reclaim::ReclaimReceipts + Clone + Send + Sync + 'static
{
    async fn kernel_clean_safe(&self, workspace: &TrackWorkspace) -> Result<bool, Self::Error>;
    async fn extra_worktrees(
        &self,
        track: &str,
    ) -> Result<Vec<crate::reclaim::ExtraWorktree>, Self::Error>;

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
        let key = (row.track_id.clone(), row.closed_at);
        let Some(track) = facts.fence(&key.0, key.1).await? else {
            continue;
        };
        let targets = facts.extra_worktrees(&key.0).await?;
        let clean_kernel =
            facts.kernel_clean_safe(&row.workspace).await? && !cleaned.contains(&key);
        let facts = facts.clone();
        // Keep the lifecycle fence through I/O and receipts even if the tick's await is cancelled.
        let result = tokio::spawn(async move {
            let _track = track;
            crate::reclaim::reclaim_extra_worktrees(targets, &facts, observe).await?;
            if !clean_kernel {
                return Ok(false);
            }
            let worktree = match (row.workspace.kind, row.workspace.worktree) {
                (TrackWorkspaceKind::Attached, Some(path)) => path,
                _ => return Ok(false),
            };
            tokio::task::spawn_blocking(move || {
                workspace::worktree::clean_ignored(&row.track_id, &worktree, observe)
            })
            .await
            .map_err(|e| WorkspaceError::Internal(format!("track worktree clean task: {e}")))?
            .map_err(WorkspaceError::from)
        })
        .await
        .map_err(|e| WorkspaceError::Internal(format!("track clean task: {e}")))?;
        match result {
            Ok(true) => {
                count += 1;
                cleaned.insert(key);
            }
            Ok(false) => {}
            Err(error) => tracing::warn!(track_id = %key.0, %error, "track worktree clean failed"),
        }
    }
    Ok(count)
}
