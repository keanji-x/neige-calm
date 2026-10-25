//! Kernel workspace policy. Mechanisms live in the `workspace` package.
pub(crate) mod identity;
pub(crate) mod ports;
pub(crate) mod reclaim;
pub(crate) mod sweep;
pub mod trash;

/// Short digest of a workspace path for an idempotency key: a key that does not name the path turns any
/// re-point into a permanent 409, since operation rows are never deleted.
pub(crate) fn workspace_key_digest(path: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(path.as_bytes());
    hex::encode(hasher.finalize())[..16].to_string()
}

/// Creation borrows an already selected locator. Content collaboration is not a
/// deployment resource grant; retain the execution owner's strict source/target policy.
/// Folder identity is checked separately by Truth in the allocation transaction.
pub(crate) async fn require_borrowed_admission_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    source: &crate::auth::HumanInputSource,
    target_area: &str,
    source_area: Option<&str>,
) -> crate::error::Result<()> {
    let principal = source.current_principal()?;
    crate::organization_access::require_legacy_input_execution_tx(tx, target_area, &principal)
        .await?;
    if let Some(area) = source_area {
        crate::organization_access::require_legacy_input_execution_tx(tx, area, &principal).await?;
    }
    source.current_principal()?;
    Ok(())
}

/// Read-only admission immediately before a retained effect. Repair belongs to
/// preparation; this check never creates a borrowed directory or worktree.
pub(crate) fn validate_runtime_workspace(
    workspace: &crate::model::TrackWorkspace,
) -> crate::error::Result<()> {
    if workspace.kind == crate::model::TrackWorkspaceKind::Attached {
        workspace_runtime::selection::validate_borrowed_workspace(std::path::Path::new(
            workspace.agent_cwd(),
        ))?;
    }
    Ok(())
}

/// Shared creation/retained repair of a persisted worktree selection. No claim
/// lookup or new allocation occurs on replay. The caller supplies its current
/// execution admission, which is rechecked around upstream and local effects.
pub(crate) async fn ensure_worktree<F, A>(
    track: &crate::model::Track,
    admit: F,
) -> crate::error::Result<()>
where
    F: Fn() -> A + Send + Sync,
    A: std::future::Future<Output = crate::error::Result<()>> + Send,
{
    if track.workspace.kind == crate::model::TrackWorkspaceKind::Attached
        && track.workspace.worktree.is_none()
    {
        admit().await?;
        validate_runtime_workspace(&track.workspace)?;
    }
    workspace_runtime::track_worktree::ensure_track_worktree_admitted(
        track,
        ports::ServerWorkspacePorts,
        ports::observe,
        admit,
    )
    .await
}

/// Actual allocation outcome, including a same-key race resolved inside the writer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CreationOutcome {
    Mint,
    Replay,
}

/// Fresh borrowed creation and worktree repair validate the persisted agent cwd.
/// A failed effect leaves the committed receipt intact for original-key repair.
pub(crate) async fn materialize_borrowed_creation(
    state: &crate::state::RouteState,
    source: &crate::auth::HumanInputSource,
    track: &crate::model::Track,
    outcome: CreationOutcome,
) -> crate::error::Result<()> {
    if track.workspace.kind != crate::model::TrackWorkspaceKind::Attached {
        return Ok(());
    }
    // A persisted none ACK is a locator, not a renewed filesystem promise.
    // Worktree replay still repairs through the admitted shared owner below.
    if outcome == CreationOutcome::Replay && track.workspace.worktree.is_none() {
        return Ok(());
    }
    #[cfg(feature = "fixtures")]
    crate::test_seams::pause_point("canonical-workspace-committed", track.area_id.as_str()).await;
    let admission = || {
        let source = source.clone();
        let area = track.area_id.to_string();
        crate::db::write_in_tx_typed(state.repo.as_ref(), move |tx| {
            Box::pin(async move { require_borrowed_admission_tx(tx, &source, &area, None).await })
        })
    };
    admission().await?;
    ensure_worktree(track, admission).await?;
    validate_runtime_workspace(&track.workspace)?;
    Ok(())
}
