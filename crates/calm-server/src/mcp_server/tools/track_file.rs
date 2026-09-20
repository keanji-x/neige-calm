//! Read-only MCP file views (`calm.track.ls`, `calm.track.cat`) rooted at the track bound to the caller's MCP connection.

use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{
    AppContext, ToolCallIdentity, ToolDescriptor, ToolHandler, ToolHandlerFuture, ToolRegistry,
    read_only_annotations, require_role_any,
};
use crate::model::{Card, CardRole, Track};
use crate::track_fs_view::{TrackFsError, TrackFsView, normalize_path};
use serde_json::{Value, json};
use std::sync::Arc;

pub const TOOL_TRACK_LS: &str = "calm.track.ls";
pub const TOOL_TRACK_CAT: &str = "calm.track.cat";

pub fn register_into(registry: &mut ToolRegistry) {
    registry.register(ls_descriptor(), wrap(track_ls));
    registry.register(cat_descriptor(), wrap(track_cat));
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

/// Return-shape contract consumed by `neige`: `ls` returns a bare JSON array; `cat` returns `{ content, content_type }`.
fn ls_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_TRACK_LS.into(),
        description: include_str!("../../../prompts/tools/calm.track.ls.md")
            .trim_end()
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" }
            }
        }),
        annotations: Some(read_only_annotations()),
        visible_to_roles: &[],
    }
}

fn cat_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_TRACK_CAT.into(),
        description: include_str!("../../../prompts/tools/calm.track.cat.md")
            .trim_end()
            .to_string(),
        input_schema: json!({
            "type": "object",
            "required": ["path"],
            "properties": {
                "path": { "type": "string" }
            }
        }),
        annotations: Some(read_only_annotations()),
        visible_to_roles: &[],
    }
}

async fn track_ls(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role_any(&identity, &[CardRole::Planner, CardRole::Worker])?;
    let path = parse_path_arg(&args, false)?;
    let (_, track) = resolve_track_for_identity(&ctx, &identity).await?;
    let view = TrackFsView::new(ctx.repo.as_ref(), &ctx.write);
    let entries = view
        .ls(&track, Some(path.as_str()))
        .await
        .map_err(track_fs_error_to_rpc)?;
    serde_json::to_value(entries)
        .map_err(|e| RpcError::internal(format!("track_file: json serialization: {e}")))
}

async fn track_cat(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role_any(&identity, &[CardRole::Planner, CardRole::Worker])?;
    let path = parse_path_arg(&args, true)?;
    let (_, track) = resolve_track_for_identity(&ctx, &identity).await?;
    // `plan/<key>/gate.log` is enabled only here (MCP carries a card identity); the gate-logs dir is the configured one, never recomputed from env.
    let view = TrackFsView::new(ctx.repo.as_ref(), &ctx.write)
        .with_gate_log_access(identity.role, ctx.gate_logs_dir.clone());
    let content = view
        .cat(&track, path.as_str())
        .await
        .map_err(track_fs_error_to_rpc)?;
    serde_json::to_value(content)
        .map_err(|e| RpcError::internal(format!("track_file: json serialization: {e}")))
}

fn parse_path_arg(args: &Value, required: bool) -> Result<String, RpcError> {
    let obj = args
        .as_object()
        .ok_or_else(|| RpcError::invalid_params("calm.track: arguments must be an object"))?;
    let Some(raw) = obj.get("path") else {
        if required {
            return Err(RpcError::invalid_params(
                "calm.track.cat: missing `path` (string)",
            ));
        }
        return Ok(String::new());
    };
    let path = raw
        .as_str()
        .ok_or_else(|| RpcError::invalid_params("calm.track: `path` must be a string"))?;
    Ok(normalize_path(path))
}

pub(crate) async fn resolve_track_for_identity(
    ctx: &Arc<AppContext>,
    identity: &ToolCallIdentity,
) -> Result<(Card, Track), RpcError> {
    let card_id_str = identity.card_id.as_str().to_string();
    let card = ctx
        .repo
        .card_get(&card_id_str)
        .await
        .map_err(|e| RpcError::internal(format!("track_file: card lookup: {e}")))?
        .ok_or_else(|| {
            RpcError::internal(format!(
                "track_file: bound card {card_id_str} not found (deleted mid-connection?)"
            ))
        })?;
    let track = ctx
        .repo
        .track_get(card.track_id.as_str())
        .await
        .map_err(|e| RpcError::internal(format!("track_file: track lookup: {e}")))?
        .ok_or_else(|| {
            RpcError::internal(format!(
                "track_file: track {} for card {} not found",
                card.track_id.as_str(),
                card_id_str
            ))
        })?;
    Ok((card, track))
}

fn track_fs_error_to_rpc(err: TrackFsError) -> RpcError {
    match err {
        TrackFsError::PathNotAvailable(message) => RpcError::invalid_params(message),
        TrackFsError::Forbidden(message) => RpcError::custom(-32403, message),
        TrackFsError::Internal(message) => RpcError::internal(message),
    }
}
