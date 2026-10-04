//! `neige.report.tag` (#1838 S1): list (Planner or Worker) or change (Planner) the tags of the
//! caller's own track report. The track comes only from the bound card identity; no argument names
//! a track, and the one accepted path is `report.md`, so there is no cross-track tag write.

use std::sync::{Arc, Mutex};

use serde_json::{Map, Value, json};

use super::track_file::resolve_track_for_identity;
use crate::db::write_with_actor_events_typed;
use crate::error::CalmError;
use crate::event::{Event, EventScope};
use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{
    AppContext, ToolCallIdentity, ToolDescriptor, ToolHandler, ToolHandlerFuture, ToolRegistry,
    require_role_any, role_gated_write_annotations,
};
use crate::model::CardRole;
use crate::report_tags::{MAX_TAGS_PER_REPORT, normalize_tag, store};
use crate::track_fs_view::normalize_path;

pub const TOOL_REPORT_TAG: &str = "neige.report.tag";

/// The only taggable path: the caller's own report.
const REPORT_PATH: &str = "report.md";
const KEYS: &[&str] = &["path", "add", "remove"];
/// Rolls back a call that left the tag list unchanged; no row writer's conflict message equals it.
const NO_CHANGE: &str = "neige.report.tag: no tag changed";

pub fn register_into(registry: &mut ToolRegistry) {
    registry.register(descriptor(), wrap(report_tag));
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

/// Served through `neige tag`, like the other `neige` views: hidden from every role's `tools/list`.
fn descriptor() -> ToolDescriptor {
    let tags = json!({ "type": "array", "items": { "type": "string" } });
    ToolDescriptor {
        name: TOOL_REPORT_TAG.into(),
        description: include_str!("../../../prompts/tools/neige.report.tag.md")
            .trim_end()
            .to_string(),
        input_schema: json!({
            "type": "object",
            "required": ["path"],
            "properties": {
                "path": { "type": "string" },
                "add": tags,
                "remove": tags
            },
            "additionalProperties": false
        }),
        annotations: Some(role_gated_write_annotations()),
        visible_to_roles: &[],
    }
}

async fn report_tag(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role_any(&identity, &[CardRole::Planner, CardRole::Worker])?;
    let tool = TOOL_REPORT_TAG;
    let obj = args
        .as_object()
        .ok_or_else(|| RpcError::invalid_params(format!("{tool}: arguments must be an object")))?;
    if let Some(key) = obj.keys().find(|key| !KEYS.contains(&key.as_str())) {
        return Err(RpcError::invalid_params(format!(
            "{tool}: unknown argument `{key}`; the tagged report is always the caller's own"
        )));
    }
    let path = obj
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::invalid_params(format!("{tool}: missing `path` (string)")))?;
    if normalize_path(path) != REPORT_PATH {
        return Err(RpcError::invalid_params(format!(
            "{tool}: only `{REPORT_PATH}` (this track's own report) takes tags; got `{path}`"
        )));
    }
    let add = tag_list(obj, "add")?;
    let remove = tag_list(obj, "remove")?;
    // Changing tags moves the report's update time, and the report is Planner-authored: the write
    // gate admits no Worker event outside the Worker's own card scope. Workers only list.
    if !(add.is_empty() && remove.is_empty()) && identity.role != CardRole::Planner {
        return Err(RpcError::invalid_params(format!(
            "{tool}: only the Planner changes report tags; a {:?} may only list them",
            identity.role
        )));
    }

    let (_, track) = resolve_track_for_identity(&ctx, &identity).await?;
    let track_id = track.id.as_str().to_string();
    if add.is_empty() && remove.is_empty() {
        let pool = ctx
            .sqlite_pool
            .as_ref()
            .ok_or_else(|| RpcError::internal(format!("{tool}: requires a sqlite-backed repo")))?;
        let tags = store::list(pool, &track_id)
            .await
            .map_err(|e| map_err(tool, e))?;
        return Ok(json!({ "tags": tags }));
    }

    let actor = identity.to_actor_id();
    let area_id = track.area_id.clone();
    let track_ref = track.id.clone();
    // The write path refuses an empty event batch, so a call that leaves the tags unchanged
    // carries them out through `unchanged` and rolls its transaction back.
    let unchanged = Arc::new(Mutex::new(None));
    let unchanged_in = unchanged.clone();
    let result = write_with_actor_events_typed(
        ctx.repo.as_ref(),
        None,
        &ctx.events,
        &ctx.write,
        move |tx| {
            Box::pin(async move {
                let applied = store::apply_tx(tx, &track_id, &add, &remove).await?;
                let Some(card) = applied.touched_report else {
                    *unchanged_in.lock().expect("unchanged lock") = Some(applied.tags);
                    return Err(CalmError::Conflict(NO_CHANGE.into()));
                };
                let scope = EventScope::Card {
                    card: card.id.clone(),
                    track: track_ref,
                    area: area_id,
                };
                Ok((applied.tags, vec![(actor, scope, Event::CardUpdated(card))]))
            })
        },
    )
    .await;
    let tags = match result {
        Ok((tags, _ids)) => tags,
        Err(CalmError::Conflict(message)) if message == NO_CHANGE => unchanged
            .lock()
            .expect("unchanged lock")
            .take()
            .ok_or_else(|| RpcError::internal(format!("{tool}: no-op result lost")))?,
        Err(e) => return Err(map_err(tool, e)),
    };
    Ok(json!({ "tags": tags }))
}

/// An absent key is an empty list; every entry is normalized here, before any row is touched.
fn tag_list(obj: &Map<String, Value>, key: &str) -> Result<Vec<String>, RpcError> {
    let tool = TOOL_REPORT_TAG;
    let Some(raw) = obj.get(key) else {
        return Ok(Vec::new());
    };
    let items = raw.as_array().ok_or_else(|| {
        RpcError::invalid_params(format!("{tool}: `{key}` must be an array of strings"))
    })?;
    if items.len() > MAX_TAGS_PER_REPORT {
        return Err(RpcError::invalid_params(format!(
            "{tool}: `{key}` names {} tags; a report carries at most {MAX_TAGS_PER_REPORT}",
            items.len()
        )));
    }
    items
        .iter()
        .map(|item| {
            let raw = item.as_str().ok_or_else(|| {
                RpcError::invalid_params(format!("{tool}: `{key}` must be an array of strings"))
            })?;
            normalize_tag(raw).map_err(|m| RpcError::invalid_params(format!("{tool}: {m}")))
        })
        .collect()
}

fn map_err(tool: &str, e: CalmError) -> RpcError {
    match e {
        CalmError::BadRequest(m) => RpcError::invalid_params(format!("{tool}: {m}")),
        other => RpcError::internal(format!("{tool}: {other}")),
    }
}
