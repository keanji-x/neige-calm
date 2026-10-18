//! Server adapters for workspace-runtime policy.
use crate::error::{CalmError, Result};
use crate::operation::workspace_lease::upstream::{
    LeaseStart, choose_lease_start, diverged_refusal, head_upstream, set_branch_upstream,
};
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use workspace_runtime::{sweep::CandidateRefs, track_worktree::TrackBranch};

#[derive(Clone, Copy)]
pub(crate) struct ServerWorkspacePorts;

#[async_trait]
impl TrackBranch for ServerWorkspacePorts {
    type Error = CalmError;

    async fn refresh_upstream(&self, repo_root: &Path) {
        crate::operation::workspace_lease::upstream_fetch::refresh_upstream(repo_root).await;
    }

    fn prepare_branch(&self, repo_root: &Path, branch: &str) -> Result<String> {
        let base = track_worktree_base(repo_root)?;
        let upstream = head_upstream(repo_root)?;
        set_branch_upstream(repo_root, branch, upstream.as_ref())?;
        Ok(base)
    }
}

impl CandidateRefs for ServerWorkspacePorts {
    fn delete_for_track(&self, track_id: &str, git_common_dirs: &[PathBuf]) {
        crate::git_candidate::refs::delete_candidate_refs_for_track(
            track_id,
            git_common_dirs.iter().map(PathBuf::as_path),
        );
    }
}

/// Where a new track branch starts: behind → upstream, ahead → HEAD, diverged → refused.
fn track_worktree_base(repo_root: &Path) -> Result<String> {
    match choose_lease_start(repo_root)? {
        LeaseStart::Head { sha } | LeaseStart::Upstream { sha } => Ok(sha),
        LeaseStart::Diverged {
            head,
            upstream,
            unpushed,
            ahead,
            behind,
        } => Err(diverged_refusal(
            repo_root, &head, &upstream, unpushed, ahead, behind,
        )),
    }
}

pub(crate) fn observe(point: &str, repo_root: &Path) {
    #[cfg(feature = "fixtures")]
    crate::test_seams::blocking_pause_point(point, repo_root.to_str().expect("test repo path"));
    #[cfg(not(feature = "fixtures"))]
    let _ = (point, repo_root);
}
