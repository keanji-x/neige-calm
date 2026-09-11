//! Unsaved connection diagnostics. This module has no database or filesystem
//! write capability; it uses the same manifest/secret validation as install.
use super::{
    ConnectorInstall, HttpCredential, HttpMcpClient, connector, http_headers::HttpHeaders, manifest,
};
use serde::Serialize;
use std::sync::Arc;
use utoipa::ToSchema;

#[derive(Serialize, ToSchema)]
pub struct McpCheckResult {
    pub tools: Vec<String>,
}

pub async fn check(connector: ConnectorInstall) -> Result<McpCheckResult, (bool, String)> {
    let (manifest, _) = connector.prepare().map_err(|why| (false, why))?;
    let block = manifest.mcp_http.as_ref().expect("connector manifest");
    let url = manifest::resolve_mcp_http_url(block, &serde_json::Map::new())
        .map_err(|why| (false, why))?;
    let key = connector
        .credential()
        .map(HttpCredential::parse)
        .transpose()
        .map_err(|why| (false, why))?;
    let headers = HttpHeaders::parse(connector.headers).map_err(|why| (false, why))?;
    let client =
        Arc::new(HttpMcpClient::new(&manifest.id, &url, block, key.as_ref()).with_headers(headers));
    let discovery = async {
        // Same best-effort initialize policy as enable, followed by the actual
        // tools/list request. A successful Check never claims business calls.
        let _ = client.initialize().await;
        client.tools_list().await
    };
    let upstream = tokio::time::timeout(super::connector_bringup_budget(&manifest), discovery)
        .await
        .map_err(|_| (true, "Connection check timed out".to_string()))?
        .map_err(|why| (true, why.to_string()))?;
    // Check reports the available catalog even when an allowlist is selected.
    let mut all = block.clone();
    all.tools_all = true;
    all.tools_allow.clear();
    let tools = connector::materialize_http_tools(&manifest.id, &all, &upstream);
    Ok(McpCheckResult {
        tools: tools.into_iter().map(|tool| tool.name).collect(),
    })
}
