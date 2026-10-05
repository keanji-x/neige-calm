//! #1780 `neige_preview_add` / `neige_preview_rm`: bind a loopback dev server to a
//! preview gateway pool port for the caller's own track. The track is always
//! `identity.track_id`, never an argument (an unknown argument is refused with the valid keys), so
//! registrations are per track. The tools' `preview_id` is the registry's and the block's `key`. Planner-only: Dispatch workers are isolated, have no network, and
//! their MCP grant allowlist does not include these tools.

use crate::ids::TrackId;
use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{
    AppContext, ToolCallIdentity, ToolDescriptor, ToolHandler, ToolHandlerFuture, ToolRegistry,
    require_role, role_gated_write_annotations,
};
use crate::mcp_server::tools::write_args::refuse_unknown_keys;
use crate::model::CardRole;
use crate::preview::PreviewRegistry;
use serde_json::{Value, json};
use std::sync::Arc;

pub const TOOL_PREVIEW_ADD: &str = "neige_preview_add";
pub const TOOL_PREVIEW_RM: &str = "neige_preview_rm";

const KEY_PATTERN: &str = "^[a-z0-9][a-z0-9_-]{0,63}$";
const ADD_KEYS: &[&str] = &["preview_id", "target_port", "title"];
const RM_KEYS: &[&str] = &["preview_id"];
pub const MAX_TITLE_CHARS: usize = 120;

pub fn register_into(registry: &mut ToolRegistry) {
    registry.register(add_descriptor(), wrap(preview_add));
    registry.register(rm_descriptor(), wrap(preview_rm));
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

fn preview_id_schema() -> Value {
    json!({
        "type": "string",
        "pattern": KEY_PATTERN,
        "description": "Stable name of this preview within the track, e.g. `fe`."
    })
}

fn add_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_PREVIEW_ADD.into(),
        description: include_str!("../../../prompts/tools/neige_preview_add.md")
            .trim_end()
            .to_string(),
        input_schema: json!({
            "type": "object",
            "required": ADD_KEYS,
            "additionalProperties": false,
            "properties": {
                "preview_id": preview_id_schema(),
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
        visible_to_roles: &[CardRole::Planner],
    }
}

fn rm_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_PREVIEW_RM.into(),
        description: include_str!("../../../prompts/tools/neige_preview_rm.md")
            .trim_end()
            .to_string(),
        input_schema: json!({
            "type": "object",
            "required": RM_KEYS,
            "additionalProperties": false,
            "properties": { "preview_id": preview_id_schema() }
        }),
        annotations: Some(role_gated_write_annotations()),
        visible_to_roles: &[CardRole::Planner],
    }
}

fn caller_track(tool: &str, identity: &ToolCallIdentity) -> Result<TrackId, RpcError> {
    require_role(identity, CardRole::Planner)?;
    identity
        .track_id
        .as_deref()
        .map(TrackId::from)
        .ok_or_else(|| RpcError::invalid_params(format!("{tool} requires a track-scoped caller")))
}

/// `KEY_PATTERN`, spelled out.
fn parse_preview_id(tool: &str, args: &Value) -> Result<String, RpcError> {
    let key = args
        .get("preview_id")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            RpcError::invalid_params(format!("{tool}: missing `preview_id` (string)"))
        })?;
    let valid_char = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit();
    let valid = key.len() <= 64
        && key.chars().next().is_some_and(valid_char)
        && key.chars().all(|c| valid_char(c) || c == '_' || c == '-');
    if !valid {
        return Err(RpcError::invalid_params(format!(
            "{tool}: `preview_id` {key:?} must match {KEY_PATTERN}"
        )));
    }
    Ok(key.to_owned())
}

