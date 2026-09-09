//! Couple tool authorization metadata to the actual client generation.
use super::manifest::{ConnectorKind, ExposedTool};
use super::{ConnectorClient, HostError, PluginHost, PluginRuntimeStatus};

pub(crate) struct PluginToolSnapshot {
    pub kind: ConnectorKind,
    pub tool: ExposedTool,
    pub client: ConnectorClient,
}

impl PluginHost {
    /// Capture metadata and a concrete client while reload/stop cannot replace
    /// either. No lock is kept across the RPC: the returned Arc pins the chosen
    /// client, so a concurrent reload may close it but cannot redirect the call
    /// to a new generation with different authorization.
    ///
    /// Callers resolve a known route and Track scope before entering here. The
    /// lock order remains lifecycle -> process table -> registry.
    pub(crate) fn tool_call_snapshot(
        &self,
        id: &str,
        tool_name: &str,
    ) -> Result<Option<PluginToolSnapshot>, HostError> {
        let guard = self.try_lock_lifecycle(id)?;
        let table = self.lock_table();
        let Some(running) = table
            .live
            .get(guard.id())
            .filter(|running| matches!(running.status, PluginRuntimeStatus::Running))
        else {
            return Ok(None);
        };
        let Some(client) = running.mcp.clone() else {
            return Ok(None);
        };
        let Some(manifest) = self.registry.get(guard.id()) else {
            return Ok(None);
        };
        // Fail closed if a future writer breaks the host's kind/client pairing.
        let kind = match &client {
            ConnectorClient::Stdio(_) => ConnectorKind::App,
            ConnectorClient::Http(_) => ConnectorKind::McpHttp,
            ConnectorClient::Cli(_) => ConnectorKind::CliQuery,
        };
        if kind != manifest.kind {
            return Ok(None);
        }
        let Some(tool) = manifest
            .exposes_tools
            .into_iter()
            .find(|tool| tool.name == tool_name)
        else {
            return Ok(None);
        };
        Ok(Some(PluginToolSnapshot { kind, tool, client }))
    }
}
