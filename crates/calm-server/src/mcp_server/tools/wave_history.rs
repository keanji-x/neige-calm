//! Hidden MCP drill-ins for wave-vcs history.
//!
//! These handlers are registered as wire-callable tools but use
//! `visible_to_roles: &[]`, so they do not appear in `tools/list` for any
//! role. Human-facing drill-in goes through `neige diff`, `neige cat-at`, and
//! `neige log`; spec turns receive the summarized since-last-turn block.

use crate::error::CalmError;
use crate::ids::WaveId;
use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{
    AppContext, ToolCallIdentity, ToolDescriptor, ToolHandler, ToolHandlerFuture, ToolRegistry,
    read_only_annotations, require_role_any,
};
use crate::mcp_server::tools::wave_file::resolve_wave_for_identity;
use crate::model::CardRole;
use crate::wave_vcs::{self, CommitLogEntry, FileDiff};
use serde_json::{Map, Value, json};
use sqlx::SqlitePool;
use std::sync::Arc;

pub const TOOL_WAVE_DIFF: &str = "calm.wave.diff";
pub const TOOL_WAVE_CAT_AT: &str = "calm.wave.cat_at";
pub const TOOL_WAVE_LOG: &str = "calm.wave.log";

pub fn register_into(registry: &mut ToolRegistry) {
    registry.register(diff_descriptor(), wrap(wave_diff));
    registry.register(cat_at_descriptor(), wrap(wave_cat_at));
    registry.register(log_descriptor(), wrap(wave_log));
}

fn wrap<F, Fut>(f: F) -> ToolHandler
where
    F: Fn(Arc<AppContext>, ToolCallIdentity, Value) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<Value, RpcError>> + Send + 'static,
{
    Arc::new(move |ctx, identity, args| -> ToolHandlerFuture { Box::pin(f(ctx, identity, args)) })
}

fn diff_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_WAVE_DIFF.into(),
        description: "Hidden drill-in: diff two commits for the current MCP-bound wave. \
             Arguments: `{ from, to?, path? }`; `to` defaults to current HEAD. \
             Text blobs include unified patch hunks."
            .into(),
        input_schema: json!({
            "type": "object",
            "required": ["from"],
            "properties": {
                "from": { "type": "string" },
                "to": { "type": "string" },
                "path": { "type": "string" }
            }
        }),
        annotations: Some(read_only_annotations()),
        visible_to_roles: &[],
    }
}

fn cat_at_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_WAVE_CAT_AT.into(),
        description: "Hidden drill-in: read `{ path }` from a historical `{ commit }` \
             in the current MCP-bound wave."
            .into(),
        input_schema: json!({
            "type": "object",
            "required": ["commit", "path"],
            "properties": {
                "commit": { "type": "string" },
                "path": { "type": "string" }
            }
        }),
        annotations: Some(read_only_annotations()),
        visible_to_roles: &[],
    }
}

fn log_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_WAVE_LOG.into(),
        description: "Hidden drill-in: list recent wave-vcs commits for the current \
             MCP-bound wave. Arguments: `{ path?, limit? }`."
            .into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "limit": { "type": "integer", "minimum": 1, "maximum": 200 }
            }
        }),
        annotations: Some(read_only_annotations()),
        visible_to_roles: &[],
    }
}

async fn wave_diff(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role_any(&identity, &[CardRole::Spec, CardRole::Worker])?;
    let pool = wave_vcs_pool(&ctx)?;
    let (_, wave) = resolve_wave_for_identity(&ctx, &identity).await?;
    let obj = object_args(&args, TOOL_WAVE_DIFF)?;
    let from = required_string(obj, "from", TOOL_WAVE_DIFF)?;
    let to = optional_string(obj, "to", TOOL_WAVE_DIFF)?;
    let path = optional_string(obj, "path", TOOL_WAVE_DIFF)?;
    ensure_commit_in_wave(pool, &wave.id, from).await?;
    let to = match to {
        Some(to) => {
            ensure_commit_in_wave(pool, &wave.id, to).await?;
            to.to_string()
        }
        None => wave_vcs::head(pool, &wave.id)
            .await
            .map_err(vcs_error_to_rpc)?
            .ok_or_else(|| {
                RpcError::invalid_params("calm.wave.diff: current wave has no VCS HEAD")
            })?,
    };
    let files =
        wave_vcs::diff_with_patches(pool, from, &to, path, wave_vcs::DEFAULT_PATCH_MAX_LINES)
            .await
            .map_err(vcs_error_to_rpc)?;
    Ok(json!({
        "from": from,
        "to": to,
        "path": path,
        "files": files.into_iter().map(file_diff_json).collect::<Vec<_>>(),
    }))
}

