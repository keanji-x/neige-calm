//! MCP side of #2130 mail: `neige_mail_send` (listed for the Planner) and the hidden views
//! `neige_mail_ls` / `neige_mail_cat`, served as `neige mail ls|cat`. The checks, the write and
//! the hop rule live in [`crate::mail`].

use std::sync::Arc;

use serde_json::{Map, Value, json};

use crate::mail::{
    self, Recipient, SendRequest, TOOL_MAIL_CAT, TOOL_MAIL_LS, TOOL_MAIL_SEND, hop_label, invalid,
};
use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{
    AppContext, ToolCallIdentity, ToolDescriptor, ToolHandler, ToolHandlerFuture, ToolRegistry,
    read_only_annotations, require_role, role_gated_write_annotations,
};
use crate::mcp_server::result::ToolResult;
use crate::model::CardRole;

/// Upper bounds, in characters; the `mails` CHECKs say the same.
pub const MAX_SUMMARY_CHARS: usize = 200;
pub const MAX_TEXT_CHARS: usize = 8000;

const SEND_KEYS: &[&str] = &["track_id", "mail_id", "summary", "text"];
const LS_KEYS: &[&str] = &["cursor"];
const CAT_KEYS: &[&str] = &["mail_id"];

pub fn register_into(registry: &mut ToolRegistry) {
    registry.register(send_descriptor(), wrap(send));
    registry.register(ls_descriptor(), wrap(ls));
    registry.register(cat_descriptor(), wrap(cat));
}

fn wrap<F, Fut>(f: F) -> ToolHandler
where
    F: Fn(Arc<AppContext>, ToolCallIdentity, Value) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<Value, RpcError>> + Send + 'static,
{
    Arc::new(move |ctx, identity, args| -> ToolHandlerFuture {
        let result = f(ctx, identity, args);
        Box::pin(async move { result.await.map(ToolResult::structured) })
    })
}

fn send_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_MAIL_SEND.into(),
        description: include_str!("../../../prompts/tools/neige_mail_send.md")
            .trim_end()
            .to_string(),
        input_schema: json!({
            "type": "object",
            "required": ["summary", "text"],
            "properties": {
                "track_id": { "type": "string" },
                "mail_id": { "type": "string" },
                "summary": { "type": "string", "minLength": 1, "maxLength": MAX_SUMMARY_CHARS },
                "text": { "type": "string", "minLength": 1, "maxLength": MAX_TEXT_CHARS }
            },
            "additionalProperties": false
        }),
        // The handler checks the role and writes only inside the caller's Area (D13).
        annotations: Some(role_gated_write_annotations()),
        visible_to_roles: &[CardRole::Planner],
    }
}

/// Served through `neige mail ls`: hidden from every role's `tools/list`.
fn ls_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_MAIL_LS.into(),
        description: include_str!("../../../prompts/tools/neige_mail_ls.md")
            .trim_end()
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": { "cursor": { "type": "string" } },
            "additionalProperties": false
        }),
        annotations: Some(read_only_annotations()),
        visible_to_roles: &[],
    }
}

/// Served through `neige mail cat <mail_id>`: hidden; a view whose only write is the read stamp.
fn cat_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_MAIL_CAT.into(),
        description: include_str!("../../../prompts/tools/neige_mail_cat.md")
            .trim_end()
            .to_string(),
        input_schema: json!({
            "type": "object",
            "required": ["mail_id"],
            "properties": { "mail_id": { "type": "string" } },
            "additionalProperties": false
        }),
        annotations: Some(read_only_annotations()),
        visible_to_roles: &[],
    }
}

/// The argument object, refusing a key outside `keys` with the valid ones.
fn arguments<'a>(
    tool: &str,
    args: &'a Value,
    keys: &[&str],
) -> Result<&'a Map<String, Value>, RpcError> {
    let object = args
        .as_object()
        .ok_or_else(|| invalid(tool, "arguments", "arguments must be an object"))?;
    if let Some(key) = object.keys().find(|key| !keys.contains(&key.as_str())) {
        return Err(invalid(
            tool,
            "arguments",
            &format!("unknown argument `{key}`; the keys are {}", keys.join(", ")),
        ));
    }
    Ok(object)
}

fn text_argument(
    tool: &str,
    object: &Map<String, Value>,
    key: &str,
) -> Result<Option<String>, RpcError> {
    match object.get(key) {
        None => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(invalid(
            tool,
            "arguments",
            &format!("`{key}` must be a string"),
        )),
    }
}

