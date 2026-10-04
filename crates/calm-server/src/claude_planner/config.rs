//! The Claude Planner's typed configuration (design #1791 §5.3, D14, D18) and the pre-spawn
//! `--version` check that keeps the pinned binary the one every release-gate result was recorded
//! against.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use super::spawn::instructions_dir;
use super::stop::MarkerInstance;
use super::translate::ToolNames;
use crate::error::{CalmError, Result};
use crate::harness::backend::TurnStartFailure;
use crate::model::CardRole;

/// How long `<claude_binary> --version` may take; it answers in milliseconds.
const VERSION_TIMEOUT: Duration = Duration::from_secs(10);

/// The file named by `--claude-planner-config`; absent keeps the Claude Planner unavailable.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaudePlannerConfig {
    /// A versioned binary, never the auto-updated `~/.local/bin/claude` symlink.
    pub claude_binary: PathBuf,
    /// Must equal the first token `<claude_binary> --version` prints before any input is written.
    pub claude_version: String,
    /// The dedicated `CLAUDE_CONFIG_DIR`; the owner runs `/login` in it.
    pub config_dir: PathBuf,
}

impl ClaudePlannerConfig {
    pub fn read(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path).map_err(|error| {
            CalmError::BadRequest(format!("claude planner config {}: {error}", path.display()))
        })?;
        serde_json::from_slice(&bytes).map_err(|error| {
            CalmError::BadRequest(format!("claude planner config {}: {error}", path.display()))
        })
    }

    /// Run `<claude_binary> --version` and refuse unless its first whitespace token equals
    /// `claude_version` (the CLI prints `2.1.280 (Claude Code)`). `env` is the spawn's own
    /// allowlisted environment, so the check runs the binary exactly as the turn will.
    pub async fn verify_version(&self, env: &[(String, std::ffi::OsString)]) -> Result<()> {
        self.version_problem(env).await.map_or(Ok(()), |problem| {
            Err(CalmError::Conflict(problem.to_string()))
        })
    }

    /// [`Self::verify_version`] for a turn start (#1981): a binary that answered as another version
    /// is refused until an operator fixes it; one that did not answer may answer next time.
    pub async fn refuse_unless_pinned(
        &self,
        env: &[(String, std::ffi::OsString)],
    ) -> Result<(), TurnStartFailure> {
        match self.version_problem(env).await {
            None => Ok(()),
            Some(VersionProblem::NotPinned(problem)) => Err(needs_an_operator(problem)),
            Some(VersionProblem::NoAnswer(problem)) => Err(CalmError::Conflict(problem).into()),
        }
    }

    /// [`Self::verify_version`]'s check, answering why the binary is refused (`None` = it is not).
    /// Runs through [`super::readiness_command::run`]: bounded, capped, killed and reaped.
    pub async fn version_problem(
        &self,
        env: &[(String, std::ffi::OsString)],
    ) -> Option<VersionProblem> {
        let (status, stdout) = match super::readiness_command::run(
            &self.claude_binary,
            &["--version"],
            env,
            VERSION_TIMEOUT,
        )
        .await
        {
            Ok(output) => output,
            Err(failure) => {
                let detail = failure.to_string();
                return Some(VersionProblem::NoAnswer(version_error(
                    &self.claude_binary,
                    &detail,
                )));
            }
        };
        if !status.success() {
            return Some(VersionProblem::NotPinned(version_error(
                &self.claude_binary,
                &format!("exited with {status}"),
            )));
        }
        let printed = String::from_utf8_lossy(&stdout);
        match printed.split_whitespace().next() {
            Some(first) if first == self.claude_version => None,
            other => Some(VersionProblem::NotPinned(version_error(
                &self.claude_binary,
                &format!(
                    "reports {:?}, the config pins {:?}",
                    other.unwrap_or(""),
                    self.claude_version
                ),
            ))),
        }
    }
}

/// Why `--version` refuses the binary; `Display` is the reason.
#[derive(Debug, thiserror::Error)]
pub enum VersionProblem {
    /// It answered, and the answer is not the pinned version (or it exited unsuccessfully).
    #[error("{0}")]
    NotPinned(String),
    /// No answer was obtained: it did not start, could not be read or waited for, timed out, or
    /// printed past the cap.
    #[error("{0}")]
    NoAnswer(String),
}

/// The flag whose absence keeps the Claude Planner unavailable (design #1791 §5.3).
pub const CONFIG_FLAG: &str = "--claude-planner-config";

/// What every Claude Planner of this server shares: the typed config (absent without
/// [`CONFIG_FLAG`]), the marker instance and the instructions directory derived from `data_dir`,
/// the MCP shim and socket, and the calm tools a Planner card sees. The marker instance exists
/// without the config, so the boot sweep and every retirement `stop` work either way.
#[derive(Debug)]
pub struct ClaudePlannerHost {
    config: Option<ClaudePlannerConfig>,
    pub instance: MarkerInstance,
    pub instructions_dir: PathBuf,
    pub mcp_shim: PathBuf,
    pub mcp_socket: PathBuf,
    pub calm_tools: ToolNames,
    /// Owns the data dir of [`Self::unconfigured_scratch`].
    _scratch: Option<tempfile::TempDir>,
}

