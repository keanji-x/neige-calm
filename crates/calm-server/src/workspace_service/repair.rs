//! Lifecycle ownership for canonical locator materialization and retained repair.
use std::sync::Arc;
use workspace_runtime::sweep::TeardownTargets;

use crate::{
    db::RouteRepo,
    error::{CalmError, Result},
    model::Track,
    per_card_lock::{KeyedLockGuard, KeyedLocks, lock_key, try_lock_key},
};

/// Live wake owns no operation drive; driven preparation must never wait for Track.
#[derive(Clone)]
pub(crate) enum RetainedAdmission {
    Live(TeardownTargets),
    // An accepted HTTP send must not wait behind a lifecycle owner that may
    // itself be waiting for ingress. Busy handoffs defer to the fenced wake.
    Handoff(TeardownTargets),
    Driven,
}

pub(crate) async fn retained_worktree<F, A>(
    repo: Arc<dyn RouteRepo>,
    locks: &KeyedLocks,
    selected: &Track,
    admission: RetainedAdmission,
    admit: F,
) -> Result<()>
where
    F: Fn() -> A + Send + Sync + 'static,
    A: std::future::Future<Output = Result<()>> + Send,
{
    let track = selected.id.as_str();
    let wait = matches!(&admission, RetainedAdmission::Live(_));
    let lifecycle = match admission {
        RetainedAdmission::Live(targets) | RetainedAdmission::Handoff(targets) => {
            // Refuse teardown already admitted, preserving live wake's Busy
            // contract. A clean admitted later may serialize on Track; this
            // check grants no filesystem effect. Only Track and the re-read
            // below authorize repair, including after deletion or repointing.
            if targets
                .snapshot_with_owners()
                .iter()
                .any(|(owner, _)| owner == track)
            {
                return Err(CalmError::WorkspaceBusy(track.to_owned()));
            }
            if wait {
                lock_key(locks, track).await
            } else {
                try_lock_key(locks, track)
                    .ok_or_else(|| CalmError::WorkspaceBusy(track.to_owned()))?
            }
        }
        RetainedAdmission::Driven => {
            try_lock_key(locks, track).ok_or_else(|| CalmError::WorkspaceBusy(track.to_owned()))?
        }
    };
    fenced_worktree(repo, lifecycle, selected, true, admit).await
}

/// Canonical HTTP creation owns no operation drive and may wait for clean or
/// deletion. Driven retained callers use non-waiting admission above instead.
pub(super) async fn selected_worktree<F, A>(
    repo: Arc<dyn RouteRepo>,
    locks: &KeyedLocks,
    selected: &Track,
    repair: bool,
    admit: F,
) -> Result<()>
where
    F: Fn() -> A + Send + Sync + 'static,
    A: std::future::Future<Output = Result<()>> + Send,
{
    let lifecycle = lock_key(locks, selected.id.as_str()).await;
    fenced_worktree(repo, lifecycle, selected, repair, admit).await
}

// Neither admission nor Git re-enters operation drive. Share the same post-lock
// read and cancellation ownership for both lifecycle admission modes.
async fn fenced_worktree<F, A>(
    repo: Arc<dyn RouteRepo>,
    lifecycle: KeyedLockGuard,
    selected: &Track,
    repair: bool,
    admit: F,
) -> Result<()>
where
    F: Fn() -> A + Send + Sync + 'static,
    A: std::future::Future<Output = Result<()>> + Send,
{
    let selected = selected.clone();
    // Own the guard through blocking Git completion even if the HTTP/wake waiter
    // disappears. The borrowed selection can no longer outlive its Track fence.
    tokio::spawn(async move {
        let _lifecycle = lifecycle;
        let current = repo
            .track_get(selected.id.as_str())
            .await?
            .ok_or_else(|| CalmError::NotFound("track".into()))?;
        if current.area_id != selected.area_id || current.workspace != selected.workspace {
            return Err(CalmError::Conflict("workspace selection changed".into()));
        }
        if !repair {
            return Ok(());
        }
        admit().await?;
        super::ensure_worktree(&current, &admit).await?;
        admit().await?;
        super::validate_runtime_workspace(&current.workspace)
    })
    .await
    .map_err(|error| CalmError::Internal(format!("workspace repair task failed: {error}")))?
}
