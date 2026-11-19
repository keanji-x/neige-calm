//! Project tool environment, below explicit ACP registration and provider policy.
use super::host::Registration;
use crate::error::{CalmError, Result};
use std::{collections::BTreeMap, ffi::OsString, future::Future, path::Path};

pub(crate) type PrepareShell =
    fn(&Path, crate::workspace_service::environment::SourceMapping) -> Result<ShellEnvironment>;

/// One native process's metadata-only hook files and activation proof. Never reused after restart.
pub(crate) struct ShellEnvironment {
    pub directory: tempfile::TempDir,
    pub config: (&'static str, OsString),
}

impl ShellEnvironment {
    pub(crate) fn check_ready(&self) -> Result<()> {
        if std::fs::read(self.directory.path().join("ready"))
            .ok()
            .as_deref()
            != Some(b"ready".as_slice())
        {
            return Err(CalmError::Conflict(
                "ACP workspace shell plugin is not ready".into(),
            ));
        }
        Ok(())
    }
}

pub(crate) async fn track_values<F>(
    registration: Registration<'_>,
    repo: &dyn crate::db::RouteRepo,
    track_id: &str,
    cwd: &Path,
    agent_logs: &Path,
    revalidate: impl FnOnce() -> F + Send,
) -> Result<(BTreeMap<OsString, OsString>, Option<ShellEnvironment>)>
where
    F: Future<Output = Result<()>> + Send,
{
    if let Some(prepare) = registration.profile.workspace_shell {
        let mapping =
            crate::workspace_service::environment::source_mapping_for_track(repo, track_id).await?;
        let hook = prepare(agent_logs, mapping)?;
        return Ok((BTreeMap::new(), Some(hook)));
    }
    let environment =
        crate::workspace_service::environment::resolve_for_track(repo, track_id, cwd, revalidate)
            .await?
            .map_err(crate::workspace_service::environment::map_environment_error)?
            .into_values();
    Ok((environment, None))
}

pub(crate) fn directory_hook(
    registration: Registration<'_>,
    root: &Path,
    cwd: &Path,
) -> Result<Option<ShellEnvironment>> {
    registration
        .profile
        .workspace_shell
        .map(|prepare| {
            let cwd = std::fs::canonicalize(cwd)?;
            prepare(
                root,
                crate::workspace_service::environment::SourceMapping {
                    source: cwd.clone(),
                    derived_roots: vec![cwd],
                },
            )
        })
        .transpose()
}

pub(crate) fn project_values(
    registration: Registration<'_>,
    values: BTreeMap<OsString, OsString>,
) -> Result<BTreeMap<String, OsString>> {
    const AUTH_HOME_KEYS: &[&str] = &["HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME"];
    let profile = registration.profile;
    // Native shell hooks resolve project variables only inside tool invocations.
    // The model/config process never receives even unrecognized project names.
    let values = if profile.workspace_shell.is_some() {
        crate::workspace_service::environment::directory_base_environment()
    } else {
        values
    };
    let mut result = values
        .into_iter()
        .filter_map(|(key, value)| {
            let Some(key) = key.to_str() else {
                return Some(Err(CalmError::BadRequest(
                    "ACP workspace environment requires UTF-8 names".into(),
                )));
            };
            // Provider authentication and control stay operator/profile-owned. Derive this
            // boundary from the same declarations that validate operator registrations.
            if key.starts_with("NEIGE_")
                || AUTH_HOME_KEYS.contains(&key)
                || profile.owned_env.contains(&key)
                || profile.workspace_blocked_env.contains(&key)
                || profile
                    .authentication
                    .iter()
                    .any(|method| method.refused_env.contains(&key))
            {
                return None;
            }
            Some(Ok((key.to_owned(), value)))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    // Provider sign-in/configuration stays in its baseline account directories.
    // Explicit registrations are merged later and retain operator authority.
    result.extend(
        AUTH_HOME_KEYS
            .iter()
            .filter_map(|key| std::env::var_os(key).map(|value| ((*key).to_owned(), value))),
    );
    Ok(result)
}

pub(crate) fn kernel_path(values: &mut BTreeMap<String, OsString>) -> Result<()> {
    let inherited = values.remove("PATH");
    let path = crate::kernel_bin_path::kernel_led_path_from(inherited.as_deref())?;
    values.insert("PATH".into(), path.path);
    Ok(())
}
