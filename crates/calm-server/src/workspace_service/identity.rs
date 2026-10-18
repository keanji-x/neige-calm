//! Kernel observation of worktree resources. Filesystem evidence is collected outside SQL.

use crate::{
    db::sqlite::{
        DirectoryObservation, begin_immediate_tx, workspace_observation_snapshot,
        workspace_observe_tx,
    },
    error::{CalmError, Result},
};

pub(crate) async fn prepare_task(ctx: &crate::operation::SpawnCtx, track_id: &str) -> Result<()> {
    let pool = ctx.operation_repo.sqlite_db();
    observe_for_task(&pool, track_id).await
}

/// Observe this request's current track selection. Observation does not decode or rebind
/// historical executions: their original cwd, lease and operation bytes stay with their owner.
/// Checkout admission keeps its existing track scope until legacy source conversion is complete.
pub(crate) async fn observe_for_task(
    pool: &crate::db::sqlite::SqliteDb,
    track_id: &str,
) -> Result<()> {
    observe(pool, track_id).await
}

/// Recheck the current selection; persisted inode facts do not identify an object incarnation.
async fn observe(pool: &crate::db::sqlite::SqliteDb, track_id: &str) -> Result<()> {
    let snapshot =
        workspace_observation_snapshot(&mut *pool.read_pool().acquire().await?, track_id).await?;
    let selected = snapshot.selected_path.clone();
    let ownership = snapshot.ownership;
    let owner_track = snapshot.track_id.clone();
    let previously_observed = matches!(snapshot.state, DirectoryObservation::Observed(_));
    let identity = tokio::task::spawn_blocking(move || {
        use crate::db::sqlite::WorkspaceOwnership;
        use std::path::Path;
        if !previously_observed
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
    .map_err(|error| CalmError::Internal(format!("workspace observation task: {error}")))?
    .map_err(|error| CalmError::Conflict(format!("WorkspaceRecoveryRequired: {error}")))?;
    let Some(identity) = identity else {
        return if previously_observed {
            Err(CalmError::Conflict(
                "WorkspaceRecoveryRequired: observed directory is missing".into(),
            ))
        } else {
            // No handle or source proof is returned. V1 creation/repair stays at its old owner.
            Ok(())
        };
    };
    identity.validate()?;
    let mut tx = begin_immediate_tx(pool).await?;
    workspace_observe_tx(&mut tx, &snapshot, &identity).await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod binding_tests;
#[cfg(test)]
mod legacy_tests;
#[cfg(test)]
mod tests;
