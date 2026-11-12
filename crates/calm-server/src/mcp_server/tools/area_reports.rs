//! MCP side of `area/reports/` (#1838 S2): the `area/` branch of `neige_ls` / `neige_cat`
//! and the Planner-only `neige_report_find`. Browsing uses the caller's `identity.area_id`,
//! never an argument. Listing, resolving and reading live in [`crate::area_reports`].

use std::sync::Arc;

use serde_json::{Value, json};

use super::track_file::track_fs_error_to_rpc;
use crate::area_reports::{self, AreaPath, Filter, REPORTS_DIR};
use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{
    AppContext, ToolCallIdentity, ToolDescriptor, ToolHandler, ToolHandlerFuture, ToolRegistry,
    read_only_annotations,
};
use crate::model::{CardRole, Track};
use crate::track_fs_view::{TrackFsEntry, normalize_path};

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

/// `neige_ls` on a path under `area/`.
pub(crate) async fn ls(
    ctx: &AppContext,
    bound: &Track,
    path: AreaPath<'_>,
) -> Result<Value, RpcError> {
    let entries = match path {
        AreaPath::Root => {
            return super::track_file::entries_result(vec![TrackFsEntry {
                name: "reports/".into(),
                kind: "dir".into(),
                size: None,
                updated_at: None,
                extra: serde_json::Map::new(),
            }]);
        }
        AreaPath::Reports => {
            let mut reports =
                area_reports::list(pool(ctx)?, bound.area_id.as_str(), &Filter::default())
                    .await
                    .map_err(track_fs_error_to_rpc)?;
            let user = super::unified_browse::is_user_area(ctx, bound.area_id.as_str()).await?;
            if !user {
                reports.retain(|r| Some(r.track_id.as_str()) == Some(bound.id.as_str()));
            }
            serde_json::to_value(reports).map(|reports| json!({"reports":reports}))
        }
        AreaPath::Report(file) => {
            return Err(RpcError::invalid_params(format!(
                "{REPORTS_DIR}/{file} is a report, not a directory; read it with `neige cat`"
            )));
        }
    };
    entries.map_err(|e| RpcError::internal(format!("area reports: json serialization: {e}")))
}

/// `neige_cat` on `raw` (classified as `path`), a path under `area/`; `selection` narrows a report
/// to those blocks (#1874) or sections (#1877).
pub(crate) async fn cat(
    ctx: &Arc<AppContext>,
    bound: &Track,
    raw: &str,
    path: AreaPath<'_>,
    args: &Value,
) -> Result<Value, RpcError> {
    super::unified_browse::reject(args, &["step"])?;
    if let Some(selection) = super::track_file::Selection::parse(
        args.as_object().expect("validated arguments"),
        super::track_file::TOOL_CAT,
    )? && !matches!(path, AreaPath::Report(_))
    {
        return Err(super::track_file::not_a_report(raw, &selection));
    }
    let AreaPath::Report(file) = path else {
        return Err(RpcError::invalid_params("path is a directory"));
    };
    let only_track = if super::unified_browse::is_user_area(ctx, bound.area_id.as_str()).await? {
        None
    } else {
        Some(bound.id.as_str())
    };
    let id = area_reports::resolve_track(pool(ctx)?, bound.area_id.as_str(), file, only_track)
        .await
        .map_err(track_fs_error_to_rpc)?;
    let track = super::unified_browse::track_in_area(ctx, bound.area_id.as_str(), &id).await?;
    super::unified_browse::report(ctx, &track, args).await
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
        .map(|reports| json!({ "reports": reports }))
        .map_err(|e| RpcError::internal(format!("{tool}: json serialization: {e}")))
}
