//! Transactional capture for postcommit teardown.
use crate::error::Result;
use crate::operation::Tx;
use std::path::PathBuf;
use workspace_runtime::sweep::WorkspaceTrackSweep;

pub(crate) async fn workspace_track_sweep_for_track_tx(
    tx: &mut Tx<'_>,
    track_id: &str,
) -> Result<Option<WorkspaceTrackSweep>> {
    let Some(track_worktree) = sqlx::query_scalar::<_, Option<String>>(
        "SELECT workspace_worktree_path FROM tracks WHERE id = ?1",
    )
    .bind(track_id)
    .fetch_optional(&mut **tx)
    .await?
    else {
        return Ok(None);
    };
    let git_common_dirs = sqlx::query_scalar::<_, String>(
        "SELECT DISTINCT git_common_dir FROM workspace_leases \
         WHERE track_id = ?1 AND git_common_dir IS NOT NULL ORDER BY git_common_dir",
    )
    .bind(track_id)
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(PathBuf::from)
    .collect();
    Ok(Some(WorkspaceTrackSweep {
        track_id: track_id.to_string(),
        track_worktree,
        git_common_dirs,
    }))
}
