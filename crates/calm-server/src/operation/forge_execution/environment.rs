//! Forge owns project selection and GitHub cache isolation; the shared release owns framing.
use super::*;
use crate::operation::stdin_payload::{KeySource, PayloadKeys};
use crate::provider_execution::launch::Release as EnvironmentRelease;
use std::{
    ffi::{OsStr, OsString},
    os::unix::fs::DirBuilderExt,
    path::PathBuf,
};
#[cfg(test)]
mod tests;

/// Neige's database reads are retryable dependencies of preparing a Held release.
/// Project/source/contract refusals remain the inner payload result.
pub(super) fn payload_result<T>(result: Result<T>) -> Result<Result<T>> {
    match result {
        Err(error @ CalmError::Db(_)) => Err(error),
        result => Ok(result),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct WorkspaceEnvironment {
    source: PathBuf,
    cwd: PathBuf,
    keys: Vec<String>,
}

impl WorkspaceEnvironment {
    pub fn validate(&self) -> Result<()> {
        if !self.source.is_absolute()
            || !self.cwd.is_absolute()
            || !self.keys.windows(2).all(|pair| pair[0] < pair[1])
        {
            return Err(conflict("invalid forge workspace environment contract"));
        }
        let names = self.keys.iter().map(String::as_str).collect::<Vec<_>>();
        PayloadKeys::new(&names, KeySource::Kernel)?;
        Ok(())
    }

    pub fn read_prefix(&self) -> Result<String> {
        let names = self.keys.iter().map(String::as_str).collect::<Vec<_>>();
        Ok(PayloadKeys::new(&names, KeySource::Kernel)?
            .with_multiline()
            .read_prefix())
    }

    /// Re-evaluate values without persisting them. Action admission and all
    /// three kinds of post-park release share this preparation.
    pub async fn prepare<F>(
        ctx: &SpawnCtx,
        frozen: &forge::FrozenForge,
        execution: &ExecutionId,
        public: Vec<(String, String)>,
        expected: Option<&Self>,
        revalidate: impl FnOnce() -> F + Send,
    ) -> Result<Result<(Self, EnvironmentRelease)>>
    where
        F: std::future::Future<Output = Result<()>> + Send,
    {
        let (source, cwd) = match async {
            let source = crate::workspace_service::environment::source_for_card(
                ctx.repo.as_ref(),
                &frozen.card_id,
                &frozen.cwd_lease,
            )
            .await?;
            let source = tokio::fs::canonicalize(source).await?;
            let cwd = tokio::fs::canonicalize(&frozen.cwd_lease).await?;
            if let Some(expected) = expected
                && (source != expected.source || cwd != expected.cwd)
            {
                return Err(conflict("forge workspace environment source changed"));
            }
            Ok((source, cwd))
        }
        .await
        {
            Ok(paths) => paths,
            Err(error) => return payload_result(Err(error)),
        };
        let environment = crate::workspace_service::environment::resolve_for_directory_admitted(
            &source, revalidate,
        )
        .await?;
        let prepared = async {
            let mut project = environment
                .map_err(crate::workspace_service::environment::map_environment_error)?
                .into_values();
            let public = public
                .into_iter()
                .map(|(key, value)| (key, OsString::from(value)))
                .collect::<Vec<_>>();
            let original = public
                .iter()
                .map(|(key, value)| (key.clone().into(), value.clone()))
                .collect();
            // Keep owner control inputs, including the result path. All other
            // bootstrap values remain subject to the project's explicit unsets.
            crate::workspace_exec::restore_preserved(&mut project, &original, &[], &["NEIGE_"]);
            // Restore only explicit owner settings. Ambient defaults are not owner
            // policy and remain subject to every project's overrides and unsets.
            for (upper, lower, value) in
                crate::proxy_env::configured_proxy_env_pairs(ctx.repo.as_ref()).await?
            {
                project.insert(upper.into(), value.clone().into());
                project.insert(lower.into(), value.into());
            }
            let path = crate::kernel_bin_path::kernel_led_path_from(
                project.get(OsStr::new("PATH")).map(OsString::as_os_str),
            )?;
            project.insert("PATH".into(), path.path);
            let token_mode = [
                "GH_TOKEN",
                "GITHUB_TOKEN",
                "GH_ENTERPRISE_TOKEN",
                "GITHUB_ENTERPRISE_TOKEN",
            ]
            .iter()
            .any(|key| {
                project
                    .get(OsStr::new(key))
                    .is_some_and(|value| !value.is_empty())
            });
            if token_mode
                && project
                    .get(OsStr::new("GH_CONFIG_DIR"))
                    .is_none_or(|value| value.is_empty())
            {
                // A token for another host must not fall back to HOME's cached identity.
                // Reusing the execution id keeps this key present at admission and release.
                let mut empty = frozen.result_path.as_os_str().to_owned();
                empty.push(format!(".gh-config.{}", execution.as_str()));
                let empty = PathBuf::from(empty);
                std::fs::DirBuilder::new()
                    .mode(0o700)
                    .recursive(true)
                    .create(&empty)?;
                if std::fs::read_dir(&empty)?.next().is_some() {
                    return Err(conflict(
                        "forge temporary GitHub configuration is not empty",
                    ));
                }
                project.insert("GH_CONFIG_DIR".into(), empty.into_os_string());
            }
            let (release, keys) =
                EnvironmentRelease::with_private_names(public, project, None, "forge workspace")?;
            if let Some(expected) = expected
                && keys != expected.keys
            {
                return Err(conflict(
                    "forge workspace environment variables changed before release",
                ));
            }
            Ok((Self { source, cwd, keys }, release))
        }
        .await;
        payload_result(prepared)
    }
}
