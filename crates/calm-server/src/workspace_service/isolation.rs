//! Atomic same-locator isolation selection, shared by explicit and lazy upgrades.
use crate::{
    db::sqlite::{TrackWorkspacePlan, track_get_tx, track_workspace_write_tx},
    error::{CalmError, Result},
    model::{Track, TrackWorkspaceKind},
};
use std::path::{Path, PathBuf};

pub(crate) async fn select_worktree_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    expected: &Track,
    repo_root: &Path,
    owned_roots: &[PathBuf],
    teardown: &workspace_runtime::sweep::TeardownTargets,
) -> Result<Track> {
    super::refuse_admitted_teardown(teardown, expected.id.as_str())?;
    let live = track_get_tx(tx, &expected.id).await?;
    if live.closed_at.is_some()
        || live.workspace != expected.workspace
        || live.workspace.kind != TrackWorkspaceKind::Attached
        || live.workspace.worktree.is_some()
    {
        return Err(CalmError::Conflict(
            "workspace changed before isolation upgrade".into(),
        ));
    }
    let mut workspace = live.workspace.clone();
    workspace.frozen_at = Some(workspace.frozen_at.unwrap_or_else(crate::model::now_ms));
    workspace.worktree = Some(
        workspace::worktree::track_worktree_path_for(repo_root, live.id.as_str())
            .to_string_lossy()
            .into_owned(),
    );
    crate::db::sqlite::admit_attached_workspace_tx(
        tx,
        &workspace.path,
        owned_roots,
        &teardown.snapshot(),
        &TrackWorkspacePlan::AttachedWithTrackWorktree(repo_root.to_owned()),
    )
    .await?;
    calm_truth::db::sqlite::workspace_retirement::retire_workspace_selection_tx(
        tx,
        live.id.as_str(),
    )
    .await?;
    crate::harness::wiring::preserve_bound_track_sessions_cwd_tx(
        tx,
        live.id.as_str(),
        live.workspace.agent_cwd(),
    )
    .await?;
    track_workspace_write_tx(tx, live.id.as_str(), &workspace).await?;
    let track = track_get_tx(tx, &live.id).await?;
    crate::track_plugin_access::grant_git_workspace_plugins_tx(tx, &track).await?;
    Ok(track)
}
