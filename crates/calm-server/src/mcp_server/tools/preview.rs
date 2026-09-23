//! #1780 `calm.preview.register` / `calm.preview.unregister`: bind a loopback dev server to a
//! preview gateway pool port for the caller's own track. The track is always
//! `identity.track_id`, never an argument, so registrations are per track.

use crate::ids::TrackId;
use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{
    AppContext, ToolCallIdentity, ToolDescriptor, ToolHandler, ToolHandlerFuture, ToolRegistry,
    require_role_any, role_gated_write_annotations,
};
use crate::model::CardRole;
use crate::preview::PreviewRegistry;
use serde_json::{Value, json};
use std::sync::Arc;

pub const TOOL_PREVIEW_REGISTER: &str = "calm.preview.register";
pub const TOOL_PREVIEW_UNREGISTER: &str = "calm.preview.unregister";

/// Planner and Worker may call; only the Planner (the documented path, via `calm.terminal.open`)
/// sees the tools in `tools/list`, so worker prompts need not advertise them.
const ROLES: &[CardRole] = &[CardRole::Planner, CardRole::Worker];
const VISIBLE_TO: &[CardRole] = &[CardRole::Planner];
const KEY_PATTERN: &str = "^[a-z0-9][a-z0-9_-]{0,63}$";
pub const MAX_TITLE_CHARS: usize = 120;

pub fn register_into(registry: &mut ToolRegistry) {
    registry.register(register_descriptor(), wrap(preview_register));
    registry.register(unregister_descriptor(), wrap(preview_unregister));
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

fn key_schema() -> Value {
    json!({
        "type": "string",
        "pattern": KEY_PATTERN,
        "description": "Stable name of this preview within the track, e.g. `fe`."
    })
}

fn register_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_PREVIEW_REGISTER.into(),
        description: include_str!("../../../prompts/tools/calm.preview.register.md")
            .trim_end()
            .to_string(),
        input_schema: json!({
            "type": "object",
            "required": ["key", "target_port", "title"],
            "additionalProperties": false,
            "properties": {
                "key": key_schema(),
                "target_port": {
                    "type": "integer",
                    "minimum": 1024,
                    "maximum": 65535,
                    "description": "The 127.0.0.1 port your dev server listens on."
                },
                "title": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": MAX_TITLE_CHARS,
                    "description": "Short label shown with the preview."
                }
            }
        }),
        annotations: Some(role_gated_write_annotations()),
        visible_to_roles: VISIBLE_TO,
    }
}

fn unregister_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_PREVIEW_UNREGISTER.into(),
        description: include_str!("../../../prompts/tools/calm.preview.unregister.md")
            .trim_end()
            .to_string(),
        input_schema: json!({
            "type": "object",
            "required": ["key"],
            "additionalProperties": false,
            "properties": { "key": key_schema() }
        }),
        annotations: Some(role_gated_write_annotations()),
        visible_to_roles: VISIBLE_TO,
    }
}

fn caller_track(tool: &str, identity: &ToolCallIdentity) -> Result<TrackId, RpcError> {
    require_role_any(identity, ROLES)?;
    identity
        .track_id
        .as_deref()
        .map(TrackId::from)
        .ok_or_else(|| RpcError::invalid_params(format!("{tool} requires a track-scoped caller")))
}

/// `KEY_PATTERN`, spelled out.
fn parse_key(tool: &str, args: &Value) -> Result<String, RpcError> {
    let key = args
        .get("key")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::invalid_params(format!("{tool}: missing `key` (string)")))?;
    let valid_char = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit();
    let valid = key.len() <= 64
        && key.chars().next().is_some_and(valid_char)
        && key.chars().all(|c| valid_char(c) || c == '_' || c == '-');
    if !valid {
        return Err(RpcError::invalid_params(format!(
            "{tool}: `key` {key:?} must match {KEY_PATTERN}"
        )));
    }
    Ok(key.to_owned())
}

fn parse_register_args(args: &Value) -> Result<(String, u16, String), RpcError> {
    let tool = TOOL_PREVIEW_REGISTER;
    let key = parse_key(tool, args)?;
    let target_port = args
        .get("target_port")
        .and_then(Value::as_u64)
        .and_then(|port| u16::try_from(port).ok())
        .ok_or_else(|| {
            RpcError::invalid_params(format!("{tool}: `target_port` must be a port number"))
        })?;
    let title = args
        .get("title")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default();
    let chars = title.chars().count();
    if chars == 0 || chars > MAX_TITLE_CHARS {
        return Err(RpcError::invalid_params(format!(
            "{tool}: `title` must be 1..={MAX_TITLE_CHARS} characters"
        )));
    }
    Ok((key, target_port, title.to_owned()))
}

fn register(
    registry: &PreviewRegistry,
    identity: &ToolCallIdentity,
    args: &Value,
) -> Result<Value, RpcError> {
    let track_id = caller_track(TOOL_PREVIEW_REGISTER, identity)?;
    let (key, target_port, title) = parse_register_args(args)?;
    let port = registry
        .register(&track_id, &key, &title, target_port)
        .map_err(|e| RpcError::invalid_params(format!("{TOOL_PREVIEW_REGISTER}: {e}")))?;
    Ok(json!({
        "key": key,
        "port": port,
        "block_hint": { "kind": "preview", "payload": { "key": key, "title": title } },
    }))
}

