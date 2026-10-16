//! Kernel registration of worktree resources. Filesystem evidence is collected outside SQL.

use std::path::Path;

use sqlx::SqlitePool;

use crate::{
    db::sqlite::{begin_immediate_tx, workspace_adopt_tx, workspace_adoption_snapshot},
    error::{CalmError, Result},
};

pub(crate) async fn prepare_task(
    ctx: &crate::operation::SpawnCtx,
    track_id: &str,
    workspace_root: &Path,
) -> Result<()> {
    let track = ctx
        .repo
        .track_get(track_id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("track {track_id}")))?;
    let root = workspace_root.to_path_buf();
    tokio::task::spawn_blocking(move || {
        super::workspace_materialize::materialize_workspace(
            &track.workspace,
            &root,
            track.id.as_str(),
        )
    })
    .await
    .map_err(|error| CalmError::Internal(format!("workspace materialization task: {error}")))??;
    let pool = ctx.operation_repo.sqlite_pool();
    adopt_for_task(&pool, track_id).await
}

/// Reconcile active historical selections before a new task can be admitted. A legacy row
/// whose physical source cannot be proved is recovery; it is never assumed to be unrelated.
pub(crate) async fn adopt_for_task(pool: &SqlitePool, track_id: &str) -> Result<()> {
    let legacy: Vec<String> = sqlx::query_scalar(
        "SELECT b.track_id FROM track_workspace_bindings b JOIN workspace_resources r \
         ON r.id=b.workspace_id WHERE r.adopted_workspace_id IS NULL AND ( \
         EXISTS(SELECT 1 FROM current_tasks t WHERE t.track_id=b.track_id \
                AND t.status IN ('dispatched','running','verifying')) OR \
         EXISTS(SELECT 1 FROM workspace_leases wl WHERE wl.track_id=b.track_id \
                AND wl.state IN ('held','releasing')) OR \
         EXISTS(SELECT 1 FROM task_git_deliveries d WHERE d.track_id=b.track_id \
                AND d.settlement IS NULL)) ORDER BY b.workspace_id,b.track_id",
    )
    .fetch_all(pool)
    .await?;
    for legacy_track in legacy {
        adopt(pool, &legacy_track).await?;
    }
    adopt(pool, track_id).await
}

/// Recheck an existing registration too: a resource identity cannot follow a replaced root.
async fn adopt(pool: &SqlitePool, track_id: &str) -> Result<()> {
    let snapshot = workspace_adoption_snapshot(&mut *pool.acquire().await?, track_id).await?;
    let selected = snapshot.selected_path.clone();
    let ownership = snapshot.ownership;
    let owner_track = snapshot.track_id.clone();
    let identity = tokio::task::spawn_blocking(move || {
        use crate::db::sqlite::WorkspaceOwnership;
        let identity = workspace::directory::DirectoryIdentity::open(
            Path::new(&selected),
            ownership != WorkspaceOwnership::Borrowed,
        )?;
        match ownership {
            WorkspaceOwnership::Borrowed => Ok(identity),
            WorkspaceOwnership::Managed => identity.verify_managed_owner(&owner_track),
            WorkspaceOwnership::TrackWorktree => identity.verify_track_worktree_owner(&owner_track),
        }
    })
    .await
    .map_err(|error| CalmError::Internal(format!("workspace adoption task: {error}")))?
    .map_err(|error| CalmError::Conflict(format!("WorkspaceRecoveryRequired: {error}")))?;
    identity.validate()?;
    let mut tx = begin_immediate_tx(pool).await?;
    workspace_adopt_tx(&mut tx, &snapshot, &identity).await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests;
