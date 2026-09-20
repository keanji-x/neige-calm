//! `calm.user.notify`, the planner's one way to speak to the user from a background turn: the front end renders the call as a
//! normal agent bubble outside the fold. The kernel only validates `text` and writes nothing; codex persists the `mcpToolCall` item.

use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{
    AppContext, ToolCallIdentity, ToolDescriptor, ToolHandler, ToolHandlerFuture, ToolRegistry,
    require_role, role_gated_write_annotations,
};
use crate::model::CardRole;
use serde_json::{Value, json};
use std::sync::Arc;

pub const TOOL_USER_NOTIFY: &str = "calm.user.notify";

/// Upper bound on `text`, in characters (not bytes).
pub const MAX_TEXT_CHARS: usize = 2000;

pub fn register_into(registry: &mut ToolRegistry) {
    registry.register(user_notify_descriptor(), wrap(user_notify));
}

fn wrap<F, Fut>(f: F) -> ToolHandler
where
    F: Fn(Arc<AppContext>, ToolCallIdentity, Value) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<Value, RpcError>> + Send + 'static,
{
    Arc::new(move |ctx, identity, args| -> ToolHandlerFuture {
        let result = f(ctx, identity, args);
        Box::pin(async move {
            result
                .await
                .map(crate::mcp_server::result::ToolResult::structured)
        })
    })
}

fn user_notify_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_USER_NOTIFY.into(),
        description: include_str!("../../../prompts/tools/calm.user.notify.md")
            .trim_end()
            .to_string(),
        input_schema: json!({
            "type": "object",
            "required": ["text"],
            "additionalProperties": false,
            "properties": {
                "text": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": MAX_TEXT_CHARS,
                    "description": "What to say to the user, verbatim. Shown as one \
                                    ordinary message from you."
                }
            }
        }),
        annotations: Some(role_gated_write_annotations()),
        visible_to_roles: &[CardRole::Planner],
    }
}

/// The validated notification text, or the reason it is refused.
fn validate_text(args: &Value) -> Result<String, RpcError> {
    let text = args
        .get("text")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::invalid_params("user_notify: missing `text` (string)"))?
        .trim();
    if text.is_empty() {
        return Err(RpcError::invalid_params(
            "user_notify: `text` must not be empty or whitespace-only",
        ));
    }
    let chars = text.chars().count();
    if chars > MAX_TEXT_CHARS {
        return Err(RpcError::invalid_params(format!(
            "user_notify: `text` is {chars} characters; the limit is {MAX_TEXT_CHARS}"
        )));
    }
    Ok(text.to_string())
}

async fn user_notify(
    _ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Planner)?;
    validate_text(&args)?;
    Ok(json!({ "ok": true }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_is_planner_only_closed_and_named() {
        let d = user_notify_descriptor();
        assert_eq!(d.name, TOOL_USER_NOTIFY);
        assert_eq!(d.visible_to_roles, &[CardRole::Planner]);
        assert_eq!(d.input_schema["additionalProperties"], json!(false));
        assert_eq!(d.input_schema["required"], json!(["text"]));
    }

    #[test]
    fn text_is_trimmed_and_bounded() {
        assert_eq!(validate_text(&json!({ "text": "  hi  " })).unwrap(), "hi");
        assert!(validate_text(&json!({ "text": "   " })).is_err());
        assert!(validate_text(&json!({})).is_err());
        assert!(validate_text(&json!({ "text": 7 })).is_err());
        let at_limit = "é".repeat(MAX_TEXT_CHARS);
        assert!(
            validate_text(&json!({ "text": at_limit })).is_ok(),
            "the limit counts characters, not bytes"
        );
        let over = "x".repeat(MAX_TEXT_CHARS + 1);
        assert!(validate_text(&json!({ "text": over })).is_err());
    }
}
