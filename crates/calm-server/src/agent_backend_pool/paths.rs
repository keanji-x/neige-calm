//! The runtime path owner. Keep configured bytes separate from resolved identity.
use super::BackendId;
use anyhow::Context;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendPaths {
    pub data: PathBuf,
    pub home: PathBuf,
    pub run: PathBuf,
    pub state: PathBuf,
    pub socket: PathBuf,
    pub authentication: PathBuf,
    pub logs: PathBuf,
    pub kernel_mcp_socket: PathBuf,
}
impl BackendPaths {
    pub fn implicit_home(data: &Path) -> PathBuf {
        data.join("codex-home")
    }

    pub fn default_logs(data: &Path) -> PathBuf {
        data.join("logs/shared-codex-appserver")
    }

    pub fn legacy_home(&self) -> PathBuf {
        Self::implicit_home(&self.data)
    }

    pub fn new(data: &Path, id: &BackendId, home: &Path, default_logs: &Path) -> Self {
        let (socket, authentication, logs) = if id.as_str() == "default" {
            (
                calm_types::runtime_paths::shared_codex_appserver_socket(data),
                data.join("state/codex-authentication.json"),
                default_logs.to_owned(),
            )
        } else {
            (
                data.join(format!("run/codex-appserver-{}.sock", id.as_str())),
                data.join(format!("state/codex-authentication-{}.json", id.as_str())),
                data.join(format!("logs/shared-codex-appserver-{}", id.as_str())),
            )
        };
        Self {
            data: data.to_owned(),
            home: home.to_owned(),
            run: socket.parent().expect("socket has a parent").to_owned(),
            state: authentication
                .parent()
                .expect("checkpoint has a parent")
                .to_owned(),
            socket,
            authentication,
            logs,
            kernel_mcp_socket: crate::mcp_server::transport::default_socket_path(data),
        }
    }

    pub(crate) fn namespaces(&self) -> [&Path; 8] {
        [
            &self.data,
            &self.home,
            &self.run,
            &self.state,
            &self.socket,
            &self.authentication,
            &self.logs,
            &self.kernel_mcp_socket,
        ]
    }

    // run/state and the kernel MCP socket are intentionally shared by backends.
    // They still must not enter any other backend's credential home.
    pub(crate) fn derived_namespaces(&self) -> [&Path; 6] {
        [
            &self.run,
            &self.state,
            &self.socket,
            &self.authentication,
            &self.logs,
            &self.kernel_mcp_socket,
        ]
    }

    pub(crate) fn private_namespaces(&self) -> [&Path; 3] {
        [&self.socket, &self.authentication, &self.logs]
    }

    /// Resolve from the runtime spellings on every entry, including absent leaves.
    pub(crate) fn resolved(&self) -> anyhow::Result<Self> {
        Ok(Self {
            data: resolved_path(&self.data)?,
            home: resolved_path(&self.home)?,
            run: resolved_path(&self.run)?,
            state: resolved_path(&self.state)?,
            socket: resolved_path(&self.socket)?,
            authentication: resolved_path(&self.authentication)?,
            logs: resolved_path(&self.logs)?,
            kernel_mcp_socket: resolved_path(&self.kernel_mcp_socket)?,
        })
    }
}

pub(crate) fn resolved_path(path: &Path) -> anyhow::Result<PathBuf> {
    resolved_absolute_path(&std::path::absolute(path)?)
}

fn resolved_absolute_path(path: &Path) -> anyhow::Result<PathBuf> {
    if path.exists() || std::fs::symlink_metadata(path).is_ok() {
        return Ok(path.canonicalize()?);
    }
    let parent = path.parent().context("derived path has no parent")?;
    Ok(resolved_absolute_path(parent)?.join(path.file_name().context("derived path has no name")?))
}
