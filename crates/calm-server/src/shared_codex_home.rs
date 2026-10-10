//! Kernel MCP configuration adapter for the provider-owned shared Codex HOME.
use crate::mcp_server::{
    McpShimConfig,
    wiring::{MCP_SERVER_KEY, MCP_TOOLSET_ENV, daemon_shim_env},
};
pub use provider::codex::shared::home::ConfigTomlModelDefaults;
use std::{io, ops::Deref, path::PathBuf};
pub const EXPECTED_MCP_SERVERS: &[&str] = &[MCP_SERVER_KEY];

pub struct SharedCodexHome(provider::codex::shared::home::SharedCodexHome);
impl Deref for SharedCodexHome {
    type Target = provider::codex::shared::home::SharedCodexHome;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl SharedCodexHome {
    pub fn new(home: PathBuf, legacy_homes_parent: PathBuf) -> Self {
        Self(provider::codex::shared::home::SharedCodexHome::new(
            home,
            legacy_homes_parent,
        ))
    }
    pub fn ensure_daemon_mcp_config(
        &self,
        shim: &McpShimConfig,
        daemon_token: &str,
    ) -> io::Result<()> {
        let env = daemon_shim_env(&shim.socket_path, daemon_token)
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect::<Vec<_>>();
        self.0
            .ensure_config(Some(&provider::codex::shared::home::McpServerConfig {
                key: MCP_SERVER_KEY,
                command: &shim.shim_bin,
                env: &env,
            }))
    }
    pub fn bump_mcp_toolset(&self) -> io::Result<u64> {
        self.0.bump_mcp_toolset(MCP_SERVER_KEY, MCP_TOOLSET_ENV)
    }
}
