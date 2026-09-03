//! Hidden MCP admin maintenance tools.
//!
//! These handlers are registered as wire-callable tools but use
//! `visible_to_roles: &[]`, so they do not appear in `tools/list` for any
//! role. Human-facing access goes through the `neige` maintenance commands.

use crate::ids::TrackId;
use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{
    AppContext, ToolCallIdentity, ToolDescriptor, ToolHandler, ToolHandlerFuture, ToolRegistry,
    require_role,
};
use crate::mcp_server::tools::track_file::resolve_track_for_identity;
use crate::model::CardRole;
use calm_truth::track_vcs_repo::TrackVcsRepo;
use serde_json::{Value, json};
use std::sync::Arc;

pub const TOOL_ADMIN_TRACK_GC: &str = "calm.admin.track_gc";
pub const TOOL_ADMIN_VACUUM: &str = "calm.admin.vacuum";

pub fn register_into(registry: &mut ToolRegistry) {
    registry.register(track_gc_descriptor(), wrap(track_gc));
    registry.register(vacuum_descriptor(), wrap(vacuum));
}

fn wrap<F, Fut>(f: F) -> ToolHandler
where
    F: Fn(Arc<AppContext>, ToolCallIdentity, Value) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<Value, RpcError>> + Send + 'static,
{
    Arc::new(move |ctx, identity, args| -> ToolHandlerFuture { Box::pin(f(ctx, identity, args)) })
}

fn track_gc_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_ADMIN_TRACK_GC.into(),
        description: "Hidden admin: prune the MCP-bound track's VCS history (keep the last \
             `keep` commits + all active-session endpoints) then sweep unreferenced objects. \
             Arguments: `{ track_id, keep, dry_run? }`. `track_id` MUST equal the caller's bound \
             track (guardrail against wrong-track GC). `dry_run` reports counts without deleting. \
             Returns `{ track_id, keep, dry_run, pruned_commits, swept_objects }`."
            .into(),
        input_schema: json!({
            "type": "object",
            "required": ["track_id", "keep"],
            "properties": {
                "track_id": { "type": "string", "minLength": 1 },
                "keep":    { "type": "integer", "minimum": 1 },
                "dry_run": { "type": "boolean" }
            }
        }),
        annotations: None,
        visible_to_roles: &[],
    }
}

fn vacuum_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_ADMIN_VACUUM.into(),
        description: "Hidden admin: run a full SQLite VACUUM to reclaim freed pages to the OS. \
             Takes a write lock on the DB and serializes with all writers; run only in a quiet \
             maintenance window. Arguments: `{}`. Returns `{ ok: true }`."
            .into(),
        input_schema: json!({ "type": "object", "properties": {} }),
        annotations: None,
        visible_to_roles: &[],
    }
}

async fn track_gc(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Spec)?;
    let track_vcs = track_vcs_repo(&ctx)?;
    let (_card, track) = resolve_track_for_identity(&ctx, &identity).await?;

    let obj = args.as_object().ok_or_else(|| {
        RpcError::invalid_params("calm.admin.track_gc: arguments must be an object")
    })?;
    let track_id = obj
        .get("track_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| RpcError::invalid_params("calm.admin.track_gc: `track_id` required"))?;
    if track_id != track.id.as_str() {
        return Err(RpcError::invalid_params(format!(
            "calm.admin.track_gc: track_id `{track_id}` does not match the caller's bound track `{}`",
            track.id.as_str()
        )));
    }
    let keep = obj
        .get("keep")
        .and_then(Value::as_u64)
        .filter(|keep| *keep > 0)
        .ok_or_else(|| {
            RpcError::invalid_params("calm.admin.track_gc: `keep` must be a positive integer")
        })? as usize;
    let dry_run = obj.get("dry_run").and_then(Value::as_bool).unwrap_or(false);

    let track_ref: TrackId = TrackId::from(track_id);

    if dry_run {
        let pruned = track_vcs
            .prune_track_history(&track_ref, keep, true)
            .await
            .map_err(|e| RpcError::internal(format!("calm.admin.track_gc: prune: {e}")))?;
        return Ok(json!({
            "track_id": track_id, "keep": keep, "dry_run": true,
            "pruned_commits": pruned, "swept_objects": 0
        }));
    }

    let pruned = track_vcs
        .prune_track_history(&track_ref, keep, false)
        .await
        .map_err(|e| RpcError::internal(format!("calm.admin.track_gc: prune: {e}")))?;
    let swept = track_vcs
        .sweep_unreferenced_objects()
        .await
        .map_err(|e| RpcError::internal(format!("calm.admin.track_gc: sweep: {e}")))?;

    Ok(json!({
        "track_id": track_id, "keep": keep, "dry_run": false,
        "pruned_commits": pruned, "swept_objects": swept
    }))
}

async fn vacuum(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    _args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Spec)?;
    let track_vcs = track_vcs_repo(&ctx)?;
    track_vcs.vacuum().await.map_err(|e| {
        RpcError::internal(format!(
            "calm.admin.vacuum: VACUUM failed (db locked?): {e}"
        ))
    })?;
    Ok(json!({ "ok": true }))
}

fn track_vcs_repo(ctx: &AppContext) -> Result<&dyn TrackVcsRepo, RpcError> {
    ctx.track_vcs
        .as_deref()
        .ok_or_else(|| RpcError::internal("calm.admin requires sqlite-backed track-vcs"))
}