fn parse_add_args(args: &Value) -> Result<(String, u16, String), RpcError> {
    let tool = TOOL_PREVIEW_ADD;
    refuse_unknown_keys(args, tool, ADD_KEYS)?;
    let key = parse_preview_id(tool, args)?;
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

fn add(
    registry: &PreviewRegistry,
    identity: &ToolCallIdentity,
    args: &Value,
) -> Result<Value, RpcError> {
    let track_id = caller_track(TOOL_PREVIEW_ADD, identity)?;
    let (key, target_port, title) = parse_add_args(args)?;
    let port = registry
        .register(&track_id, &key, &title, target_port)
        .map_err(|e| RpcError::invalid_params(format!("{TOOL_PREVIEW_ADD}: {e}")))?;
    Ok(json!({
        "preview_id": key,
        "port": port,
        "block_hint": { "kind": "preview", "payload": { "key": key, "title": title } },
    }))
}

fn rm(
    registry: &PreviewRegistry,
    identity: &ToolCallIdentity,
    args: &Value,
) -> Result<Value, RpcError> {
    let track_id = caller_track(TOOL_PREVIEW_RM, identity)?;
    refuse_unknown_keys(args, TOOL_PREVIEW_RM, RM_KEYS)?;
    let key = parse_preview_id(TOOL_PREVIEW_RM, args)?;
    let port = registry.unregister(&track_id, &key);
    Ok(json!({ "preview_id": key, "port": port }))
}

async fn preview_add(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    add(&ctx.preview, &identity, &args)
}

async fn preview_rm(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    rm(&ctx.preview, &identity, &args)
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
        json!({ "preview_id": key, "target_port": target_port, "title": " Web FE " })
    }

    fn message(result: Result<Value, RpcError>) -> String {
        let error = result.expect_err("must be refused");
        assert_eq!(error.code, RpcError::INVALID_PARAMS);
        error.message
    }

    #[test]
    fn add_returns_pool_port_keeps_it_and_hints_the_block() {
        let reg = pool();
        let planner = caller(CardRole::Planner, Some("track-a"));
        let first = add(&reg, &planner, &args("fe", 5173)).unwrap();
        assert_eq!(
            first,
            json!({
                "preview_id": "fe",
                "port": 4050,
                "block_hint": { "kind": "preview", "payload": { "key": "fe", "title": "Web FE" } },
            })
        );
        let again = add(&reg, &planner, &args("fe", 5180)).unwrap();
        assert_eq!(again["port"], 4050, "same (track, key) keeps its port");
        assert_eq!(reg.lookup(4050).unwrap().target_port, 5180);
    }

    #[test]
    fn full_disabled_and_refused_targets_say_why() {
        let reg = pool();
        let planner = caller(CardRole::Planner, Some("track-a"));
        add(&reg, &planner, &args("fe", 5173)).unwrap();
        add(&reg, &planner, &args("api", 8080)).unwrap();
        let full = message(add(&reg, &planner, &args("docs", 8081)));
        assert!(full.contains("4050: track track-a key fe"), "{full}");
        let calm = message(add(&reg, &planner, &args("x", 4040)));
        assert!(calm.contains("calm's own listen"), "{calm}");
        let off = message(add(
            &PreviewRegistry::disabled(),
            &planner,
            &args("fe", 5173),
        ));
        assert!(off.contains("CALM_PREVIEW_PORTS"), "{off}");
    }

    #[test]
    fn rm_frees_only_the_callers_own_key() {
        let reg = pool();
        let a = caller(CardRole::Planner, Some("track-a"));
        let b = caller(CardRole::Planner, Some("track-b"));
        add(&reg, &b, &args("fe", 5173)).unwrap();
        let key = json!({ "preview_id": "fe" });
        assert_eq!(
            rm(&reg, &a, &key).unwrap(),
            json!({ "preview_id": "fe", "port": null })
        );
        assert_eq!(reg.lookup(4050).unwrap().track_id, TrackId::from("track-b"));
        assert_eq!(
            rm(&reg, &b, &key).unwrap(),
            json!({ "preview_id": "fe", "port": 4050 })
        );
        assert!(reg.lookup(4050).is_none());
        assert_eq!(add(&reg, &a, &args("fe", 5173)).unwrap()["port"], 4050);
    }

    #[test]
    fn other_roles_and_trackless_callers_are_refused() {
        let reg = pool();
        for role in [CardRole::Worker, CardRole::Assistant, CardRole::ReportCard] {
            let who = caller(role, Some("track-a"));
            assert!(message(add(&reg, &who, &args("fe", 5173))).contains("requires role"));
            assert!(
                message(rm(&reg, &who, &json!({"preview_id": "fe"}))).contains("requires role")
            );
        }
        let trackless = caller(CardRole::Planner, None);
        let refused = message(add(&reg, &trackless, &args("fe", 5173)));
        assert!(refused.contains("track-scoped"), "{refused}");
        assert!(reg.for_track(&TrackId::from("track-a")).is_empty());
    }

    /// The track comes from the identity only; a `track_id`, the retired `key` or any other
    /// unknown key is refused with the valid keys, and nothing is added or removed.
    #[test]
    fn unknown_arguments_are_refused_with_the_valid_keys() {
        let reg = pool();
        let a = caller(CardRole::Planner, Some("track-a"));
        for extra in ["track_id", "key", "extra"] {
            let mut spoofed = args("fe", 5173);
            spoofed[extra] = json!("track-b");
            let refused = message(add(&reg, &a, &spoofed));
            assert_eq!(
                refused,
                format!(
                    "neige_preview_add: unknown argument `{extra}`; valid: `preview_id`, \
                     `target_port`, `title`"
                )
            );
        }
        assert!(reg.for_track(&TrackId::from("track-a")).is_empty());
        add(&reg, &a, &args("fe", 5173)).unwrap();
        let refused = message(rm(&reg, &a, &json!({"preview_id": "fe", "key": "fe"})));
        assert_eq!(
            refused,
            "neige_preview_rm: unknown argument `key`; valid: `preview_id`"
        );
        assert_eq!(reg.for_track(&TrackId::from("track-a")).len(), 1);
    }

    #[test]
    fn key_and_title_are_validated() {
        let reg = pool();
        let who = caller(CardRole::Planner, Some("track-a"));
        let long = "a".repeat(65);
        for key in ["", "Fe", "-fe", "_x", "a b", "a.b", long.as_str()] {
            assert!(
                message(add(&reg, &who, &args(key, 5173))).contains("`preview_id`"),
                "{key:?}"
            );
        }
        assert!(add(&reg, &who, &args(&"a".repeat(64), 5173)).is_ok());
        let blank = json!({ "preview_id": "fe", "target_port": 5173, "title": "  " });
        assert!(message(add(&reg, &who, &blank)).contains("`title`"));
    }
}
