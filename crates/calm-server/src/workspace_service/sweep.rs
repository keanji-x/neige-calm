//! Transactional capture and postcommit teardown policy.
use super::track_worktree;
use crate::error::Result;
use crate::operation::Tx;
use std::path::PathBuf;
/// What a track teardown removes after its commit (D10): the track worktree and its branch
/// (#1830 S1), and the candidate refs in every common dir a lease of the track recorded.
#[derive(Clone, Debug)]
pub(crate) struct WorkspaceTrackSweep {
    pub(crate) track_id: String,
    /// `tracks.workspace_worktree_path`: removed with the track, whatever its leases.
    pub(crate) track_worktree: Option<String>,
    pub(crate) git_common_dirs: Vec<PathBuf>,
}

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

/// The post-commit half of a track or area teardown, best effort: the track worktree, then the
/// candidate refs, addressed by the lease rows' common dirs (not by the track cwd, which may be a
/// moved linked worktree). Never fails the caller: the track rows are already gone.
pub(crate) async fn sweep_workspace_worktrees_for_tracks(sweeps: Vec<WorkspaceTrackSweep>) {
    if let Err(error) =
        tokio::task::spawn_blocking(move || sweep_workspace_worktrees_blocking(sweeps)).await
    {
        tracing::warn!(%error, "track teardown Git sweep task failed");
    }
}

fn sweep_workspace_worktrees_blocking(sweeps: Vec<WorkspaceTrackSweep>) {
    for sweep in sweeps {
        if let Some(track_worktree) = &sweep.track_worktree {
            track_worktree::remove_track_worktree(&sweep.track_id, track_worktree);
        }
        crate::git_candidate::refs::delete_candidate_refs_for_track(
            &sweep.track_id,
            sweep.git_common_dirs.iter().map(PathBuf::as_path),
        );
    }
}
