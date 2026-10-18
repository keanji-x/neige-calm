//! Postcommit teardown policy.
use crate::track_worktree::{self, ObservePoint};
use std::path::PathBuf;

pub trait CandidateRefs: Clone + Send + Sync + 'static {
    fn delete_for_track(&self, track_id: &str, git_common_dirs: &[PathBuf]);
}

/// What a track teardown removes after its commit (D10): the track worktree and its branch
/// (#1830 S1), and the candidate refs in every common dir a lease of the track recorded.
#[derive(Clone, Debug)]
pub struct WorkspaceTrackSweep {
    pub track_id: String,
    /// `tracks.workspace_worktree_path`: removed with the track, whatever its leases.
    pub track_worktree: Option<String>,
    pub git_common_dirs: Vec<PathBuf>,
}

/// The post-commit half of a track or area teardown, best effort: the track worktree, then the
/// candidate refs, addressed by the lease rows' common dirs (not by the track cwd, which may be a
/// moved linked worktree). Never fails the caller: the track rows are already gone.
pub async fn sweep_workspace_worktrees_for_tracks<P: CandidateRefs>(
    sweeps: Vec<WorkspaceTrackSweep>,
    ports: P,
    observe: ObservePoint,
) {
    if let Err(error) = tokio::task::spawn_blocking(move || {
        sweep_workspace_worktrees_blocking(sweeps, ports, observe)
    })
    .await
    {
        tracing::warn!(%error, "track teardown Git sweep task failed");
    }
}

fn sweep_workspace_worktrees_blocking<P: CandidateRefs>(
    sweeps: Vec<WorkspaceTrackSweep>,
    ports: P,
    observe: ObservePoint,
) {
    for sweep in sweeps {
        if let Some(track_worktree) = &sweep.track_worktree {
            track_worktree::remove_track_worktree(&sweep.track_id, track_worktree, observe);
        }
        ports.delete_for_track(&sweep.track_id, &sweep.git_common_dirs);
    }
}