async fn wave_cat_at(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role_any(&identity, &[CardRole::Spec, CardRole::Worker])?;
    let pool = wave_vcs_pool(&ctx)?;
    let (_, wave) = resolve_wave_for_identity(&ctx, &identity).await?;
    let obj = object_args(&args, TOOL_WAVE_CAT_AT)?;
    let commit = required_string(obj, "commit", TOOL_WAVE_CAT_AT)?;
    let path = required_string(obj, "path", TOOL_WAVE_CAT_AT)?;
    ensure_commit_in_wave(pool, &wave.id, commit).await?;
    let blob = wave_vcs::cat_at(pool, commit, path)
        .await
        .map_err(vcs_error_to_rpc)?;
    Ok(json!({
        "commit": blob.commit,
        "path": blob.path,
        "content": blob.content,
        "content_type": blob.content_type,
    }))
}

async fn wave_log(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role_any(&identity, &[CardRole::Spec, CardRole::Worker])?;
    let pool = wave_vcs_pool(&ctx)?;
    let (_, wave) = resolve_wave_for_identity(&ctx, &identity).await?;
    let obj = object_args(&args, TOOL_WAVE_LOG)?;
    let path = optional_string(obj, "path", TOOL_WAVE_LOG)?;
    let limit = optional_limit(obj, TOOL_WAVE_LOG)?;
    let log = wave_vcs::log(pool, &wave.id, path, limit)
        .await
        .map_err(vcs_error_to_rpc)?;
    Ok(json!({
        "commits": log.commits.into_iter().map(commit_log_json).collect::<Vec<_>>(),
        "truncated": log.truncated,
    }))
}

fn wave_vcs_pool(ctx: &AppContext) -> Result<&SqlitePool, RpcError> {
    ctx.wave_vcs_pool
        .as_ref()
        .ok_or_else(|| RpcError::internal("calm.wave history requires sqlite-backed wave-vcs"))
}

async fn ensure_commit_in_wave(
    pool: &SqlitePool,
    wave_id: &WaveId,
    commit_hash: &str,
) -> Result<(), RpcError> {
    match wave_vcs::commit_record(pool, commit_hash)
        .await
        .map_err(vcs_error_to_rpc)?
    {
        Some(record) if record.wave_id == *wave_id => Ok(()),
        Some(_) => Err(RpcError::invalid_params(format!(
            "calm.wave: commit {commit_hash} is outside the bound wave"
        ))),
        None => Err(RpcError::invalid_params(format!(
            "calm.wave: unknown commit {commit_hash}"
        ))),
    }
}

fn object_args<'a>(args: &'a Value, tool: &str) -> Result<&'a Map<String, Value>, RpcError> {
    args.as_object()
        .ok_or_else(|| RpcError::invalid_params(format!("{tool}: arguments must be an object")))
}

fn required_string<'a>(
    obj: &'a Map<String, Value>,
    key: &str,
    tool: &str,
) -> Result<&'a str, RpcError> {
    obj.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| RpcError::invalid_params(format!("{tool}: missing `{key}` (string)")))
}

fn optional_string<'a>(
    obj: &'a Map<String, Value>,
    key: &str,
    tool: &str,
) -> Result<Option<&'a str>, RpcError> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if value.is_empty() => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.as_str())),
        Some(_) => Err(RpcError::invalid_params(format!(
            "{tool}: `{key}` must be a string if provided"
        ))),
    }
}

fn optional_limit(obj: &Map<String, Value>, tool: &str) -> Result<usize, RpcError> {
    match obj.get("limit") {
        None | Some(Value::Null) => Ok(50),
        Some(Value::Number(number)) => {
            let Some(limit) = number.as_u64() else {
                return Err(RpcError::invalid_params(format!(
                    "{tool}: `limit` must be a positive integer"
                )));
            };
            Ok((limit as usize).clamp(1, 200))
        }
        Some(_) => Err(RpcError::invalid_params(format!(
            "{tool}: `limit` must be an integer if provided"
        ))),
    }
}

fn file_diff_json(diff: FileDiff) -> Value {
    json!({
        "path": diff.path,
        "status": diff.status.wire_label(),
        "old_hash": diff.old_hash,
        "new_hash": diff.new_hash,
        "old_content_type": diff.old_content_type,
        "new_content_type": diff.new_content_type,
        "patch": diff.patch,
        "patch_truncated": diff.patch_truncated,
    })
}

fn commit_log_json(commit: CommitLogEntry) -> Value {
    json!({
        "hash": commit.hash,
        "parent_hash": commit.parent_hash,
        "lifecycle": commit.lifecycle,
        "event_id": commit.event_id,
        "created_at": commit.created_at,
        "message": commit.message,
        "changed_paths": commit.changed_paths,
    })
}

fn vcs_error_to_rpc(err: CalmError) -> RpcError {
    match err {
        CalmError::NotFound(message) | CalmError::BadRequest(message) => {
            RpcError::invalid_params(message)
        }
        other => RpcError::internal(format!("{other}")),
    }
}
