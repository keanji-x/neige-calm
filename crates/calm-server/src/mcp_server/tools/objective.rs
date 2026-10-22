//! Planner lifecycle tools; the objective host owns authorization and idempotency.
use crate::mcp_server::framing::{RpcError, calm_error};
use crate::mcp_server::registry::{
    AppContext, ToolCallIdentity, ToolDescriptor, ToolRegistry, role_gated_write_annotations,
};
use crate::model::CardRole;
use calm_truth::objective_host::{self, InputSource, ObjectiveInput, PlannerRequestId, Reaction};
use serde_json::{Value, json};
use std::sync::Arc;

pub fn register_into(registry: &mut ToolRegistry) {
    registry.register(
        ToolDescriptor {
            name: "neige_objective_close".into(),
            description: include_str!("../../../prompts/tools/neige_objective_close.md").trim_end().into(),
            input_schema: json!({"type":"object", "additionalProperties":false, "required":["key"], "properties":{"key":{"type":"string", "minLength":1}}}),
            annotations: Some(role_gated_write_annotations()),
            roles: &[CardRole::Planner],
            listed_for: &[CardRole::Planner],
        },
        Arc::new(|ctx, identity, args| Box::pin(async move {
            close(ctx, identity, args).await.map(crate::mcp_server::result::ToolResult::structured)
        })),
    );
}

async fn close(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    let key = args
        .get("key")
        .and_then(Value::as_str)
        .filter(|key| !key.trim().is_empty())
        .ok_or_else(|| RpcError::invalid_params("key must be a nonempty string"))?;
    let principal = identity
        .to_principal()
        .ok_or_else(|| RpcError::forbidden("objective close needs a track-bound Planner"))?;
    let track_id = identity
        .track_id
        .as_ref()
        .ok_or_else(|| RpcError::forbidden("objective close needs a track"))?;
    let db = ctx
        .sqlite_db
        .as_ref()
        .ok_or_else(|| RpcError::internal("objective storage unavailable"))?;
    let reaction = objective_host::react(
        db,
        ObjectiveInput {
            track_id: track_id.clone().into(),
            event: objective::Event::Closed {
                objective: objective::ids::ObjectiveKey::new(key),
            },
            source: InputSource::Planner {
                principal,
                request_id: PlannerRequestId::new(format!("close:{key}")),
            },
        },
    )
    .await
    .map_err(|error| calm_error(error.into()))?
    .into_reaction();
    Ok(match reaction {
        Reaction::Appended { seq, .. } => json!({"status":"Appended", "seq":seq}),
        Reaction::Duplicate { seq } => json!({"status":"Duplicate", "seq":seq}),
    })
}
