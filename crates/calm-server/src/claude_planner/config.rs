//! The Claude Planner's typed configuration (design #1791 §5.3, D14, D18) and the pre-spawn
//! `--version` check that keeps the pinned binary the one every release-gate result was recorded
//! against.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::Deserialize;

use super::spawn::instructions_dir;
use super::stop::MarkerInstance;
use crate::error::{CalmError, Result};

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
        let mut command = tokio::process::Command::new(&self.claude_binary);
        command
            .arg("--version")
            .env_clear()
            .envs(env.iter().map(|(key, value)| (key, value)))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let output = tokio::time::timeout(VERSION_TIMEOUT, command.output())
            .await
            .map_err(|_| version_error(&self.claude_binary, "timed out"))?
            .map_err(|error| version_error(&self.claude_binary, &error.to_string()))?;
        if !output.status.success() {
            return Err(version_error(
                &self.claude_binary,
                &format!("exited with {}", output.status),
            ));
        }
        let printed = String::from_utf8_lossy(&output.stdout);
        match printed.split_whitespace().next() {
            Some(first) if first == self.claude_version => Ok(()),
            other => Err(version_error(
                &self.claude_binary,
                &format!(
                    "reports {:?}, the config pins {:?}",
                    other.unwrap_or(""),
                    self.claude_version
                ),
            )),
        }
    }
}

/// What every Claude Planner of this server shares: the typed config, the marker instance and the
/// instructions directory derived from `data_dir`, and the MCP shim and socket.
#[derive(Debug)]
pub struct ClaudePlannerHost {
    pub config: ClaudePlannerConfig,
    pub instance: MarkerInstance,
    pub instructions_dir: PathBuf,
    pub mcp_shim: PathBuf,
    pub mcp_socket: PathBuf,
}

impl ClaudePlannerHost {
    pub fn new(
        config: ClaudePlannerConfig,
        data_dir: &Path,
        mcp_shim: PathBuf,
        mcp_socket: PathBuf,
    ) -> Result<Self> {
        Ok(Self {
            config,
            instance: MarkerInstance::for_data_dir(data_dir)?,
            instructions_dir: instructions_dir(data_dir)?,
            mcp_shim,
            mcp_socket,
        })
    }
}

fn version_error(binary: &Path, detail: &str) -> CalmError {
    CalmError::Conflict(format!(
        "claude planner binary {} --version {detail}",
        binary.display()
    ))
}
