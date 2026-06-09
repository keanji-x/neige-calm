//! Read-only MCP file views for the current wave.
//!
//! `calm.wave.ls` and `calm.wave.cat` expose a small path-based view
//! rooted at the wave bound to the caller's MCP connection. The wave is
//! always derived from [`ToolCallIdentity`]; callers never provide a
//! `wave_id`.

use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{
    AppContext, ToolCallIdentity, ToolDescriptor, ToolHandler, ToolHandlerFuture, ToolRegistry,
    read_only_annotations, require_role_any,
};
use crate::model::{Card, CardRole, Wave};
use crate::wave_fs_view::{WaveFsError, WaveFsView, normalize_path};
use serde_json::{Value, json};
use std::sync::Arc;

pub const TOOL_WAVE_LS: &str = "calm.wave.ls";
pub const TOOL_WAVE_CAT: &str = "calm.wave.cat";

pub fn register_into(registry: &mut ToolRegistry) {
    registry.register(ls_descriptor(), wrap(wave_ls));
    registry.register(cat_descriptor(), wrap(wave_cat));
}

fn wrap<F, Fut>(f: F) -> ToolHandler
where
    F: Fn(Arc<AppContext>, ToolCallIdentity, Value) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<Value, RpcError>> + Send + 'static,
{
    Arc::new(move |ctx, identity, args| -> ToolHandlerFuture { Box::pin(f(ctx, identity, args)) })
}

/// Return-shape contract consumed by `neige`: `calm.wave.ls` returns a bare
/// JSON array of `{ name, kind, ... }` entries; `calm.wave.cat` returns an
/// object `{ content, content_type }`.
fn ls_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_WAVE_LS.into(),
        description: "Spec/Worker: list file-like read views for the current MCP-bound wave. \
             Accepts optional `{ path }`; `/` lists `index.md`, `wave.json`, \
             `report.md`, `cards/`, and `runs/`; `cards/<card_id>` lists \
             `meta.json`, `payload.json`, `events.json`, and `conversation.md`. \
             The wave is derived from the bound \
             card identity, never from arguments."
            .into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" }
            }
        }),
        annotations: Some(read_only_annotations()),
    }
}

fn cat_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_WAVE_CAT.into(),
        description: "Spec/Worker: read one file-like view from the current MCP-bound wave. \
             Supports `index.md`, `wave.json`, `report.md`, `cards/index.json`, \
             `cards/<card_id>/meta.json`, `cards/<card_id>/payload.json`, \
             `cards/<card_id>/events.json`, `cards/<card_id>/conversation.md`, \
             `runs/index.json`, `runs/<idempotency_key>.md`, and \
             `runs/<idempotency_key>.json`."
            .into(),
        input_schema: json!({
            "type": "object",
            "required": ["path"],
            "properties": {
                "path": { "type": "string" }
            }
        }),
        annotations: Some(read_only_annotations()),
    }
}

async fn wave_ls(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role_any(&identity, &[CardRole::Spec, CardRole::Worker])?;
    let path = parse_path_arg(&args, false)?;
    let (_, wave) = resolve_wave_for_identity(&ctx, &identity).await?;
    let view = WaveFsView::new(ctx.repo.as_ref(), &ctx.write);
    let entries = view
        .ls(&wave, Some(path.as_str()))
        .await
        .map_err(wave_fs_error_to_rpc)?;
    serde_json::to_value(entries)
        .map_err(|e| RpcError::internal(format!("wave_file: json serialization: {e}")))
}

async fn wave_cat(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role_any(&identity, &[CardRole::Spec, CardRole::Worker])?;
    let path = parse_path_arg(&args, true)?;
    let (_, wave) = resolve_wave_for_identity(&ctx, &identity).await?;
    let view = WaveFsView::new(ctx.repo.as_ref(), &ctx.write);
    let content = view
        .cat(&wave, path.as_str())
        .await
        .map_err(wave_fs_error_to_rpc)?;
    serde_json::to_value(content)
        .map_err(|e| RpcError::internal(format!("wave_file: json serialization: {e}")))
}

fn parse_path_arg(args: &Value, required: bool) -> Result<String, RpcError> {
    let obj = args
        .as_object()
        .ok_or_else(|| RpcError::invalid_params("calm.wave: arguments must be an object"))?;
    let Some(raw) = obj.get("path") else {
        if required {
            return Err(RpcError::invalid_params(
                "calm.wave.cat: missing `path` (string)",
            ));
        }
        return Ok(String::new());
    };
    let path = raw
        .as_str()
        .ok_or_else(|| RpcError::invalid_params("calm.wave: `path` must be a string"))?;
    Ok(normalize_path(path))
}

async fn resolve_wave_for_identity(
    ctx: &Arc<AppContext>,
    identity: &ToolCallIdentity,
) -> Result<(Card, Wave), RpcError> {
    let card_id_str = identity.card_id.as_str().to_string();
    let card = ctx
        .repo
        .card_get(&card_id_str)
        .await
        .map_err(|e| RpcError::internal(format!("wave_file: card lookup: {e}")))?
        .ok_or_else(|| {
            RpcError::internal(format!(
                "wave_file: bound card {card_id_str} not found (deleted mid-connection?)"
            ))
        })?;
    let wave = ctx
        .repo
        .wave_get(card.wave_id.as_str())
        .await
        .map_err(|e| RpcError::internal(format!("wave_file: wave lookup: {e}")))?
        .ok_or_else(|| {
            RpcError::internal(format!(
                "wave_file: wave {} for card {} not found",
                card.wave_id.as_str(),
                card_id_str
            ))
        })?;
    Ok((card, wave))
}

fn wave_fs_error_to_rpc(err: WaveFsError) -> RpcError {
    match err {
        WaveFsError::PathNotAvailable(message) => RpcError::invalid_params(message),
        WaveFsError::Forbidden(message) => RpcError::custom(-32403, message),
        WaveFsError::Internal(message) => RpcError::internal(message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptors_document_spec_worker_access() {
        let ls = ls_descriptor();
        let cat = cat_descriptor();

        assert!(
            ls.description.starts_with("Spec/Worker:"),
            "ls descriptor should advertise Spec/Worker access: {}",
            ls.description
        );
        assert!(
            cat.description.starts_with("Spec/Worker:"),
            "cat descriptor should advertise Spec/Worker access: {}",
            cat.description
        );
        assert!(
            !ls.description.contains("Spec-only") && !cat.description.contains("Spec-only"),
            "wave file descriptors must not claim spec-only access"
        );
    }
}
