//! Kernel MCP configuration adapter for the provider-owned shared Codex HOME.
use crate::mcp_server::{
    McpShimConfig,
    wiring::{MCP_SERVER_KEY, MCP_TOOLSET_ENV, daemon_shim_env},
};
pub use provider::codex::shared::home::ConfigTomlModelDefaults;
use std::{collections::BTreeSet, io, ops::Deref, path::PathBuf};
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
    /// The kernel entry's catalog refresh: bump its generation and make its per-tool approval
    /// map exactly `delegated_tools`, the minted names of the installed and enabled plugins'
    /// delegated tools. The kernel's per-role terminal grants travel in each thread's start config
    /// instead (`card_mcp_thread_start_config`), and Codex merges the two maps key by key.
    pub fn refresh_mcp_entry(&self, delegated_tools: &BTreeSet<String>) -> io::Result<u64> {
        self.0
            .refresh_mcp_entry(MCP_SERVER_KEY, MCP_TOOLSET_ENV, delegated_tools)
    }
}
