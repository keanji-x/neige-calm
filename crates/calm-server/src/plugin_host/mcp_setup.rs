pub use calm_types::mcp_connector::McpCheckResult;
pub async fn check(connector: super::ConnectorInstall) -> Result<McpCheckResult, (bool, String)> {
    plugin::host::mcp_setup::check(connector, &super::KERNEL_VERSION).await
}
