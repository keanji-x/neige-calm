//! Terminal-owned decoding of v1 cwd requests. These are legacy inputs, not launch capabilities.

use crate::{
    error::{CalmError, Result},
    model::{TrackWorkspace, TrackWorkspaceKind},
    operation::SpawnCtx,
};

pub(super) enum LegacyTerminalTarget {
    WorkspaceDefault { cwd: String },
    ExternalDir { cwd: String },
}

impl LegacyTerminalTarget {
    pub(super) fn into_cwd(self) -> String {
        match self {
            Self::WorkspaceDefault { cwd } | Self::ExternalDir { cwd } => cwd,
        }
    }
}

/// V1 explicit cwd is independent of the track's current selection. An attached track
/// without a kernel worktree likewise retains its legacy directory contract, including
/// non-Git directories. These inputs carry no WorkspaceId or source proof; physical spawn
/// validation remains with the existing executor, preserving definite NoChild receipts.
/// Workspace-ref-v2 conversion and its required launch capabilities are a separate rollout.
pub(super) fn decode(
    track_id: &str,
    workspace: &TrackWorkspace,
    requested: Option<String>,
) -> Result<LegacyTerminalTarget> {
    if let Some(cwd) = requested {
        return Ok(LegacyTerminalTarget::ExternalDir { cwd });
    }
    let cwd = workspace.agent_cwd();
    if cwd.trim().is_empty() {
        return Err(CalmError::Internal(format!(
            "track {track_id} has no workspace path; refusing to open a terminal in the server's cwd"
        )));
    }
    let cwd = cwd.to_owned();
    if workspace.kind == TrackWorkspaceKind::Attached && workspace.worktree.is_none() {
        Ok(LegacyTerminalTarget::ExternalDir { cwd })
    } else {
        Ok(LegacyTerminalTarget::WorkspaceDefault { cwd })
    }
}

pub(super) async fn prepare(
    ctx: &SpawnCtx,
    track_id: &str,
    explicit: Option<String>,
) -> Result<()> {
    let track = ctx
        .repo
        .track_get(track_id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("track {track_id}")))?;
    match decode(track_id, &track.workspace, explicit)? {
        LegacyTerminalTarget::WorkspaceDefault { .. } => {
            crate::workspace_service::identity::observe_for_task(
                &ctx.operation_repo.sqlite_pool(),
                track_id,
            )
            .await
        }
        LegacyTerminalTarget::ExternalDir { .. } => Ok(()),
    }
}
