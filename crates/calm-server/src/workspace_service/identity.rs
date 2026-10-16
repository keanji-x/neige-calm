//! Kernel registration of worktree resources. Filesystem evidence is collected outside SQL.

use sqlx::SqlitePool;

use crate::{
    db::sqlite::{begin_immediate_tx, workspace_adopt_tx, workspace_adoption_snapshot},
    error::{CalmError, Result},
};

pub(crate) async fn prepare_task(ctx: &crate::operation::SpawnCtx, track_id: &str) -> Result<()> {
    let pool = ctx.operation_repo.sqlite_pool();
    adopt_for_task(&pool, track_id).await
}

/// Register this request's current track selection. Registration does not decode or rebind
/// historical executions: their original cwd, lease and operation bytes stay with their owner.
/// Checkout admission keeps its existing track scope until legacy source conversion is complete.
pub(crate) async fn adopt_for_task(pool: &SqlitePool, track_id: &str) -> Result<()> {
    adopt(pool, track_id).await
}

/// Recheck an existing registration too: a resource identity cannot follow a replaced root.
async fn adopt(pool: &SqlitePool, track_id: &str) -> Result<()> {
    let snapshot = workspace_adoption_snapshot(&mut *pool.acquire().await?, track_id).await?;
    let selected = snapshot.selected_path.clone();
    let ownership = snapshot.ownership;
    let owner_track = snapshot.track_id.clone();
    let has_directory_record = snapshot.has_directory_record;
    let identity = tokio::task::spawn_blocking(move || {
        use crate::db::sqlite::WorkspaceOwnership;
        use std::path::Path;
        if !has_directory_record
            && ownership == WorkspaceOwnership::Managed
            && !workspace::materialize::is_materialized(Path::new(&selected), &owner_track)?
        {
            return Ok(None);
        }
        let Some(identity) = workspace::directory::DirectoryIdentity::open_if_present(
            Path::new(&selected),
            ownership != WorkspaceOwnership::Borrowed,
        )?
        else {
            return Ok(None);
        };
        match ownership {
            WorkspaceOwnership::Borrowed => Ok(Some(identity)),
            WorkspaceOwnership::Managed => identity.verify_managed_owner(&owner_track).map(Some),
            WorkspaceOwnership::TrackWorktree => {
                identity.verify_track_worktree_owner(&owner_track).map(Some)
            }
        }
    })
    .await
    .map_err(|error| CalmError::Internal(format!("workspace adoption task: {error}")))?
    .map_err(|error| CalmError::Conflict(format!("WorkspaceRecoveryRequired: {error}")))?;
    let Some(identity) = identity else {
        return if snapshot.has_directory_record {
            Err(CalmError::Conflict(
                "WorkspaceRecoveryRequired: registered directory is missing".into(),
            ))
        } else {
            // No handle or source proof is returned. V1 creation/repair stays at its old owner.
            Ok(())
        };
    };
    identity.validate()?;
    let mut tx = begin_immediate_tx(pool).await?;
    workspace_adopt_tx(&mut tx, &snapshot, &identity).await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod legacy_tests;
#[cfg(test)]
mod tests;
