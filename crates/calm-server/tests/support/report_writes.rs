//! Report writes the way an agent makes them (#1883): a full `neige_report_read` by the writing
//! session, which anchors the write, then the write tool itself. No revision is passed.

use std::sync::Arc;

use calm_server::mcp_server::registry::AppContext;
use calm_server::mcp_server::tools::track_report::TOOL_REPORT_READ;
use calm_server::mcp_server::tools::track_report_blocks::{TOOL_REPORT_COMMIT, TOOL_REPORT_WRITE};
use calm_server::mcp_server::{ToolCallIdentity, ToolRegistry};
use calm_server::plugin_host::mcp::RpcError;
use serde_json::{Value, json};

async fn call(
    ctx: &Arc<AppContext>,
    registry: &ToolRegistry,
    name: &str,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    let handler = registry
        .lookup(name)
        .unwrap_or_else(|| panic!("tool not registered: {name}"));
    handler(ctx.clone(), identity, args)
        .await
        .map(calm_server::mcp_server::result::ToolResult::into_structured)
}

async fn read_all(
    ctx: &Arc<AppContext>,
    registry: &ToolRegistry,
    identity: ToolCallIdentity,
) -> Value {
    call(ctx, registry, TOOL_REPORT_READ, identity, json!({}))
        .await
        .expect("read the report before writing it")
}

/// One `upsert` op committed after a full read; a `message` / `lifecycle` key in `op` moves to the
/// commit. Returns the commit result plus the written block's `id` and `rev`.
pub async fn upsert_block(
    ctx: &Arc<AppContext>,
    registry: &ToolRegistry,
    identity: ToolCallIdentity,
    mut op: Value,
) -> Result<Value, RpcError> {
    let before = read_all(ctx, registry, identity.clone()).await;
    let fields = op.as_object_mut().expect("an op object");
    let mut args = json!({ "message": "upsert one block" });
    for key in ["message", "lifecycle"] {
        if let Some(value) = fields.remove(key) {
            args[key] = value;
        }
    }
    fields.insert("op".into(), json!("upsert"));
    let target = fields.get("id").cloned();
    args["ops"] = json!([op]);
    let mut out = call(ctx, registry, TOOL_REPORT_COMMIT, identity, args).await?;
    let known = before["blocks"].as_array().expect("read returns the index");
    let block = out["blocks"]
        .as_array()
        .expect("commit returns the index")
        .iter()
        .find(|block| match &target {
            Some(id) => &block["id"] == id,
            None => !known.iter().any(|seen| seen["id"] == block["id"]),
        })
        .cloned()
        .expect("the written block is in the index");
    out["id"] = block["id"].clone();
    out["rev"] = block["rev"].clone();
    Ok(out)
}

/// `neige_report_commit` of `ops` after a full read.
pub async fn read_then_commit(
    ctx: &Arc<AppContext>,
    registry: &ToolRegistry,
    identity: ToolCallIdentity,
    ops: Value,
) -> Result<Value, RpcError> {
    read_all(ctx, registry, identity.clone()).await;
    let args = json!({ "message": "test commit", "ops": ops });
    call(ctx, registry, TOOL_REPORT_COMMIT, identity, args).await
}

/// `neige_report_write` of `args` after a full read.
pub async fn read_then_write_markdown(
    ctx: &Arc<AppContext>,
    registry: &ToolRegistry,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    read_all(ctx, registry, identity.clone()).await;
    call(ctx, registry, TOOL_REPORT_WRITE, identity, args).await
}
