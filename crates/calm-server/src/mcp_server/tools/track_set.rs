//! Planner selection of the current Track's working method.
use crate::mcp_server::{
    framing::{RpcError, calm_error},
    registry::{ToolDescriptor, ToolRegistry, role_gated_write_annotations},
    result::ToolResult,
};
use crate::model::CardRole;
use serde_json::{Value, json};

pub const TOOL_TRACK_SET: &str = "neige_track_set";

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    template_id: String,
    message: Option<String>,
}

pub fn register_into(registry: &mut ToolRegistry) {
    registry.register(ToolDescriptor {
        name: TOOL_TRACK_SET.into(),
        description: include_str!("../../../prompts/tools/neige_track_set.md").trim_end().into(),
        input_schema: json!({"type":"object", "additionalProperties":false, "required":["template_id"],
            "properties":{"template_id":{"type":"string", "minLength":1},"message":{"type":"string"}}}),
        annotations: Some(role_gated_write_annotations()),
        roles: &[CardRole::Planner], listed_for: &[CardRole::Planner],
    }, std::sync::Arc::new(|ctx, identity, args| Box::pin(async move {
        let args: Args = serde_json::from_value(args).map_err(|e| RpcError::invalid_params(e.to_string()))?;
        if args.template_id.trim().is_empty() { return Err(RpcError::invalid_params("template_id must not be blank")); }
        let service = ctx.track_creator.get().and_then(std::sync::Weak::upgrade)
            .ok_or_else(|| RpcError::internal("Track application service is unavailable"))?;
        let value: Value = service.set_template(identity, args.template_id, args.message).await.map_err(calm_error)?;
        Ok(ToolResult::structured(value))
    })));
}
