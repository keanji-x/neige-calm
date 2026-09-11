//! Public result of an unsaved remote MCP connection diagnostic.
use serde::Serialize;
use ts_rs::TS;
use utoipa::ToSchema;

#[derive(Serialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct McpCheckResult {
    pub tools: Vec<String>,
}
