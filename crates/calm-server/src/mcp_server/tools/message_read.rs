//! Worker-only reads of kernel-recorded Planner messages.
use crate::mcp_server::framing::{RpcError, calm_error};
use crate::mcp_server::registry::{
    AppContext, ToolCallIdentity, ToolDescriptor, ToolRegistry, role_gated_write_annotations,
};
use crate::mcp_server::tools::emit::required_attempt_id;
use crate::model::{CardRole, TaskStatus};
use crate::operation::worker_attempt::{current_running_attempt_tx, owned_attempt_tx};
use serde_json::{Value, json};
use std::sync::Arc;

pub const TOOL_MESSAGE_READ: &str = "neige_message_read";

pub fn register_into(registry: &mut ToolRegistry) {
    registry.register(ToolDescriptor {
        name: TOOL_MESSAGE_READ.into(),
        description: include_str!("../../../prompts/tools/neige_message_read.md").trim_end().into(),
        input_schema: json!({"type":"object", "additionalProperties":false, "required":["attempt_id"], "properties":{"attempt_id":{"type":"string", "minLength":1}}}),
        annotations: Some(role_gated_write_annotations()),
        roles: &[CardRole::Worker], listed_for: &[CardRole::Worker],
    }, Arc::new(|ctx, identity, args| Box::pin(async move {
        read_messages(ctx, identity, args).await.map(crate::mcp_server::result::ToolResult::structured)
    })));
}

async fn read_messages(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    let attempt = required_attempt_id(&args, TOOL_MESSAGE_READ)?;
    let (_, track) =
        crate::mcp_server::tools::track_file::resolve_track_for_identity(&ctx, &identity).await?;
    crate::db::write_in_tx_typed(ctx.repo.as_ref(), move |tx| {
        Box::pin(async move {
            crate::decision_sink::admit_worker_write_tx(tx, &identity, &track.id).await?;
            let task = owned_attempt_tx(tx, &attempt, &identity.card_id, track.id.as_str()).await?;
            let reported = matches!(
                task.status,
                TaskStatus::Done | TaskStatus::Failed | TaskStatus::Verifying
            );
            let messages = if reported {
                crate::db::sqlite::planner_messages_for_attempt_tx(tx, &attempt).await?
            } else {
                current_running_attempt_tx(tx, &task).await?;
                crate::db::sqlite::planner_messages_read_tx(tx, &attempt).await?
            };
            let messages: Vec<_> = messages
                .into_iter()
                .map(|m| {
                    json!({
                        "message_id":m.message_id, "seq":m.seq, "text":m.text,
                        "read_at":crate::time_format::at_opt(m.read_at_ms)
                    })
                })
                .collect();
            let mut result = json!({"messages":messages});
            if reported {
                result["status"] = json!("messages_are_not_work");
                result["message"] = json!("this attempt has reported; messages are not work");
            }
            Ok(result)
        })
    })
    .await
    .map_err(calm_error)
}
