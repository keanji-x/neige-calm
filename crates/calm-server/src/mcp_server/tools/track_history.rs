//! Hidden MCP drill-ins for track-vcs history.
//!
//! These handlers are registered as wire-callable tools but use
//! `visible_to_roles: &[]`, so they do not appear in `tools/list` for any
//! role. Human-facing drill-in goes through `neige diff`, `neige cat-at`, and
//! `neige log`; planner turns receive the summarized since-last-turn block.

use crate::ids::TrackId;
use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{
    AppContext, ToolCallIdentity, ToolDescriptor, ToolHandler, ToolHandlerFuture, ToolRegistry,
    read_only_annotations, require_role_any,
};
use crate::mcp_server::tools::track_file::resolve_track_for_identity;
use crate::model::CardRole;
use crate::track_vcs::{self, CommitLogEntry, FileDiff};
use calm_truth::track_vcs_repo::TrackVcsRepo;
use serde_json::{Map, Value, json};
use std::sync::Arc;

pub const TOOL_TRACK_DIFF: &str = "calm.track.diff";
pub const TOOL_TRACK_CAT_AT: &str = "calm.track.cat_at";
pub const TOOL_TRACK_LOG: &str = "calm.track.log";

pub fn register_into(registry: &mut ToolRegistry) {
    registry.register(diff_descriptor(), wrap(track_diff));
    registry.register(cat_at_descriptor(), wrap(track_cat_at));
    registry.register(log_descriptor(), wrap(track_log));
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
        name: TOOL_TRACK_DIFF.into(),
        description: "Hidden drill-in: diff two commits for the current MCP-bound track. \
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
        name: TOOL_TRACK_CAT_AT.into(),
        description: "Hidden drill-in: read `{ path }` from a historical `{ commit }` \
             in the current MCP-bound track."
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
        name: TOOL_TRACK_LOG.into(),
        description: "Hidden drill-in: list recent track-vcs commits for the current \
             MCP-bound track. Arguments: `{ path?, limit? }`."
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

async fn track_diff(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role_any(&identity, &[CardRole::Planner, CardRole::Worker])?;
    let vcs = track_vcs_repo(&ctx)?;
    let (_, track) = resolve_track_for_identity(&ctx, &identity).await?;
    let obj = object_args(&args, TOOL_TRACK_DIFF)?;
    let from = required_string(obj, "from", TOOL_TRACK_DIFF)?;
    let to = optional_string(obj, "to", TOOL_TRACK_DIFF)?;
    let path = optional_string(obj, "path", TOOL_TRACK_DIFF)?;
    ensure_commit_in_track(vcs, &track.id, from).await?;
    let to = match to {
        Some(to) => {
            ensure_commit_in_track(vcs, &track.id, to).await?;
            to.to_string()
        }
        None => vcs
            .head(&track.id)
            .await
            .map_err(vcs_error_to_rpc)?
            .ok_or_else(|| {
                RpcError::invalid_params("calm.track.diff: current track has no VCS HEAD")
            })?,
    };
    let files = vcs
        .diff_with_patches(from, &to, path, track_vcs::DEFAULT_PATCH_MAX_LINES)
        .await
        .map_err(vcs_error_to_rpc)?;
    Ok(json!({
        "from": from,
        "to": to,
        "path": path,
        "files": files.into_iter().map(file_diff_json).collect::<Vec<_>>(),
    }))
}

async fn track_cat_at(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role_any(&identity, &[CardRole::Planner, CardRole::Worker])?;
    let vcs = track_vcs_repo(&ctx)?;
    let (_, track) = resolve_track_for_identity(&ctx, &identity).await?;
    let obj = object_args(&args, TOOL_TRACK_CAT_AT)?;
    let commit = required_string(obj, "commit", TOOL_TRACK_CAT_AT)?;
    let path = required_string(obj, "path", TOOL_TRACK_CAT_AT)?;
    ensure_commit_in_track(vcs, &track.id, commit).await?;
    let blob = vcs.cat_at(commit, path).await.map_err(vcs_error_to_rpc)?;
    Ok(json!({
        "commit": blob.commit,
        "path": blob.path,
        "content": blob.content,
        "content_type": blob.content_type,
    }))
}

async fn track_log(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role_any(&identity, &[CardRole::Planner, CardRole::Worker])?;
    let vcs = track_vcs_repo(&ctx)?;
    let (_, track) = resolve_track_for_identity(&ctx, &identity).await?;
    let obj = object_args(&args, TOOL_TRACK_LOG)?;
    let path = optional_string(obj, "path", TOOL_TRACK_LOG)?;
    let limit = optional_limit(obj, TOOL_TRACK_LOG)?;
    let log = vcs
        .log(&track.id, path, limit)
        .await
        .map_err(vcs_error_to_rpc)?;
    Ok(json!({
        "commits": log.commits.into_iter().map(commit_log_json).collect::<Vec<_>>(),
        "truncated": log.truncated,
    }))
}

fn track_vcs_repo(ctx: &AppContext) -> Result<&dyn TrackVcsRepo, RpcError> {
    ctx.track_vcs
        .as_deref()
        .ok_or_else(|| RpcError::internal("calm.track history requires sqlite-backed track-vcs"))
}

async fn ensure_commit_in_track(
    vcs: &dyn TrackVcsRepo,
    track_id: &TrackId,
    commit_hash: &str,
) -> Result<(), RpcError> {
    match vcs
        .commit_record(commit_hash)
        .await
        .map_err(vcs_error_to_rpc)?
    {
        Some(record) if record.track_id == *track_id => Ok(()),
        Some(_) => Err(RpcError::invalid_params(format!(
            "calm.track: commit {commit_hash} is outside the bound track"
        ))),
        None => Err(RpcError::invalid_params(format!(
            "calm.track: unknown commit {commit_hash}"
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

fn vcs_error_to_rpc(err: calm_truth::TruthError) -> RpcError {
    match err {
        calm_truth::TruthError::Core(calm_types::error::CoreError::NotFound(message))
        | calm_truth::TruthError::Core(calm_types::error::CoreError::BadRequest(message)) => {
            RpcError::invalid_params(message)
        }
        other => RpcError::internal(format!("{other}")),
    }
}
