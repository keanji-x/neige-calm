//! Transactional capture for postcommit teardown.
use crate::error::Result;
use crate::operation::Tx;
use std::path::PathBuf;
use workspace_runtime::sweep::WorkspaceTrackSweep;

pub(crate) async fn workspace_track_sweep_for_track_tx(
    tx: &mut Tx<'_>,
    track_id: &str,
    deleting: &[String],
) -> Result<Option<WorkspaceTrackSweep>> {
    let Some(track) = calm_truth::db::sqlite::track_find_tx(tx, track_id).await? else {
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
    let extra_worktrees = super::reclaim::snapshot(tx, track_id, deleting).await?;
    Ok(Some(WorkspaceTrackSweep {
        track_id: track_id.to_string(),
        workspace: track.workspace,
        git_common_dirs,
        extra_worktrees,
        removed_refs: super::reclaim::removed_refs(track_id)?,
    }))
}

#[cfg(feature = "fixtures")]
#[derive(Clone)]
pub struct WorkspaceSweepHook {
    pub entered: std::sync::Arc<tokio::sync::Notify>,
    pub release: std::sync::Arc<tokio::sync::Notify>,
}

#[cfg(feature = "fixtures")]
fn hooks() -> &'static std::sync::Mutex<std::collections::HashMap<String, WorkspaceSweepHook>> {
    static HOOKS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, WorkspaceSweepHook>>,
    > = std::sync::OnceLock::new();
    HOOKS.get_or_init(Default::default)
}

#[cfg(feature = "fixtures")]
pub fn install_workspace_sweep_hook_for_test(track: &str, hook: WorkspaceSweepHook) {
    hooks().lock().unwrap().insert(track.to_owned(), hook);
}

pub(crate) async fn before_sweep(sweeps: &[WorkspaceTrackSweep]) {
    #[cfg(feature = "fixtures")]
    for sweep in sweeps {
        let hook = hooks().lock().unwrap().remove(&sweep.track_id);
        if let Some(hook) = hook {
            hook.entered.notify_one();
            hook.release.notified().await;
        }
    }
    #[cfg(not(feature = "fixtures"))]
    let _ = sweeps;
}