/// The checked send request, or the §6 argument refusal.
fn send_request(args: &Value) -> Result<SendRequest, RpcError> {
    const TOOL: &str = TOOL_MAIL_SEND;
    let object = arguments(TOOL, args, SEND_KEYS)?;
    let to = match (
        text_argument(TOOL, object, "track_id")?,
        text_argument(TOOL, object, "mail_id")?,
    ) {
        (Some(track_id), None) => Recipient::Track(track_id),
        (None, Some(mail_id)) => Recipient::Reply(mail_id),
        _ => {
            return Err(invalid(
                TOOL,
                "recipient",
                "give exactly one of track_id (new mail) or mail_id (reply)",
            ));
        }
    };
    // SQLite's `length()` stops at a NUL, so the table's length CHECKs cannot judge such a value.
    for key in ["summary", "text"] {
        if text_argument(TOOL, object, key)?.is_some_and(|value| value.contains('\0')) {
            return Err(invalid(
                TOOL,
                if key == "summary" { "summary" } else { "text" },
                &format!("{key} must not contain NUL (U+0000)"),
            ));
        }
    }
    let summary = text_argument(TOOL, object, "summary")?
        .map(|summary| summary.trim().to_string())
        .filter(|summary| {
            (1..=MAX_SUMMARY_CHARS).contains(&summary.chars().count())
                && !summary.contains(['\n', '\r'])
        })
        .ok_or_else(|| invalid(TOOL, "summary", "summary is 1..200 characters on one line"))?;
    let text = text_argument(TOOL, object, "text")?
        .filter(|text| !text.trim().is_empty() && text.chars().count() <= MAX_TEXT_CHARS)
        .ok_or_else(|| invalid(TOOL, "text", "text is 1..8000 characters"))?;
    Ok(SendRequest { to, summary, text })
}

async fn send(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Planner)?;
    let request = send_request(&args)?;
    let sent = mail::send(&ctx, &identity, request).await?;
    Ok(json!({ "mail_id": sent.mail_id, "hop": hop_label(sent.hop) }))
}

async fn ls(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Planner)?;
    let object = arguments(TOOL_MAIL_LS, &args, LS_KEYS)?;
    let cursor = text_argument(TOOL_MAIL_LS, object, "cursor")?;
    mail::ls(&ctx, &identity, cursor.as_deref()).await
}

async fn cat(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Planner)?;
    let object = arguments(TOOL_MAIL_CAT, &args, CAT_KEYS)?;
    let mail_id = text_argument(TOOL_MAIL_CAT, object, "mail_id")?
        .ok_or_else(|| invalid(TOOL_MAIL_CAT, "arguments", "missing `mail_id` (string)"))?;
    mail::cat(&ctx, &identity, &mail_id).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refusal(args: Value) -> String {
        send_request(&args).expect_err("refused").message
    }

    #[test]
    fn a_send_names_exactly_one_recipient_and_bounded_texts() {
        let base = json!({ "summary": "s", "text": "t" });
        let with = |key: &str, value: Value| {
            let mut args = base.clone();
            args[key] = value;
            args
        };
        assert!(send_request(&with("track_id", json!("tr"))).is_ok());
        assert!(send_request(&with("mail_id", json!("m"))).is_ok());
        let one_of = "neige_mail_send: give exactly one of track_id (new mail) or mail_id (reply)";
        assert_eq!(refusal(base.clone()), one_of);
        let mut both = with("track_id", json!("tr"));
        both["mail_id"] = json!("m");
        assert_eq!(refusal(both), one_of);
        let target = with("track_id", json!("tr"));
        let mut long = target.clone();
        long["summary"] = json!("x".repeat(MAX_SUMMARY_CHARS + 1));
        let mut two_lines = target.clone();
        two_lines["summary"] = json!("a\nb");
        for args in [long, two_lines] {
            assert_eq!(
                refusal(args),
                "neige_mail_send: summary is 1..200 characters on one line"
            );
        }
        let mut blank = target.clone();
        blank["text"] = json!("  ");
        assert_eq!(
            refusal(blank),
            "neige_mail_send: text is 1..8000 characters"
        );
        let mut extra = target;
        extra["cc"] = json!("x");
        assert!(refusal(extra).contains("unknown argument `cc`"));
    }
}
