//! One named independent task through the Planner report writer.
use crate::decision_sink::CardDecisionSink;
use crate::error::CalmError;
use crate::mcp_server::{framing::RpcError, registry::*};
use crate::model::CardRole;
use crate::track_report::dispatch::DispatchArgs;
use serde_json::{Value, json};
use std::sync::Arc;

pub const TOOL_TASK_DISPATCH: &str = "calm.task.dispatch";

pub fn register_into(registry: &mut ToolRegistry) {
    registry.register(ToolDescriptor {
        name: TOOL_TASK_DISPATCH.into(),
        description: include_str!("../../../prompts/tools/calm.task.dispatch.md").trim_end().to_string(),
        input_schema: json!({
            "type":"object", "additionalProperties":false,
            "required":["name","goal","acceptance","executor","workspace"],
            "properties": {
                "name":{"type":"string","minLength":1,"description":"Nonempty after trim, at most 200 UTF-8 bytes, no control characters; immutable Track-local Dispatch identity."},
                "goal":{"type":"string","minLength":1},
                "acceptance":{"type":"string","minLength":1},
                "executor":{"type":"string","enum":["codex"]},
                "plugin_tools":{"type":"array","maxItems":32,"uniqueItems":true,"items":{"type":"string","minLength":1,"maxLength":256},"description":"plugin.<id>_<tool> names delegated to the Worker through platform MCP; the Codex-sanitized spelling from your tool list ([^A-Za-z0-9_] shown as _) is accepted and resolved to the registry name. Omitted means no plugin grants. Frozen for replay and recovery; current Track scope and plugin availability still apply. Ordinary tools only, no ForgeAction tools or wildcards."},
                "workspace":{"type":"string","enum":["empty","verified-candidate"]},
                "input":{"type":"object","additionalProperties":false,"required":["producer","slot"],
                    "properties":{
                        "producer":{"type":"string","pattern":"^[a-z0-9][a-z0-9._-]{0,63}$","not":{"pattern":"[^a-z0-9._-]"},"description":"Exact same-Track candidate producer key; use repair_key for C2."},
                        "slot":{"type":"string","pattern":"^[A-Za-z0-9_-]{1,128}$","not":{"pattern":"[^A-Za-z0-9_-]"}}
                    }}
            },
            "oneOf":[
                {"properties":{"workspace":{"const":"empty"}},"not":{"required":["input"]}},
                {"properties":{"workspace":{"const":"verified-candidate"}},"required":["input"]}
            ]
        }),
        annotations: Some(role_gated_write_annotations()),
        visible_to_roles: &[CardRole::Planner],
    }, Arc::new(|ctx, identity, args| Box::pin(async move {
        dispatch(ctx, identity, args).await.map(crate::mcp_server::result::ToolResult::structured)
    })));
}

async fn dispatch(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Planner)?;
    let args: DispatchArgs =
        serde_json::from_value(args).map_err(|e| RpcError::invalid_params(e.to_string()))?;
    let (track, _, card, payload) =
        super::track_report::resolve_report_for_caller(&ctx, &identity).await?;
    // #1668 — the model only sees Codex-sanitized spellings; resolve them to
    // registry names before validation so the frozen contract (and its
    // replay lookup) carries what the Worker side matches exactly.
    let (plugin_tools, admission) = crate::mcp_server::transport::resolve_dispatch_plugin_tools(
        &ctx,
        identity.track_id.as_deref(),
        args.plugin_tools(),
    )
    .await?;
    let args = args
        .with_plugin_tools(plugin_tools)
        .normalize()
        .map_err(map_error)?;
    CardDecisionSink::from_app_context(&ctx)
        .commit_task_dispatch(
            &identity,
            track,
            card,
            payload,
            args,
            admission,
            ctx.task_budget_default,
        )
        .await
        .map_err(map_error)
}

fn map_error(error: CalmError) -> RpcError {
    match error {
        CalmError::BadRequest(m) => RpcError::invalid_params(m),
        CalmError::Forbidden(m) => RpcError::custom(-32403, m),
        CalmError::Conflict(m) => RpcError::custom(-32409, m),
        other => RpcError::internal(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_dispatch_registration_is_independent_of_legacy_emit_order() {
        let mut registry = ToolRegistry::default();
        register_into(&mut registry);
        super::super::emit::register_into(&mut registry);
        let descriptor = registry
            .descriptors()
            .into_iter()
            .find(|d| d.name == TOOL_TASK_DISPATCH)
            .unwrap();
        assert_eq!(descriptor.visible_to_roles, &[CardRole::Planner]);
        assert_eq!(
            descriptor.input_schema["required"],
            json!(["name", "goal", "acceptance", "executor", "workspace"])
        );
    }

    #[test]
    fn candidate_dispatch_schema_requires_input_only_for_candidate_variant() {
        let mut registry = ToolRegistry::default();
        register_into(&mut registry);
        let descriptor = registry
            .descriptors()
            .into_iter()
            .find(|d| d.name == TOOL_TASK_DISPATCH)
            .unwrap();
        let schema = &descriptor.input_schema;
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(
            schema["properties"]["workspace"]["enum"],
            json!(["empty", "verified-candidate"])
        );
        assert_eq!(
            schema["oneOf"],
            json!([
                {"properties":{"workspace":{"const":"empty"}},"not":{"required":["input"]}},
                {"properties":{"workspace":{"const":"verified-candidate"}},"required":["input"]}
            ])
        );
        let input = &schema["properties"]["input"];
        assert_eq!(input["type"], "object");
        assert_eq!(input["additionalProperties"], false);
        assert_eq!(input["required"], json!(["producer", "slot"]));
        for field in ["producer", "slot"] {
            assert_eq!(input["properties"][field]["type"], "string");
        }
        for (workspace, source) in [
            ("empty", None),
            (
                "verified-candidate",
                Some(json!({"producer":"release-2","slot":"release_bundle"})),
            ),
        ] {
            let mut args = json!({"name":"Release", "goal":"Exercise files", "acceptance":"Report findings", "executor":"codex", "workspace":workspace});
            if let Some(source) = source {
                args["input"] = source;
            }
            let parsed: DispatchArgs = serde_json::from_value(args.clone()).unwrap();
            assert_eq!(
                serde_json::to_value(parsed.normalize().unwrap()).unwrap(),
                args
            );
        }
    }
}
