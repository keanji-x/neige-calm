//! Lifecycle ownership for canonical locator materialization and retained repair.
use std::sync::Arc;
use workspace_runtime::sweep::TeardownTargets;

use crate::{
    db::RouteRepo,
    error::{CalmError, Result},
    model::Track,
    per_card_lock::{KeyedLockGuard, KeyedLocks, lock_key, try_lock_key},
};

/// Live wake owns no operation drive; driven preparation must never wait for Track.
#[derive(Clone)]
pub(crate) enum RetainedAdmission {
    Live(TeardownTargets),
    // An accepted HTTP send must not wait behind a lifecycle owner that may
    // itself be waiting for ingress. Busy handoffs defer to the fenced wake.
    Handoff(TeardownTargets),
    Driven,
}

/// Preserve retained callers' original preparation point. Locator repair alone
/// never initializes a managed directory; fresh activation supplies its root.
pub(crate) enum RetainedPreparation {
    Locator,
    Managed(std::path::PathBuf),
}

/// Resource preparation preserves existing Git history; retained activation may
/// still establish the baseline required by its original materialization contract.
#[derive(Clone, Copy)]
pub(crate) enum ManagedPreparation {
    Existing,
    InitializeMissing,
    Materialize,
}

pub(crate) async fn retained_worktree<F, A>(
    repo: Arc<dyn RouteRepo>,
    locks: &KeyedLocks,
    selected: &Track,
    admission: RetainedAdmission,
    preparation: RetainedPreparation,
    admit: F,
) -> Result<()>
where
    F: Fn() -> A + Send + Sync + 'static,
    A: std::future::Future<Output = Result<()>> + Send,
{
    let track = selected.id.as_str();
    let wait = matches!(&admission, RetainedAdmission::Live(_));
    let lifecycle = match admission {
        RetainedAdmission::Live(targets) | RetainedAdmission::Handoff(targets) => {
            // Refuse teardown already admitted, preserving live wake's Busy
            // contract. A clean admitted later may serialize on Track; this
            // check grants no filesystem effect. Only Track and the re-read
            // below authorize repair, including after deletion or repointing.
            if targets
                .snapshot_with_owners()
                .iter()
                .any(|(owner, _)| owner == track)
            {
                return Err(CalmError::WorkspaceBusy(track.to_owned()));
            }
            if wait {
                lock_key(locks, track).await
            } else {
                try_lock_key(locks, track)
                    .ok_or_else(|| CalmError::WorkspaceBusy(track.to_owned()))?
            }
        }
        RetainedAdmission::Driven => {
            try_lock_key(locks, track).ok_or_else(|| CalmError::WorkspaceBusy(track.to_owned()))?
        }
    };
    let workspace_root = match preparation {
        RetainedPreparation::Locator => None,
        RetainedPreparation::Managed(root) => Some(root),
    };
    fenced_worktree(repo, lifecycle, selected, true, workspace_root, admit).await
}

/// Canonical HTTP creation owns no operation drive and may wait for clean or
/// deletion. Driven retained callers use non-waiting admission above instead.
pub(super) async fn selected_worktree<F, A>(
    repo: Arc<dyn RouteRepo>,
    locks: &KeyedLocks,
    selected: &Track,
    repair: bool,
    admit: F,
) -> Result<()>
where
    F: Fn() -> A + Send + Sync + 'static,
    A: std::future::Future<Output = Result<()>> + Send,
{
    let lifecycle = lock_key(locks, selected.id.as_str()).await;
    fenced_worktree(repo, lifecycle, selected, repair, None, admit).await
}

// Neither admission nor Git re-enters operation drive. Share the same post-lock
// read and cancellation ownership for both lifecycle admission modes.
async fn fenced_worktree<F, A>(
    repo: Arc<dyn RouteRepo>,
    lifecycle: KeyedLockGuard,
    selected: &Track,
    repair: bool,
    workspace_root: Option<std::path::PathBuf>,
    admit: F,
) -> Result<()>
where
    F: Fn() -> A + Send + Sync + 'static,
    A: std::future::Future<Output = Result<()>> + Send,
{
    fenced_selection(
        repo,
        lifecycle,
        selected,
        move |current, _cancelled| async move {
            if !repair {
                return Ok(());
            }
            admit().await?;
            super::ensure_worktree(&current, &admit).await?;
            if let Some(root) = workspace_root
                && current.workspace.kind == crate::model::TrackWorkspaceKind::Managed
            {
                admit().await?;
                prepare_managed(&current, root, ManagedPreparation::Materialize).await?;
            }
            admit().await?;
            super::validate_runtime_workspace(&current.workspace)
        },
    )
    .await
}

