//! Project environment selection follows the workspace/lease owner, never Git private paths.
use crate::{
    db::RouteRepo,
    error::{CalmError, Result},
    execution_environment::{PROXY_KEYS, proxy_value_is_secret},
    model::TrackWorkspace,
};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
};
use workspace::environment::Environment;

mod admitted;
pub(crate) use admitted::resolve_for_directory_admitted;
#[cfg(test)]
mod cancellation_tests;

fn filter_environment(mut environment: Environment) -> Environment {
    // The kernel alone supplies its connection, identity and lifecycle inputs.
    environment.retain(|key| !key.to_str().is_some_and(|key| key.starts_with("NEIGE_")));
    environment
}

/// Map a project evaluation refusal without changing owner or worker errors.
pub(crate) fn map_environment_error(error: workspace::environment::EnvironmentError) -> CalmError {
    CalmError::BadRequest(error.to_string())
}

/// The same developer-tool base is used in the kernel and inside admitted loaders.
pub(crate) fn directory_base_environment() -> BTreeMap<OsString, OsString> {
    let policy = calm_session::control::execution::AmbientProxyPolicy::RejectSecret;
    calm_proc_supervisor::ambient_keys(&policy)
        .chain(["DIRENV_CONFIG"])
        .filter(|key| !calm_types::forge_env::FORGE_CREDENTIAL_ENV_KEYS.contains(key))
        .filter_map(|key| {
            let value = std::env::var_os(key)?;
            // Project code receives public routing defaults. The owning provider
            // restores its credential-bearing proxy only after project evaluation.
            if PROXY_KEYS.contains(&key) && proxy_value_is_secret(key, &value) {
                return None;
            }
            Some((key.into(), value))
        })
        .collect()
}

/// Resolve a track's source environment while retaining its separately admitted cwd.
pub(crate) async fn resolve_for_track<F>(
    repo: &dyn RouteRepo,
    track_id: &str,
    cwd: &Path,
    revalidate: impl FnOnce() -> F + Send,
) -> Result<std::result::Result<Environment, workspace::environment::EnvironmentError>>
where
    F: std::future::Future<Output = Result<()>> + Send,
{
    resolve_for_directory_admitted(&source_for_track(repo, track_id, cwd).await?, revalidate).await
}

/// Select the environment source without evaluating project code during preparation.
pub(crate) async fn source_for_track(
    repo: &dyn RouteRepo,
    track_id: &str,
    cwd: &Path,
) -> Result<PathBuf> {
    source_mapping_for_track(repo, track_id).await?.select(cwd)
}

/// Metadata needed to select an actual tool cwd without evaluating project code.
pub(crate) async fn source_mapping_for_track(
    repo: &dyn RouteRepo,
    track_id: &str,
) -> Result<SourceMapping> {
    let track = repo
        .track_get(track_id)
        .await?
        .ok_or_else(|| CalmError::NotFound("workspace environment track".into()))?;
    source_mapping(&track.workspace, None).await
}

pub(crate) struct SourceMapping {
    pub(crate) source: PathBuf,
    pub(crate) derived_roots: Vec<PathBuf>,
}

impl SourceMapping {
    pub(crate) fn select(&self, cwd: &Path) -> Result<PathBuf> {
        source_directory(
            &self.source,
            &std::fs::canonicalize(cwd)?,
            &self.derived_roots,
        )
    }
}

/// Keep the admitted task's recorded worktree relation in metadata-only launch plans.
pub(crate) async fn source_for_card(
    repo: &dyn RouteRepo,
    card_id: &str,
    cwd: &Path,
) -> Result<PathBuf> {
    let card = repo
        .card_get(card_id)
        .await?
        .ok_or_else(|| CalmError::NotFound("workspace environment card".into()))?;
    let track = repo
        .track_get(card.track_id.as_str())
        .await?
        .ok_or_else(|| CalmError::NotFound("workspace environment track".into()))?;
    let lease = repo.latest_workspace_lease_for_card(card_id).await?;
    selected_source(
        &track.workspace,
        cwd,
        lease.as_ref().map(|lease| Path::new(&lease.path)),
    )
    .await
}

async fn selected_source(
    workspace: &TrackWorkspace,
    cwd: &Path,
    lease_root: Option<&Path>,
) -> Result<PathBuf> {
    source_mapping(workspace, lease_root).await?.select(cwd)
}

