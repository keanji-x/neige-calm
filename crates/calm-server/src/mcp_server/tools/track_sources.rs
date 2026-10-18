//! Planner read port for the shared creation catalog; no borrowed REST credentials.
use crate::error::{CalmError, Result};
use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{AppContext, ToolDescriptor, ToolRegistry};
use crate::mcp_server::result::ToolResult;
use crate::model::CardRole;
pub use crate::routes::track_catalog::{GetSource, ListSources};
use serde_json::{Value, json};
use std::sync::Arc;

pub const TOOL_SOURCES_LIST: &str = "neige_catalog_ls";
pub const TOOL_SOURCE_GET: &str = "neige_catalog_cat";

#[async_trait::async_trait]
pub trait TrackCatalog: Send + Sync {
    async fn list_sources(&self, request: ListSources) -> Result<Value>;
    async fn get_source(&self, request: GetSource) -> Result<Value>;
}

fn rpc(error: CalmError) -> RpcError {
    match error {
        CalmError::BadRequest(message) => RpcError::invalid_params(message),
        CalmError::Conflict(message) => RpcError::conflict(message),
        CalmError::NotFound(message) => RpcError::not_found(message),
        other => RpcError::internal(other.to_string()),
    }
}

pub fn register_into(registry: &mut ToolRegistry) {
    for (name, description, required, properties) in [
        (
            TOOL_SOURCES_LIST,
            include_str!("../../../prompts/tools/neige_catalog_ls.md"),
            json!(["kind", "limit"]),
            json!({
                "kind": {"type": "string", "enum": ["template", "recipe"]},
                "limit": {"type": "integer", "minimum": 1, "maximum": 50},
                "cursor": {"type": "string"}
            }),
        ),
        (
            TOOL_SOURCE_GET,
            include_str!("../../../prompts/tools/neige_catalog_cat.md"),
            json!(["kind", "source_id"]),
            json!({
                "kind": {"type": "string", "enum": ["template", "recipe"]}, "source_id": {"type": "string"}
            }),
        ),
    ] {
        registry.register(ToolDescriptor {
            name: name.into(), description: description.trim_end().into(),
            input_schema: json!({"type": "object", "required": required, "properties": properties, "additionalProperties": false}),
            annotations: Some(json!({"readOnlyHint": true, "destructiveHint": false, "openWorldHint": false})),
            roles: &[CardRole::Planner], listed_for: &[CardRole::Planner],
        }, Arc::new(move |ctx: Arc<AppContext>, _, args| Box::pin(async move {
            if args.get("cursor").is_some_and(Value::is_null) {
                return Err(RpcError::invalid_params("cursor must not be null"));
            }
            let service = ctx.track_creator.get().and_then(std::sync::Weak::upgrade)
                .ok_or_else(|| RpcError::internal("track application service is unavailable"))?;
            let value = if name == TOOL_SOURCES_LIST {
                service.list_sources(serde_json::from_value(args).map_err(|e| RpcError::invalid_params(e.to_string()))?).await
            } else {
                service.get_source(serde_json::from_value(args).map_err(|e| RpcError::invalid_params(e.to_string()))?).await
            }.map_err(rpc)?;
            Ok(ToolResult::structured(value))
        })));
    }
}
