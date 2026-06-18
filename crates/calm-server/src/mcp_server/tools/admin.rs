//! Hidden MCP admin maintenance tools.
//!
//! These handlers are registered as wire-callable tools but use
//! `visible_to_roles: &[]`, so they do not appear in `tools/list` for any
//! role. Human-facing access goes through the `neige` maintenance commands.

use crate::ids::WaveId;
use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{
    AppContext, ToolCallIdentity, ToolDescriptor, ToolHandler, ToolHandlerFuture, ToolRegistry,
    require_role,
};
use crate::mcp_server::tools::wave_file::resolve_wave_for_identity;
use crate::model::CardRole;
use calm_truth::wave_vcs_repo::WaveVcsRepo;
use serde_json::{Value, json};
use std::sync::Arc;

pub const TOOL_ADMIN_WAVE_GC: &str = "calm.admin.wave_gc";
pub const TOOL_ADMIN_VACUUM: &str = "calm.admin.vacuum";

pub fn register_into(registry: &mut ToolRegistry) {
    registry.register(wave_gc_descriptor(), wrap(wave_gc));
    registry.register(vacuum_descriptor(), wrap(vacuum));
}

fn wrap<F, Fut>(f: F) -> ToolHandler
where
    F: Fn(Arc<AppContext>, ToolCallIdentity, Value) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<Value, RpcError>> + Send + 'static,
{
    Arc::new(move |ctx, identity, args| -> ToolHandlerFuture { Box::pin(f(ctx, identity, args)) })
}

fn wave_gc_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_ADMIN_WAVE_GC.into(),
        description: "Hidden admin: prune the MCP-bound wave's VCS history (keep the last \
             `keep` commits + all active-session endpoints) then sweep unreferenced objects. \
             Arguments: `{ wave_id, keep, dry_run? }`. `wave_id` MUST equal the caller's bound \
             wave (guardrail against wrong-wave GC). `dry_run` reports counts without deleting. \
             Returns `{ wave_id, keep, dry_run, pruned_commits, swept_objects }`."
            .into(),
        input_schema: json!({
            "type": "object",
            "required": ["wave_id", "keep"],
            "properties": {
                "wave_id": { "type": "string", "minLength": 1 },
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

async fn wave_gc(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Spec)?;
    let wave_vcs = wave_vcs_repo(&ctx)?;
    let (_card, wave) = resolve_wave_for_identity(&ctx, &identity).await?;

    let obj = args.as_object().ok_or_else(|| {
        RpcError::invalid_params("calm.admin.wave_gc: arguments must be an object")
    })?;
    let wave_id = obj
        .get("wave_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| RpcError::invalid_params("calm.admin.wave_gc: `wave_id` required"))?;
    if wave_id != wave.id.as_str() {
        return Err(RpcError::invalid_params(format!(
            "calm.admin.wave_gc: wave_id `{wave_id}` does not match the caller's bound wave `{}`",
            wave.id.as_str()
        )));
    }
    let keep = obj
        .get("keep")
        .and_then(Value::as_u64)
        .filter(|keep| *keep > 0)
        .ok_or_else(|| {
            RpcError::invalid_params("calm.admin.wave_gc: `keep` must be a positive integer")
        })? as usize;
    let dry_run = obj.get("dry_run").and_then(Value::as_bool).unwrap_or(false);

    let wave_ref: WaveId = WaveId::from(wave_id);

    if dry_run {
        let pruned = wave_vcs
            .prune_wave_history(&wave_ref, keep, true)
            .await
            .map_err(|e| RpcError::internal(format!("calm.admin.wave_gc: prune: {e}")))?;
        return Ok(json!({
            "wave_id": wave_id, "keep": keep, "dry_run": true,
            "pruned_commits": pruned, "swept_objects": 0
        }));
    }

    let pruned = wave_vcs
        .prune_wave_history(&wave_ref, keep, false)
        .await
        .map_err(|e| RpcError::internal(format!("calm.admin.wave_gc: prune: {e}")))?;
    let swept = wave_vcs
        .sweep_unreferenced_objects()
        .await
        .map_err(|e| RpcError::internal(format!("calm.admin.wave_gc: sweep: {e}")))?;

    Ok(json!({
        "wave_id": wave_id, "keep": keep, "dry_run": false,
        "pruned_commits": pruned, "swept_objects": swept
    }))
}

async fn vacuum(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    _args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Spec)?;
    let wave_vcs = wave_vcs_repo(&ctx)?;
    wave_vcs.vacuum().await.map_err(|e| {
        RpcError::internal(format!(
            "calm.admin.vacuum: VACUUM failed (db locked?): {e}"
        ))
    })?;
    Ok(json!({ "ok": true }))
}

fn wave_vcs_repo(ctx: &AppContext) -> Result<&dyn WaveVcsRepo, RpcError> {
    ctx.wave_vcs
        .as_deref()
        .ok_or_else(|| RpcError::internal("calm.admin requires sqlite-backed wave-vcs"))
}