fn unregister(
    registry: &PreviewRegistry,
    identity: &ToolCallIdentity,
    args: &Value,
) -> Result<Value, RpcError> {
    let track_id = caller_track(TOOL_PREVIEW_UNREGISTER, identity)?;
    let key = parse_key(TOOL_PREVIEW_UNREGISTER, args)?;
    let port = registry.unregister(&track_id, &key);
    Ok(json!({ "key": key, "port": port }))
}

async fn preview_register(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    register(&ctx.preview, &identity, &args)
}

async fn preview_unregister(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    unregister(&ctx.preview, &identity, &args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preview::PreviewPorts;
    use crate::session_projection_repo::AgentProvider;

    fn caller(role: CardRole, track: Option<&str>) -> ToolCallIdentity {
        ToolCallIdentity {
            card_id: "card-1".into(),
            role,
            provider: AgentProvider::Codex,
            session_id: "session-1".into(),
            track_id: track.map(str::to_owned),
            area_id: "area-1".into(),
            thread_id: "thread-1".into(),
        }
    }

    fn pool() -> PreviewRegistry {
        PreviewRegistry::new(PreviewPorts::parse("4050-4051").unwrap(), 4040)
    }

    fn args(key: &str, target_port: u16) -> Value {
        json!({ "key": key, "target_port": target_port, "title": " Web FE " })
    }

    fn message(result: Result<Value, RpcError>) -> String {
        let error = result.expect_err("must be refused");
        assert_eq!(error.code, RpcError::INVALID_PARAMS);
        error.message
    }

    #[test]
    fn register_returns_pool_port_keeps_it_and_hints_the_block() {
        let reg = pool();
        let worker = caller(CardRole::Worker, Some("track-a"));
        let first = register(&reg, &worker, &args("fe", 5173)).unwrap();
        assert_eq!(
            first,
            json!({
                "key": "fe",
                "port": 4050,
                "block_hint": { "kind": "preview", "payload": { "key": "fe", "title": "Web FE" } },
            })
        );
        let planner = caller(CardRole::Planner, Some("track-a"));
        let again = register(&reg, &planner, &args("fe", 5180)).unwrap();
        assert_eq!(again["port"], 4050, "same (track, key) keeps its port");
        assert_eq!(reg.lookup(4050).unwrap().target_port, 5180);
    }

    #[test]
    fn full_disabled_and_refused_targets_say_why() {
        let reg = pool();
        let worker = caller(CardRole::Worker, Some("track-a"));
        register(&reg, &worker, &args("fe", 5173)).unwrap();
        register(&reg, &worker, &args("api", 8080)).unwrap();
        let full = message(register(&reg, &worker, &args("docs", 8081)));
        assert!(full.contains("4050: track track-a key fe"), "{full}");
        let calm = message(register(&reg, &worker, &args("x", 4040)));
        assert!(calm.contains("calm's own listen"), "{calm}");
        let off = message(register(
            &PreviewRegistry::disabled(),
            &worker,
            &args("fe", 5173),
        ));
        assert!(off.contains("CALM_PREVIEW_PORTS"), "{off}");
    }

    #[test]
    fn unregister_frees_only_the_callers_own_key() {
        let reg = pool();
        let a = caller(CardRole::Worker, Some("track-a"));
        let b = caller(CardRole::Worker, Some("track-b"));
        register(&reg, &b, &args("fe", 5173)).unwrap();
        let key = json!({ "key": "fe" });
        assert_eq!(
            unregister(&reg, &a, &key).unwrap(),
            json!({ "key": "fe", "port": null })
        );
        assert_eq!(reg.lookup(4050).unwrap().track_id, TrackId::from("track-b"));
        assert_eq!(
            unregister(&reg, &b, &key).unwrap(),
            json!({ "key": "fe", "port": 4050 })
        );
        assert!(reg.lookup(4050).is_none());
        assert_eq!(register(&reg, &a, &args("fe", 5173)).unwrap()["port"], 4050);
    }

    #[test]
    fn other_roles_and_trackless_callers_are_refused() {
        let reg = pool();
        for role in [CardRole::Assistant, CardRole::ReportCard] {
            let who = caller(role, Some("track-a"));
            assert!(message(register(&reg, &who, &args("fe", 5173))).contains("requires role"));
            assert!(
                message(unregister(&reg, &who, &json!({"key": "fe"}))).contains("requires role")
            );
        }
        let trackless = caller(CardRole::Planner, None);
        let refused = message(register(&reg, &trackless, &args("fe", 5173)));
        assert!(refused.contains("track-scoped"), "{refused}");
        assert!(reg.for_track(&TrackId::from("track-a")).is_empty());
    }

    #[test]
    fn key_and_title_are_validated() {
        let reg = pool();
        let who = caller(CardRole::Planner, Some("track-a"));
        let long = "a".repeat(65);
        for key in ["", "Fe", "-fe", "_x", "a b", "a.b", long.as_str()] {
            assert!(
                message(register(&reg, &who, &args(key, 5173))).contains("`key`"),
                "{key:?}"
            );
        }
        assert!(register(&reg, &who, &args(&"a".repeat(64), 5173)).is_ok());
        let blank = json!({ "key": "fe", "target_port": 5173, "title": "  " });
        assert!(message(register(&reg, &who, &blank)).contains("`title`"));
    }
}
