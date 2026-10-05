//! Granted workspace report reads. Foreign reports never enter the caller’s write ledger.
use crate::managed_track::require_workspace_reports;
use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{
    AppContext, ToolCallIdentity, ToolDescriptor, ToolHandler, ToolRegistry, read_only_annotations,
};
use crate::mcp_server::result::ToolResult;
use crate::mcp_server::tools::write_args::refuse_unknown_keys;
use crate::model::CardRole;
use crate::workspace_reports::{self, ReportChangesQuery, ReportEditsQuery};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

pub const TOOL_WORKSPACE_LS: &str = "neige_workspace_ls";
pub const TOOL_WORKSPACE_CAT: &str = "neige_workspace_cat";
pub const TOOL_WORKSPACE_DIFF: &str = "neige_workspace_diff";
pub const TOOL_WORKSPACE_LOG: &str = "neige_workspace_log";

pub fn register_into(registry: &mut ToolRegistry) {
    for (name, description, schema) in [
        (
            TOOL_WORKSPACE_LS,
            include_str!("../../../prompts/tools/neige_workspace_ls.md"),
            json!({"type":"object","properties":{"cursor":{"type":"string"}},"additionalProperties":false}),
        ),
        (
            TOOL_WORKSPACE_CAT,
            include_str!("../../../prompts/tools/neige_workspace_cat.md"),
            json!({"type":"object","required":["track_id"],"properties":{"track_id":{"type":"string"}},"additionalProperties":false}),
        ),
        (
            TOOL_WORKSPACE_DIFF,
            include_str!("../../../prompts/tools/neige_workspace_diff.md"),
            json!({"type":"object","required":["date"],"properties":{"date":{"type":"string"},"cursor":{"type":"string"},"through_event_id":{"type":"integer","minimum":0}},"additionalProperties":false}),
        ),
        (
            TOOL_WORKSPACE_LOG,
            include_str!("../../../prompts/tools/neige_workspace_log.md"),
            json!({"type":"object","required":["date","track_id","through_event_id"],"properties":{"date":{"type":"string"},"track_id":{"type":"string"},"cursor":{"type":"string"},"through_event_id":{"type":"integer","minimum":0}},"additionalProperties":false}),
        ),
    ] {
        // Closed input (§4): the schema's own keys are the valid ones, so a retired key such as
        // `after` is refused naming them.
        let keys: Arc<[String]> = schema["properties"]
            .as_object()
            .expect("workspace schemas declare properties")
            .keys()
            .cloned()
            .collect();
        let handler: ToolHandler = Arc::new(move |ctx, identity, args| {
            let keys = keys.clone();
            Box::pin(async move {
                let valid: Vec<&str> = keys.iter().map(String::as_str).collect();
                refuse_unknown_keys(&args, name, &valid)?;
                dispatch(ctx, identity, args, name)
                    .await
                    .map(ToolResult::structured)
            })
        });
        registry.register(
            ToolDescriptor {
                name: name.into(),
                description: description.trim_end().into(),
                input_schema: schema,
                annotations: Some(read_only_annotations()),
                visible_to_roles: &[CardRole::Planner],
            },
            handler,
        );
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListArgs {
    cursor: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArgs {
    track_id: String,
}

async fn dispatch(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
    name: &str,
) -> Result<Value, RpcError> {
    let zone = require_workspace_reports(&ctx, &identity).await?;
    let pool = ctx
        .sqlite_pool
        .as_ref()
        .ok_or_else(|| RpcError::internal("workspace reports require sqlite"))?;
    let invalid = |e: serde_json::Error| RpcError::invalid_params(e.to_string());
    match name {
        TOOL_WORKSPACE_LS => {
            let args: ListArgs = serde_json::from_value(args).map_err(invalid)?;
            let mut rows:Vec<(String,String,String,String,Option<i64>)>=sqlx::query_as(concat!(
"SELECT t.id,t.title,a.id,a.name,t.closed_at FROM tracks t JOIN areas a ON a.id=t.area_id ",
"WHERE a.kind='user' AND (?1 IS NULL OR t.id>?1) ORDER BY t.id LIMIT ?2",
))
                .bind(args.cursor).bind((workspace_reports::PAGE_SIZE+1) as i64).fetch_all(pool).await.map_err(|e|RpcError::internal(e.to_string()))?;
            let next_cursor = (rows.len() > workspace_reports::PAGE_SIZE)
                .then(|| rows[workspace_reports::PAGE_SIZE - 1].0.clone());
            rows.truncate(workspace_reports::PAGE_SIZE);
            Ok(
                json!({"reports":rows.into_iter().map(|(track_id,title,area_id,area_name,closed_at)|json!({"track_id":track_id,"title":title,"area_id":area_id,"area_name":area_name,"closed_at":closed_at})).collect::<Vec<_>>(),"next_cursor":next_cursor,"timezone":zone.name()}),
            )
        }
        TOOL_WORKSPACE_CAT => {
            let args: ReadArgs = serde_json::from_value(args).map_err(invalid)?;
            let track = ctx
                .repo
                .track_get(&args.track_id)
                .await
                .map_err(|e| RpcError::internal(e.to_string()))?
                .ok_or_else(|| RpcError::invalid_params("report Track not found"))?;
            let visible = ctx
                .repo
                .area_get(track.area_id.as_str())
                .await
                .map_err(|e| RpcError::internal(e.to_string()))?
                .is_some_and(|area| area.kind == crate::model::AreaKind::User);
            if !visible {
                return Err(RpcError::custom(
                    -32403,
                    "report is outside user-visible Areas",
                ));
            }
            let (card, _) = super::track_report::load_report_for_track(&ctx, &track).await?;
            let snapshot = crate::track_report_read::load_report_read_snapshot(
                ctx.repo.as_ref(),
                card.id.as_str(),
            )
            .await
            .map_err(|e| RpcError::internal(e.to_string()))?;
            Ok(
                json!({"track_id":track.id,"title":track.title,"area_id":track.area_id,"summary":snapshot.summary,"body":snapshot.body,"doc_rev":snapshot.doc_rev,"blocks":snapshot.blocks}),
            )
        }
        TOOL_WORKSPACE_DIFF => {
            let query: ReportChangesQuery = serde_json::from_value(args).map_err(invalid)?;
            serde_json::to_value(
                workspace_reports::changes(pool, &query, zone)
                    .await
                    .map_err(error)?,
            )
            .map_err(|e| RpcError::internal(e.to_string()))
        }
        TOOL_WORKSPACE_LOG => {
            let query: ReportEditsQuery = serde_json::from_value(args).map_err(invalid)?;
            serde_json::to_value(
                workspace_reports::edits(pool, &query, zone)
                    .await
                    .map_err(error)?,
            )
            .map_err(|e| RpcError::internal(e.to_string()))
        }
        _ => Err(RpcError::internal(
            "unregistered workspace report operation",
        )),
    }
}

fn error(error: crate::error::CalmError) -> RpcError {
    match error {
        crate::error::CalmError::BadRequest(message) => RpcError::invalid_params(message),
        other => RpcError::internal(other.to_string()),
    }
}

#[cfg(test)]
mod tests;