async fn source_mapping(
    workspace: &TrackWorkspace,
    lease_root: Option<&Path>,
) -> Result<SourceMapping> {
    if !Path::new(&workspace.path).is_absolute() {
        return Err(CalmError::BadRequest(
            "workspace environment requires absolute directories".into(),
        ));
    }
    let mut derived_roots = Vec::new();
    for root in std::iter::once(Path::new(workspace.agent_cwd())).chain(lease_root) {
        match tokio::fs::canonicalize(root).await {
            Ok(root) => derived_roots.push(root),
            // Retired locators are provenance, not prerequisites for a separately
            // admitted existing cwd. An actually missing cwd still fails above.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    derived_roots.sort_by_key(|root| std::cmp::Reverse(root.components().count()));
    Ok(SourceMapping {
        source: workspace.path.clone().into(),
        derived_roots,
    })
}

fn source_directory(source: &Path, cwd: &Path, derived_roots: &[PathBuf]) -> Result<PathBuf> {
    if !source.is_absolute() || !cwd.is_absolute() {
        return Err(CalmError::BadRequest(
            "workspace environment requires absolute directories".into(),
        ));
    }
    for root in derived_roots {
        if let Ok(relative) = cwd.strip_prefix(root) {
            // New worktree-only subdirectories have no source counterpart yet.
            // Use the nearest existing source directory, within this same project.
            let mapped = source.join(relative);
            for directory in mapped.ancestors() {
                if !directory.starts_with(source) {
                    break;
                }
                match std::fs::metadata(directory) {
                    Ok(meta) if meta.is_dir() => return Ok(directory.to_owned()),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Ok(_) => {
                        return Err(CalmError::BadRequest(
                            "workspace environment source is not a directory".into(),
                        ));
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            return Err(CalmError::BadRequest(
                "workspace environment source is unavailable".into(),
            ));
        }
    }
    // An explicitly admitted unrelated cwd selects its own directory environment.
    Ok(cwd.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::TrackWorkspaceKind;

    #[test]
    fn workspace_environment_source_follows_owned_external_worktrees() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("source");
        let nested = project.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        let checkout = temp.path().join("external-worktree");
        let worker = temp.path().join("worker-lease");
        let workspace = TrackWorkspace {
            kind: TrackWorkspaceKind::Attached,
            path: project.display().to_string(),
            worktree: Some(checkout.display().to_string()),
            frozen_at: Some(1),
        };
        let roots = [checkout.clone(), worker.clone()];
        assert_eq!(
            source_directory(Path::new(&workspace.path), &checkout.join("nested"), &roots).unwrap(),
            nested
        );
        assert_eq!(
            source_directory(Path::new(&workspace.path), &worker, &roots).unwrap(),
            project
        );
        assert_eq!(
            source_directory(
                Path::new(&workspace.path),
                &checkout.join("new-directory"),
                &roots
            )
            .unwrap(),
            project
        );
        let other = temp.path().join("unrelated-project");
        assert_eq!(
            source_directory(Path::new(&workspace.path), &other, &roots).unwrap(),
            other
        );
    }

    #[tokio::test]
    async fn workspace_environment_deleted_history_does_not_block_current_worktree() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        let checkout = temp.path().join("checkout");
        std::fs::create_dir_all(project.join("nested")).unwrap();
        std::fs::create_dir_all(checkout.join("nested")).unwrap();
        let history = temp.path().join("released-worktree");
        let workspace = TrackWorkspace {
            kind: TrackWorkspaceKind::Attached,
            path: project.display().to_string(),
            worktree: Some(checkout.display().to_string()),
            frozen_at: Some(1),
        };
        assert_eq!(
            selected_source(&workspace, &checkout.join("nested"), Some(&history))
                .await
                .unwrap(),
            project.join("nested")
        );
    }

    #[tokio::test]
    async fn workspace_environment_missing_agent_root_does_not_block_explicit_directory() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        let other = temp.path().join("other-project");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        let workspace = TrackWorkspace {
            kind: TrackWorkspaceKind::Attached,
            path: project.display().to_string(),
            worktree: Some(temp.path().join("removed-agent-root").display().to_string()),
            frozen_at: Some(1),
        };
        assert_eq!(
            selected_source(&workspace, &other, None).await.unwrap(),
            other
        );
    }
}