impl ClaudePlannerHost {
    pub fn new(
        config: Option<ClaudePlannerConfig>,
        data_dir: &Path,
        mcp_shim: PathBuf,
        mcp_socket: PathBuf,
    ) -> Result<Self> {
        let calm_tools = ToolNames::new(
            crate::mcp_server::wiring::MCP_SERVER_KEY,
            crate::mcp_server::build_default_registry()
                .descriptors_for_role(CardRole::Planner)
                .into_iter()
                .map(|descriptor| descriptor.name),
        );
        Ok(Self {
            config,
            instance: MarkerInstance::for_data_dir(data_dir)?,
            instructions_dir: instructions_dir(data_dir)?,
            mcp_shim,
            mcp_socket,
            calm_tools,
            _scratch: None,
        })
    }

    /// A host with no config over a private temporary data dir, for runtimes assembled without a
    /// data dir (`AppState::from_parts`, the dispatcher's own runtime): it never spawns, and its
    /// marker instance matches no other calm-server's processes.
    pub fn unconfigured_scratch() -> Result<Self> {
        let scratch = tempfile::Builder::new()
            .prefix("calm-claude-planner-")
            .tempdir()?;
        let mut host = Self::new(
            None,
            scratch.path(),
            PathBuf::from("neige-mcp-stdio-shim"),
            scratch.path().join("mcp.sock"),
        )?;
        host._scratch = Some(scratch);
        Ok(host)
    }

    /// The config, or the refusal that names [`CONFIG_FLAG`].
    pub fn configured(&self) -> Result<&ClaudePlannerConfig> {
        self.config
            .as_ref()
            .ok_or_else(|| CalmError::Conflict(unavailable_message()))
    }

    /// The readiness preflight (§4.1 row 11): configured, and the pinned binary answers
    /// `--version` with the pinned version.
    pub async fn check_ready(&self) -> Result<()> {
        let config = self.configured()?;
        config.verify_version(&self.readiness_env(config)?).await
    }

    /// The environment of every readiness command (`--version`, `auth status`, the model list): the spawn's own
    /// allowlist under the `readiness` marker, without the MCP token.
    pub(crate) fn readiness_env(
        &self,
        config: &ClaudePlannerConfig,
    ) -> Result<Vec<(String, std::ffi::OsString)>> {
        Ok(super::spawn::base_env(&super::spawn::EnvInputs {
            path: crate::kernel_bin_path::kernel_led_path()?.path,
            config_dir: &config.config_dir,
            mcp_socket: &self.mcp_socket,
            marker: self.instance.marker("readiness"),
            proxy: &[],
        }))
    }
}

/// What a reader and a refused caller are told while [`CONFIG_FLAG`] is absent.
pub fn unavailable_message() -> String {
    format!("the Claude Planner is unavailable: calm-server was started without {CONFIG_FLAG}")
}

/// A turn-start refusal only an operator can clear (#1981): the reader is told `reason` now, and
/// the paced retry sends the queued message once the fix lands.
pub(super) fn needs_an_operator(reason: String) -> TurnStartFailure {
    TurnStartFailure::Refused {
        reader: format!(
            "claude will not start a turn for this conversation: {reason}. Your message is still \
             queued and will be sent once that is fixed."
        ),
        error: CalmError::Conflict(reason),
    }
}

fn version_error(binary: &Path, detail: &str) -> String {
    format!(
        "claude planner binary {} --version {detail}",
        binary.display()
    )
}

#[cfg(test)]
mod tests {
    use super::ClaudePlannerConfig;

    const FULL: &str =
        r#"{"claude_binary":"/v/2.1.280","claude_version":"2.1.280","config_dir":"/c"}"#;

    #[test]
    fn the_three_required_fields_parse() {
        let config: ClaudePlannerConfig = serde_json::from_str(FULL).expect("parse");
        assert_eq!(config.claude_version, "2.1.280");
    }

    #[test]
    fn an_unknown_field_is_refused() {
        let with_extra = FULL.replace('}', r#","claude_bin":"/x"}"#);
        let error = serde_json::from_str::<ClaudePlannerConfig>(&with_extra).unwrap_err();
        assert!(error.to_string().contains("unknown field"), "{error}");
    }

    #[test]
    fn each_missing_required_field_is_refused() {
        for field in ["claude_binary", "claude_version", "config_dir"] {
            let mut value: serde_json::Value = serde_json::from_str(FULL).unwrap();
            value.as_object_mut().unwrap().remove(field);
            let error = serde_json::from_value::<ClaudePlannerConfig>(value).unwrap_err();
            assert!(error.to_string().contains(field), "{field}: {error}");
        }
    }
}
