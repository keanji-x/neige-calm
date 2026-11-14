//! Planner-only Report search; navigation is delegated to report_paths.

use std::sync::Arc;

use serde_json::{Value, json};

use super::track_file::track_fs_error_to_rpc;
use crate::area_reports::{self, Filter};
use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{
    AppContext, ToolCallIdentity, ToolDescriptor, ToolHandler, ToolHandlerFuture, ToolRegistry,
    read_only_annotations,
};
use crate::model::CardRole;

pub const TOOL_REPORT_FIND: &str = "neige_report_find";

pub fn register_into(registry: &mut ToolRegistry) {
    registry.register(find_descriptor(), wrap(report_find));
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

/// Served through `neige report find`, like the other `neige` views: hidden from every role's `tools/list`.
fn find_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_REPORT_FIND.into(),
        description: include_str!("../../../prompts/tools/neige_report_find.md")
            .trim_end()
            .to_string(),
        input_schema: json!({
            "type": "object",
            "required": ["path"],
            "properties": {
                "path": { "type": "string" },
                "name": { "type": "string" },
                "tag": { "type": "string" }
            },
            "additionalProperties": false
        }),
        annotations: Some(read_only_annotations()),
        roles: &[CardRole::Planner],
        listed_for: &[],
    }
}

fn pool(ctx: &AppContext) -> Result<&sqlx::SqlitePool, RpcError> {
    ctx.sqlite_db
        .as_ref()
        .map(crate::db::sqlite::SqliteDb::read_pool)
        .ok_or_else(|| RpcError::internal("area reports: requires a sqlite-backed repo"))
}

async fn report_find(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    let tool = TOOL_REPORT_FIND;
    let obj = args
        .as_object()
        .ok_or_else(|| RpcError::invalid_params(format!("{tool}: arguments must be an object")))?;
    let text = |key: &str| -> Result<Option<String>, RpcError> {
        match obj.get(key) {
            None => Ok(None),
            Some(Value::String(value)) => Ok(Some(value.clone())),
            Some(_) => Err(RpcError::invalid_params(format!(
                "{tool}: `{key}` must be a string"
            ))),
        }
    };
    let path = text("path")?
        .ok_or_else(|| RpcError::invalid_params(format!("{tool}: missing `path` (string)")))?;
    let (bound, _) = crate::managed_track::report_browse_context(&ctx, &identity).await?;
    let scope = crate::report_paths::resolve(&ctx, &identity, &bound, &path).await?;
    if scope != crate::report_paths::Scope::Area(bound.area_id.to_string()) {
        return Err(RpcError::invalid_params(
            "find requires the current user Area directory",
        ));
    }
    let filter = Filter {
        name: text("name")?,
        tag: text("tag")?,
    };
    let entries = area_reports::find(&ctx, pool(&ctx)?, &identity.area_id, &filter)
        .await
        .map_err(track_fs_error_to_rpc)?;
    serde_json::to_value(entries)
        .map(|reports| json!({ "reports": reports }))
        .map_err(|e| RpcError::internal(format!("{tool}: json serialization: {e}")))
}
