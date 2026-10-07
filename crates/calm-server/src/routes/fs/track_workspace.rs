//! The read capability for ordinary Track workspace files only.
//!
//! Attachment and write callers retain the generic Linux-only openers in `fs`.

use super::{OpenWorkspaceFile, Result};
use std::path::Path;

#[cfg(target_os = "macos")]
mod macos;

pub(super) async fn open_track_workspace_file(
    workspace_root: &Path,
    relative_path: &str,
) -> Result<OpenWorkspaceFile> {
    #[cfg(target_os = "macos")]
    {
        macos::open(workspace_root, relative_path).await
    }
    #[cfg(not(target_os = "macos"))]
    {
        super::open_workspace_regular_file(
            workspace_root,
            relative_path,
            super::WorkspaceSymlinks::FollowedInsideRoot,
        )
        .await
    }
}
