//! Terminal-owned project environment preparation and the released v1 go-only contract.
use super::*;
use crate::operation::stdin_payload::{KeySource, PayloadKeys};
use crate::provider_execution::launch::Release as EnvironmentRelease;
use std::{
    ffi::{OsStr, OsString},
    path::Path,
};

#[derive(Clone, PartialEq)]
pub(super) struct Selection {
    pub track_id: String,
    pub cwd: String,
    workspace: crate::model::TrackWorkspace,
}
impl Selection {
    pub async fn capture(tx: &mut Tx<'_>, input: &Value) -> Result<Self> {
        let payload = TerminalWorkerAdapter::admission_fences_tx(tx, input).await?;
        let cwd = terminal_cwd_or_track_workspace(
            tx,
            &payload.track_id,
            explicit_terminal_cwd(payload.cwd),
        )
        .await?;
        let workspace = crate::db::sqlite::track_workspace_read_tx(tx, &payload.track_id).await?;
        Ok(Self {
            track_id: payload.track_id,
            cwd,
            workspace,
        })
    }
}

/// V1 project-environment metadata. Absence is accepted only for the released
/// six-field preparation (plus its two known lifecycle-owned additions).
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Contract {
    version: u8,
    pub cwd: String,
    pub source: String,
    keys: Vec<String>,
}
impl Contract {
    pub fn new(cwd: String, source: &Path, keys: Vec<String>) -> Result<Self> {
        let source = source
            .to_str()
            .ok_or_else(|| {
                CalmError::BadRequest("workspace environment source requires UTF-8".into())
            })?
            .to_owned();
        Ok(Self {
            version: 1,
            cwd,
            source,
            keys,
        })
    }

    pub fn read(output: &TxOutput) -> Result<Option<Self>> {
        if let Some(value) = output.data.get("workspace_environment") {
            let contract: Self = serde_json::from_value(value.clone())?;
            if contract.version != 1
                || !Path::new(&contract.source).is_absolute()
                || contract.cwd != output.output_string("cwd", "terminal environment")?
                || !contract.keys.windows(2).all(|pair| pair[0] < pair[1])
            {
                return Err(CalmError::Conflict(
                    "terminal workspace environment contract changed".into(),
                ));
            }
            contract.with_payload_keys(|_| Ok(()))?;
            return Ok(Some(contract));
        }
        let data = output
            .data
            .as_object()
            .ok_or_else(|| CalmError::Conflict("invalid legacy terminal preparation".into()))?;
        let required = [
            "card_id",
            "track_id",
            "cmd",
            "cwd",
            "env",
            "terminal_worker_pipe",
        ];
        if !required.iter().all(|key| data.contains_key(*key))
            || data.keys().any(|key| {
                !required.contains(&key.as_str()) && key != KEY && key != "terminal_launch"
            })
        {
            return Err(CalmError::Conflict(
                "unknown terminal environment preparation".into(),
            ));
        }
        Ok(None)
    }

    pub fn with_payload_keys<T>(
        &self,
        use_keys: impl FnOnce(PayloadKeys<'_>) -> Result<T>,
    ) -> Result<T> {
        let names = self.keys.iter().map(String::as_str).collect::<Vec<_>>();
        use_keys(PayloadKeys::new(&names, KeySource::Kernel)?.with_multiline())
    }

    pub async fn release<F>(
        &self,
        repo: &dyn crate::db::RouteRepo,
        output: &TxOutput,
        paths: &Prepared,
        revalidate: impl FnOnce() -> F + Send,
    ) -> Result<Result<Vec<u8>>>
    where
        F: std::future::Future<Output = Result<()>> + Send,
    {
        let source = match async {
            let card = output.output_string("card_id", "terminal environment")?;
            let source = crate::workspace_service::environment::source_for_card(
                repo,
                &card,
                Path::new(&self.cwd),
            )
            .await?;
            let source = tokio::fs::canonicalize(source).await?;
            if source != Path::new(&self.source) {
                return Err(CalmError::Conflict(
                    "terminal workspace environment source changed".into(),
                ));
            }
            Ok(source)
        }
        .await
        {
            Ok(source) => source,
            // These Neige reads precede any release bytes; dependency recovery can retry Held.
            Err(error @ CalmError::Db(_)) => return Err(error),
            Err(error) => return Ok(Err(error)),
        };
        let environment = resolve(&source, &output.data["env"], paths, revalidate).await?;
        Ok(environment.and_then(|(release, keys)| {
            if keys != self.keys {
                return Err(CalmError::Conflict(
                    "terminal workspace environment variables changed before release".into(),
                ));
            }
            Ok(release.payload)
        }))
    }
}

pub(super) async fn resolve<F>(
    source: &Path,
    env: &Value,
    paths: &Prepared,
    revalidate: impl FnOnce() -> F + Send,
) -> Result<Result<(EnvironmentRelease, Vec<String>)>>
where
    F: std::future::Future<Output = Result<()>> + Send,
{
    let environment =
        crate::workspace_service::environment::resolve_for_directory_admitted(source, revalidate)
            .await?;
    Ok((|| {
        let mut project = environment
            .map_err(crate::workspace_service::environment::map_environment_error)?
            .into_values();
        // Terminal and explicitly configured proxy inputs keep their existing owner.
        for (key, value) in crate::routes::terminal::terminal_child_envs(env) {
            project.insert(key.into(), value.into());
        }
        let path = crate::kernel_bin_path::kernel_led_path_from(
            project.get(OsStr::new("PATH")).map(OsString::as_os_str),
        )?;
        project.insert("PATH".into(), path.path);
        for (key, path) in [
            ("NEIGE_GATE_LOG_PATH", &paths.log_path),
            ("NEIGE_GATE_EXIT_PATH", &paths.exit_path),
            ("NEIGE_GATE_STEP_PATH", &paths.step_path),
        ] {
            project.insert(key.into(), path.as_os_str().to_owned());
        }
        let public = super::environment(env)?
            .into_iter()
            .map(|(key, value)| (key, value.into()))
            .collect();
        EnvironmentRelease::with_private_names(public, project, None, "terminal worker")
    })())
}
