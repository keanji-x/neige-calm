//! `neige_user_ask`, the Planner's one way to ask the user (#2209): writes `ask.requested` through
//! the shared [`crate::ask`] entry; the user's `ask.answered` wakes the Planner.

use crate::db::write_with_actor_events_typed;
use crate::event::AskQuestion;
use crate::ids::CardId;
use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{
    AppContext, ToolCallIdentity, ToolDescriptor, ToolHandler, ToolHandlerFuture, ToolRegistry,
    refuse_unknown_keys, role_gated_write_annotations,
};
use crate::model::CardRole;
use serde_json::{Value, json};
use std::sync::Arc;

pub const TOOL_USER_ASK: &str = "neige_user_ask";

pub fn register_into(registry: &mut ToolRegistry) {
    registry.register(user_ask_descriptor(), wrap(user_ask));
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

fn user_ask_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_USER_ASK.into(),
        description: include_str!("../../../prompts/tools/neige_user_ask.md")
            .trim_end()
            .to_string(),
        input_schema: json!({
            "type": "object",
            "required": ["questions"],
            "additionalProperties": false,
            "properties": {
                "questions": {
                    "type": "array",
                    "minItems": 1,
                    "items": {
                        "type": "object",
                        "required": ["title"],
                        "additionalProperties": false,
                        "properties": {
                            "title": { "type": "string" },
                            "options": { "type": "array", "items": { "type": "string" } }
                        }
                    }
                }
            }
        }),
        annotations: Some(role_gated_write_annotations()),
        roles: &[CardRole::Planner],
        listed_for: &[CardRole::Planner],
    }
}

/// The questions as the tool takes them: each a closed object, `options` left out for a free
/// answer. Bounds and trimming are the shared entry's (`crate::ask::validate_questions`).
fn parse_questions(args: &Value) -> Result<Vec<AskQuestion>, RpcError> {
    let questions = args
        .get("questions")
        .and_then(Value::as_array)
        .ok_or_else(|| RpcError::invalid_params("missing `questions` (array)"))?;
    questions
        .iter()
        .enumerate()
        .map(|(i, question)| {
            let at = format!("{TOOL_USER_ASK}: questions[{i}]");
            let object = question
                .as_object()
                .ok_or_else(|| RpcError::invalid_params(format!("{at} must be an object")))?;
            refuse_unknown_keys(object, &["title", "options"], &at)?;
            let title = object.get("title").and_then(Value::as_str).ok_or_else(|| {
                RpcError::invalid_params(format!("{at}: missing `title` (string)"))
            })?;
            let options = match object.get("options") {
                None => Vec::new(),
                Some(options) => options
                    .as_array()
                    .and_then(|options| {
                        options
                            .iter()
                            .map(|option| option.as_str().map(str::to_string))
                            .collect::<Option<Vec<_>>>()
                    })
                    .ok_or_else(|| {
                        RpcError::invalid_params(format!("{at}: `options` must be strings"))
                    })?,
            };
            Ok(AskQuestion {
                title: title.to_string(),
                options,
            })
        })
        .collect()
}

async fn user_ask(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    let questions = parse_questions(&args)?;
    let planner_card = CardId::from(identity.card_id.clone());
    let actor = identity.to_actor_id();

    let (_unit, ids) =
        write_with_actor_events_typed::<(), _>(ctx.repo.as_ref(), None, &ctx.events, &ctx.write, {
            move |tx| {
                Box::pin(async move {
                    let (scope, event) =
                        crate::ask::ask_requested_tx(tx, &planner_card, questions, None).await?;
                    Ok(((), vec![(actor, scope, event)]))
                })
            }
        })
        .await
        .map_err(crate::mcp_server::framing::calm_error)?;
    match ids.as_slice() {
        [ask_id] => Ok(json!({ "ask_id": ask_id })),
        _ => Err(RpcError::internal(format!(
            "expected one event id, got {ids:?}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_is_planner_only_closed_and_named() {
        let d = user_ask_descriptor();
        assert_eq!(d.name, TOOL_USER_ASK);
        assert_eq!(d.roles, &[CardRole::Planner]);
        assert_eq!(d.input_schema["additionalProperties"], json!(false));
        assert_eq!(d.input_schema["required"], json!(["questions"]));
        assert_eq!(
            d.input_schema["properties"]["questions"]["items"]["additionalProperties"],
            json!(false)
        );
    }
}
