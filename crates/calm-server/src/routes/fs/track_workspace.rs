//! Ordinary Track reads share the secure filesystem owner with attachments.
use super::{OpenWorkspaceFile, Result};
use std::path::Path;

pub(super) async fn open_track_workspace_file(
    workspace_root: &Path,
    relative_path: &str,
) -> Result<OpenWorkspaceFile> {
    super::open_workspace_regular_file(
        workspace_root,
        relative_path,
        super::WorkspaceSymlinks::FollowedInsideRoot,
    )
    .await
}
