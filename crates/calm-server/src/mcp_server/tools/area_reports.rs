//! MCP side of `area/reports/` (#1838 S2): the `area/` branch of `neige_track_ls` / `neige_track_cat`
//! and the CLI-only `neige_report_find`. Planner only; the area is the caller's `identity.area_id`,
//! never an argument. Listing, resolving and reading live in [`crate::area_reports`].

use std::sync::Arc;

use serde_json::{Value, json};

use super::track_file::{Selection, not_a_report, report_blocks_content, track_fs_error_to_rpc};
use crate::area_reports::{self, AreaPath, Filter, REPORTS_DIR};
use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{
    AppContext, ToolCallIdentity, ToolDescriptor, ToolHandler, ToolHandlerFuture, ToolRegistry,
    read_only_annotations, require_role_any,
};
use crate::model::CardRole;
use crate::track_fs_view::{TrackFsEntry, TrackFsError, normalize_path};

pub const TOOL_REPORT_FIND: &str = "neige_report_find";

const FIND_KEYS: &[&str] = &["path", "name", "tag"];

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
        visible_to_roles: &[],
    }
}

/// Other tracks' reports are the Planner's to read; a Worker is Forbidden. (Roles outside the track
/// views' Planner|Worker are refused earlier, by the same role gate as `neige_track_ls`.)
fn require_planner(identity: &ToolCallIdentity) -> Result<(), RpcError> {
    if identity.role == CardRole::Planner {
        return Ok(());
    }
    Err(track_fs_error_to_rpc(TrackFsError::Forbidden(format!(
        "{REPORTS_DIR}/ is the Planner's view of this area's reports; a {:?} reads only its own track",
        identity.role
    ))))
}

fn pool(ctx: &AppContext) -> Result<&sqlx::SqlitePool, RpcError> {
    ctx.sqlite_pool
        .as_ref()
        .ok_or_else(|| RpcError::internal("area reports: requires a sqlite-backed repo"))
}

/// `neige_track_ls` on a path under `area/`.
pub(crate) async fn ls(
    ctx: &AppContext,
    identity: &ToolCallIdentity,
    path: Result<AreaPath<'_>, String>,
) -> Result<Value, RpcError> {
    require_planner(identity)?;
    let entries = match path.map_err(RpcError::invalid_params)? {
        AreaPath::Root => serde_json::to_value(vec![TrackFsEntry {
            name: "reports/".into(),
            kind: "dir".into(),
            size: None,
            updated_at: None,
            extra: serde_json::Map::new(),
        }]),
        AreaPath::Reports => serde_json::to_value(
            area_reports::list(pool(ctx)?, &identity.area_id, &Filter::default())
                .await
                .map_err(track_fs_error_to_rpc)?,
        ),
        AreaPath::Report(file) => {
            return Err(RpcError::invalid_params(format!(
                "{REPORTS_DIR}/{file} is a report, not a directory; read it with `neige track cat`"
            )));
        }
    };
    entries.map_err(|e| RpcError::internal(format!("area reports: json serialization: {e}")))
}

/// `neige_track_cat` on `raw` (classified as `path`), a path under `area/`; `selection` narrows a report
/// to those blocks (#1874) or sections (#1877).
pub(crate) async fn cat(
    ctx: &AppContext,
    identity: &ToolCallIdentity,
    raw: &str,
    path: Result<AreaPath<'_>, String>,
    selection: Option<&Selection>,
) -> Result<Value, RpcError> {
    require_planner(identity)?;
    if let Some(selection) = selection {
        let Ok(AreaPath::Report(file)) = path else {
            return Err(not_a_report(raw, selection));
        };
        let blocks = area_reports::read_blocks(pool(ctx)?, &identity.area_id, file)
            .await
            .map_err(track_fs_error_to_rpc)?;
        let ids = selection.block_ids(&blocks)?;
        return report_blocks_content(&blocks, &ids);
    }
    let file = match path.map_err(RpcError::invalid_params)? {
        AreaPath::Report(file) => file,
        dir @ (AreaPath::Root | AreaPath::Reports) => {
            let dir = if dir == AreaPath::Root {
                "area"
            } else {
                REPORTS_DIR
            };
            return Err(RpcError::invalid_params(format!(
                "`{dir}/` is a directory; list it with `neige track ls {dir}/`"
            )));
        }
    };
    let content = area_reports::read(pool(ctx)?, &identity.area_id, file)
        .await
        .map_err(track_fs_error_to_rpc)?;
    serde_json::to_value(content)
        .map_err(|e| RpcError::internal(format!("area reports: json serialization: {e}")))
}

async fn report_find(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role_any(&identity, &[CardRole::Planner, CardRole::Worker])?;
    require_planner(&identity)?;
    let tool = TOOL_REPORT_FIND;
    let obj = args
        .as_object()
        .ok_or_else(|| RpcError::invalid_params(format!("{tool}: arguments must be an object")))?;
    if let Some(key) = obj.keys().find(|key| !FIND_KEYS.contains(&key.as_str())) {
        return Err(RpcError::invalid_params(format!(
            "{tool}: unknown argument `{key}`; the searched area is always the caller's own"
        )));
    }
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
    if normalize_path(&path) != REPORTS_DIR {
        return Err(RpcError::invalid_params(format!(
            "{tool}: only `{REPORTS_DIR}/` can be searched; got `{path}`"
        )));
    }
    let filter = Filter {
        name: text("name")?,
        tag: text("tag")?,
    };
    let entries = area_reports::list(pool(&ctx)?, &identity.area_id, &filter)
        .await
        .map_err(track_fs_error_to_rpc)?;
    serde_json::to_value(entries)
        .map_err(|e| RpcError::internal(format!("{tool}: json serialization: {e}")))
}