/// Execute a resource consumer under the same lifecycle owner as retained repair.
/// The cancellation signal stops streaming; non-cancellable filesystem work still
/// belongs to the spawned owner until its last effect has returned.
pub(crate) async fn managed_effect<F, A, E, EF, T>(
    repo: Arc<dyn RouteRepo>,
    locks: &KeyedLocks,
    selected: &Track,
    root: std::path::PathBuf,
    preparation: ManagedPreparation,
    admit: F,
    effect: E,
) -> Result<T>
where
    F: Fn() -> A + Send + Sync + 'static,
    A: std::future::Future<Output = Result<()>> + Send,
    E: FnOnce(tokio::sync::oneshot::Receiver<()>) -> EF + Send + 'static,
    EF: std::future::Future<Output = Result<T>> + Send,
    T: Send + 'static,
{
    let lifecycle = lock_key(locks, selected.id.as_str()).await;
    fenced_selection(
        repo,
        lifecycle,
        selected,
        move |current, mut cancelled| async move {
            admit().await?;
            prepare_managed(&current, root, preparation).await?;
            admit().await?;
            if matches!(
                cancelled.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Closed)
            ) {
                return Err(CalmError::BadRequest(
                    "workspace resource request cancelled".into(),
                ));
            }
            effect(cancelled).await
        },
    )
    .await
}

/// One post-lock selection check for every resource owner and its final admission.
pub(crate) async fn selected_track(repo: &dyn RouteRepo, selected: &Track) -> Result<Track> {
    let current = repo
        .track_get(selected.id.as_str())
        .await?
        .ok_or_else(|| CalmError::NotFound("track".into()))?;
    if current.area_id != selected.area_id || current.workspace != selected.workspace {
        return Err(CalmError::Conflict("workspace selection changed".into()));
    }
    Ok(current)
}

async fn fenced_selection<E, EF, T>(
    repo: Arc<dyn RouteRepo>,
    lifecycle: KeyedLockGuard,
    selected: &Track,
    effect: E,
) -> Result<T>
where
    E: FnOnce(Track, tokio::sync::oneshot::Receiver<()>) -> EF + Send + 'static,
    EF: std::future::Future<Output = Result<T>> + Send,
    T: Send + 'static,
{
    let selected = selected.clone();
    let (waiter, cancelled) = tokio::sync::oneshot::channel();
    let result = tokio::spawn(async move {
        let _lifecycle = lifecycle;
        let current = selected_track(repo.as_ref(), &selected).await?;
        effect(current, cancelled).await
    })
    .await
    .map_err(|error| CalmError::Internal(format!("workspace resource task failed: {error}")))?;
    drop(waiter);
    result
}

/// Reuse the existing materializer for both retained activation and uploads.
/// Canonical creation remains lazy. Refuse corrupt selectors and existing links
/// before the path-based materializer can initialize Git or write its exclude.
/// An existing resource must validate as an owned Git root, even with unborn HEAD;
/// neither failed validation nor missing HEAD authorizes baseline repair. Upload's
/// store owner already prepares the Git exclude before accepting any bytes.
async fn prepare_managed(
    track: &Track,
    root: std::path::PathBuf,
    preparation: ManagedPreparation,
) -> Result<()> {
    use crate::model::TrackWorkspaceKind;
    if track.workspace.kind != TrackWorkspaceKind::Managed {
        return Err(CalmError::BadRequest(
            "resource requires a managed workspace".into(),
        ));
    }
    let track = track.clone();
    tokio::task::spawn_blocking(move || {
        let path = std::path::Path::new(&track.workspace.path);
        let expected = workspace::materialize::managed_workspace_path(
            &root,
            track.area_id.as_str(),
            track.id.as_str(),
        );
        if path != expected || track.workspace.worktree.is_some() {
            return Err(CalmError::Conflict(
                "managed workspace selection requires recovery".into(),
            ));
        }
        for ancestor in path.ancestors().chain([path.join(".git").as_path()]) {
            match std::fs::symlink_metadata(ancestor) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    return Err(CalmError::Conflict(
                        "managed workspace symlinks are refused".into(),
                    ));
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        let reuse = match preparation {
            ManagedPreparation::Existing => true,
            ManagedPreparation::InitializeMissing => {
                match std::fs::symlink_metadata(path.join(".git")) {
                    Ok(_) => true,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                    Err(error) => return Err(error.into()),
                }
            }
            ManagedPreparation::Materialize => false,
        };
        if reuse {
            workspace::directory::DirectoryIdentity::open(path, true)?
                .verify_managed_owner(track.id.as_str())?;
            return Ok(());
        }
        #[cfg(feature = "fixtures")]
        crate::test_seams::blocking_pause_point(
            "workspace-managed-materializing",
            track.id.as_str(),
        );
        workspace_runtime::selection::materialize_managed_workspace(
            &root,
            path,
            track.id.as_str(),
        )?;
        workspace::directory::DirectoryIdentity::open(path, true)?
            .verify_managed_owner(track.id.as_str())?;
        Ok(())
    })
    .await
    .map_err(|error| CalmError::Internal(format!("workspace preparation task failed: {error}")))?
    .map_err(|error| {
        tracing::error!(%error, "managed workspace preparation failed");
        match error {
            CalmError::Conflict(_) | CalmError::BadRequest(_) => error,
            _ => CalmError::Internal("managed workspace preparation failed".into()),
        }
    })
}
