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
        description: "Declare one named Codex task in an empty isolated workspace or consuming a verified candidate. workspace=verified-candidate requires input with exact same-Track producer key and slot; no report revision or generated task key copying is needed. For review-required sources, use calm.task.verdict to accept the producer after exact checks and review pass; optional lifecycle continues in that same write. Reviewing already schedules. Declared-checks-only sources keep their existing machine policy. Select the returned repair_key explicitly to consume a repaired candidate. current.candidate_input is a compact input/admission diagnostic, not a Worker result; calm.plan.list provides full evidence. name is a Track-local business identity for Dispatch-created tasks: only surrounding whitespace is trimmed; case and Unicode are exact. Same name and exact typed contract replays the original task key and block, even after report changes or session replacement; a different contract conflicts. Use a new meaningful name for new work, existing recovery for technical retries and calm.task.repair for an eligible rejected candidate. goal and acceptance are required. Semantic acceptance is reviewed from the completion report, not a machine gate or file candidate qualification. Receipt creation does not mean running: current diagnostics preserve User release, lifecycle and budget controls. current.contract_status compares the current declaration with the original Dispatch contract; it does not prove an attempt executed that contract. No dependencies or other options. Normal result receipts arrive through the existing Planner result path. The executor environment is fixed and identical for every attempt: a fresh empty workspace (writable /workspace and /tmp only), no network for the model's commands, no web search, exactly four MCP tools (calm.task.complete, calm.task.fail, calm.report.read, calm.plan.list), and a read-only host /usr whose contents the kernel does not enumerate. The response states it as current.executor_environment; recovery never changes it, so a task that needs a missing capability must be re-planned, not recovered.".into(),
        input_schema: json!({
            "type":"object", "additionalProperties":false,
            "required":["name","goal","acceptance","executor","workspace"],
            "properties": {
                "name":{"type":"string","minLength":1,"description":"Nonempty after trim, at most 200 UTF-8 bytes, no control characters; immutable Track-local Dispatch identity."},
                "goal":{"type":"string","minLength":1},
                "acceptance":{"type":"string","minLength":1},
                "executor":{"type":"string","enum":["codex"]},
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
    let args = args.normalize().map_err(map_error)?;
    let (track, _, card, payload) =
        super::track_report::resolve_report_for_caller(&ctx, &identity).await?;
    CardDecisionSink::from_app_context(&ctx)
        .commit_task_dispatch(
            &identity,
            track,
            card,
            payload,
            args,
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
