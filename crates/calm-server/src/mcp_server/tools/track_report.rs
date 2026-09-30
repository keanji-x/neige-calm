//! Track-report MCP read tool `calm.report.read`, plus the report resolution helpers the write tools in `track_report_blocks` share.
//! `read` admits the Planner and the Assistant; its text is what anchors their report writes.

use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{
    AppContext, ToolCallIdentity, ToolDescriptor, ToolHandler, ToolHandlerFuture, ToolRegistry,
    read_only_annotations, require_role_any,
};
use crate::mcp_server::result::ToolResult;
use crate::mcp_server::tools::report_links::unknown_block;
use crate::mcp_server::tools::track_file::Selection;
use crate::mcp_server::tools::track_report_hydrate::{hydrated_block_index, parse_resolve_arg};
use crate::model::{Card, CardRole, Track};
use crate::track_report::TrackReportPayload;
use crate::track_report_read::{
    load_report_read_snapshot, marked_blocks_text, selected_blocks_text,
};
use serde_json::{Value, json};
use std::sync::Arc;

pub const TOOL_REPORT_READ: &str = "calm.report.read";

pub fn register_into(registry: &mut ToolRegistry) {
    registry.register(read_descriptor(), wrap_with(report_read, read_result));
}

fn wrap_with<F, Fut>(f: F, envelope: fn(Value) -> ToolResult) -> ToolHandler
where
    F: Fn(Arc<AppContext>, ToolCallIdentity, Value) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<Value, RpcError>> + Send + 'static,
{
    Arc::new(move |ctx, identity, args| -> ToolHandlerFuture {
        let result = f(ctx, identity, args);
        Box::pin(async move { result.await.map(envelope) })
    })
}

/// The read's envelope: one summary line in `content[0].text`, the document itself only in `structuredContent`.
fn read_result(value: Value) -> ToolResult {
    let summary = read_summary_line(&value);
    ToolResult::structured_with_summary(value, summary)
}

/// In BYTES: the receipt's size bound is a byte bound, and the summary is Chinese (120 chars would be ~360 bytes).
const SUMMARY_LINE_BYTES: usize = 120;

/// `None` when the whole text fits.
fn clip_to_bytes(text: &str, budget: usize) -> Option<&str> {
    if text.len() <= budget {
        return None;
    }
    let cut = text
        .char_indices()
        .map(|(index, _)| index)
        .take_while(|index| *index <= budget)
        .last()
        .unwrap_or(0);
    Some(&text[..cut])
}

fn read_summary_line(value: &Value) -> String {
    let doc_rev = &value["docRev"];
    let blocks = value["blocks"].as_array().map_or(0, Vec::len);
    let payload = match value.get("text").and_then(Value::as_str) {
        Some(text) => format!("{} bytes", text.len()),
        None => "index only".to_string(),
    };
    let summary = value["summary"].as_str().unwrap_or_default();
    let summary = summary.split(['\n', '\r']).next().unwrap_or_default();
    let summary = match clip_to_bytes(summary, SUMMARY_LINE_BYTES) {
        Some(prefix) => format!("{prefix}…"),
        None => summary.to_string(),
    };
    format!(
        "docRev {doc_rev} · {blocks} blocks · {payload} · {summary}; full state in structuredContent"
    )
}

fn read_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_REPORT_READ.into(),
        description: include_str!("../../../prompts/tools/calm.report.read.md")
            .trim_end()
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "select": {
                    "oneOf": [
                        { "type": "string", "enum": ["full", "index"] },
                        {
                            "type": "object",
                            "required": ["blocks"],
                            "properties": {
                                "blocks": { "type": "array", "items": { "type": "string" }, "minItems": 1 }
                            },
                            "additionalProperties": false
                        },
                        {
                            "type": "object",
                            "required": ["sections"],
                            "properties": {
                                "sections": { "type": "array", "items": { "type": "string" }, "minItems": 1 }
                            },
                            "additionalProperties": false
                        }
                    ]
                },
                "with_markers": { "type": "boolean" },
                "resolve": {
                    "type": "object",
                    "additionalProperties": { "type": "string", "enum": ["full", "none"] }
                }
            }
        }),
        annotations: Some(read_only_annotations()),
        visible_to_roles: &[CardRole::Planner],
    }
}

pub(crate) async fn report_read(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    // Assistant reads too: this is the read every agent report write is anchored by (#1883).
    require_role_any(&identity, &[CardRole::Planner, CardRole::Assistant])?;
    let select = parse_select_arg(&args, "calm.report.read")?;
    let with_markers = match args.get("with_markers") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => {
            return Err(RpcError::invalid_params(
                "calm.report.read: `with_markers` must be a boolean if provided",
            ));
        }
    };
    // The response body comes from ONE fresh row snapshot so `summary`/`text`/`blocks` can never tear against each other.
    let resolve_modes = parse_resolve_arg(&args, "calm.report.read")?;
    let (track, _, report_card, _) = resolve_report_for_caller(&ctx, &identity).await?;
    let snapshot = load_report_read_snapshot(ctx.repo.as_ref(), report_card.id.as_str())
        .await
        .map_err(|e| RpcError::internal(format!("track_report: {e}")))?;
    // The index is always present.
    let all = || {
        snapshot
            .blocks
            .iter()
            .map(|block| block.id.clone())
            .collect()
    };
    let text = match &select {
        ReadSelect::Index => None,
        ReadSelect::Part(selection) => {
            // Markers are unconditional here: a partial text is only addressable through them. An unknown id is the caller's mistake.
            let ids = selection.block_ids(&snapshot.blocks)?;
            let text = selected_blocks_text(&snapshot.blocks, &ids)
                .map_err(|id| unknown_block(&snapshot.blocks, id))?;
            Some((text, ids))
        }
        ReadSelect::Full if with_markers => Some((marked_blocks_text(&snapshot.blocks), all())),
        ReadSelect::Full => Some((snapshot.body.clone(), all())),
    };
    // Only a read that returned text is this session's read of the report (#1877): exactly the blocks it rendered.
    if let Some((_, rendered)) = &text {
        ctx.read_ledger.record(
            &identity.session_id,
            &identity.card_id,
            report_card.id.as_str(),
            snapshot.doc_rev,
            &snapshot.blocks,
            rendered,
        );
    }
    // `resolved` is rows and overlays only; this read never calls a plugin and never writes.
    let index: Vec<Value> =
        hydrated_block_index(&ctx, track.id.as_str(), &snapshot.blocks, &resolve_modes).await;
    let mut response = json!({
        "summary": snapshot.summary,
        "schemaVersion": snapshot.schema_version,
        "docRev": snapshot.doc_rev,
        "updated_at": snapshot.updated_at,
        "blocks": index,
    });
    if let Some((text, _)) = text {
        response["text"] = Value::String(text);
    }
    // `taskDiagnostics` is the dispatched-task runtime projection `calm.plan.list` withholds from the assistant; only the Planner gets it.
    if identity.role == CardRole::Planner {
        response["taskDiagnostics"] = json!(snapshot.task_diagnostics);
    }
    Ok(response)
}

#[derive(Debug, PartialEq, Eq)]
enum ReadSelect {
    /// Today's shape: the whole `text` plus the index.
    Full,
    /// The index and the document metadata; no `text`.
    Index,
    /// The index plus a `text` of exactly the selected blocks or sections, in document order.
    Part(Selection),
}

fn parse_select_arg(args: &Value, tool: &str) -> Result<ReadSelect, RpcError> {
    const SHAPE: &str = "must be \"full\", \"index\", { \"blocks\": [block id, …] } or \
                         { \"sections\": [H1 text, …] } if provided";
    match args.get("select") {
        None | Some(Value::Null) => Ok(ReadSelect::Full),
        Some(Value::String(mode)) if mode == "full" => Ok(ReadSelect::Full),
        Some(Value::String(mode)) if mode == "index" => Ok(ReadSelect::Index),
        Some(Value::Object(map)) if map.len() == 1 => {
            match Selection::parse(map, &format!("{tool}: `select`"))? {
                Some(selection) => Ok(ReadSelect::Part(selection)),
                None => Err(RpcError::invalid_params(format!(
                    "{tool}: `select` {SHAPE}"
                ))),
            }
        }
        Some(_) => Err(RpcError::invalid_params(format!(
            "{tool}: `select` {SHAPE}"
        ))),
    }
}

/// A missing planner card, track, or report card is a data-shape bug (every track has exactly one report card), surfaced as InternalError.
pub(crate) async fn resolve_report_for_caller(
    ctx: &Arc<AppContext>,
    identity: &ToolCallIdentity,
) -> Result<(Track, Card, Card, TrackReportPayload), RpcError> {
    let card_id_str = identity.card_id.as_str().to_string();
    let planner_card = ctx
        .repo
        .card_get(&card_id_str)
        .await
        .map_err(|e| RpcError::internal(format!("track_report: planner card lookup: {e}")))?
        .ok_or_else(|| {
            RpcError::internal(format!(
                "track_report: bound planner card {card_id_str} not found (deleted mid-connection?)"
            ))
        })?;
    let track = ctx
        .repo
        .track_get(planner_card.track_id.as_str())
        .await
        .map_err(|e| RpcError::internal(format!("track_report: track lookup: {e}")))?
        .ok_or_else(|| {
            RpcError::internal(format!(
                "track_report: track {} for planner card {} not found",
                planner_card.track_id.as_str(),
                card_id_str
            ))
        })?;
    let (report_card, payload) = load_report_for_track(ctx, &track).await?;
    Ok((track, planner_card, report_card, payload))
}

/// Role-agnostic by design: callers must enforce their own MCP entry gate and track binding before reaching it.
pub(crate) async fn load_report_for_track(
    ctx: &Arc<AppContext>,
    track: &Track,
) -> Result<(Card, TrackReportPayload), RpcError> {
    // Exactly one report card per track (partial unique index `idx_cards_one_report_per_track`); scanning every card is fine, tracks are small.
    let cards = ctx
        .repo
        .cards_by_track(track.id.as_str())
        .await
        .map_err(|e| RpcError::internal(format!("track_report: cards_by_track: {e}")))?;
    let report_card = cards
        .into_iter()
        .find(|c| c.kind == "track-report")
        .ok_or_else(|| {
            RpcError::internal(format!(
                "track_report: track {} has no track-report card (invariant violation)",
                track.id.as_str()
            ))
        })?;
    let payload: TrackReportPayload =
        serde_json::from_value(report_card.payload.clone()).map_err(|e| {
            RpcError::internal(format!(
                "track_report: malformed payload on card {}: {e}",
                report_card.id.as_str()
            ))
        })?;
    Ok((report_card, payload))
}

pub(crate) fn updated_report_doc_rev(card: &Card, tool: &str) -> Result<u64, RpcError> {
    card.payload
        .get("docRev")
        .and_then(Value::as_u64)
        .ok_or_else(|| RpcError::internal(format!("{tool}: updated report payload has no docRev")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_select_arg_accepts_the_three_forms_and_refuses_the_rest() {
        let parse = |args: Value| parse_select_arg(&args, "t");
        assert_eq!(parse(json!({})).unwrap(), ReadSelect::Full);
        assert_eq!(parse(json!({"select": null})).unwrap(), ReadSelect::Full);
        assert_eq!(parse(json!({"select": "full"})).unwrap(), ReadSelect::Full);
        assert_eq!(
            parse(json!({"select": "index"})).unwrap(),
            ReadSelect::Index
        );
        assert_eq!(
            parse(json!({"select": {"blocks": ["b_2", "b_1"]}})).unwrap(),
            ReadSelect::Part(Selection::Blocks(vec!["b_2".into(), "b_1".into()]))
        );
        assert_eq!(
            parse(json!({"select": {"sections": ["B", "A"]}})).unwrap(),
            ReadSelect::Part(Selection::Sections(vec!["B".into(), "A".into()]))
        );
        for bad in [
            json!({"select": "all"}),
            json!({"select": 1}),
            json!({"select": {}}),
            json!({"select": {"blocks": []}}),
            json!({"select": {"blocks": "b_1"}}),
            json!({"select": {"blocks": [1]}}),
            json!({"select": {"blocks": ["b_1"], "with_markers": true}}),
            json!({"select": {"blocks": ["b_1"], "sections": ["A"]}}),
            json!({"select": {"sections": []}}),
        ] {
            let err = parse(bad.clone()).expect_err("must be refused");
            assert_eq!(err.code, RpcError::INVALID_PARAMS, "{bad}");
            assert!(err.message.contains("select"), "{bad}: {}", err.message);
        }
    }

    #[test]
    fn read_summary_line_is_one_short_line() {
        let line = read_summary_line(&json!({
            "docRev": 12,
            "blocks": [{"id": "b_1"}, {"id": "b_2"}],
            "text": "héllo",
            "summary": "first line\nsecond line",
        }));
        assert_eq!(
            line,
            "docRev 12 · 2 blocks · 6 bytes · first line; full state in structuredContent"
        );
        let index = read_summary_line(&json!({"docRev": 0, "blocks": [], "summary": ""}));
        assert_eq!(
            index,
            "docRev 0 · 0 blocks · index only · ; full state in structuredContent"
        );
        let long = "字".repeat(200);
        let clipped = read_summary_line(&json!({"docRev": 1, "blocks": [], "summary": long}));
        assert!(
            clipped.contains(&format!("· {}…;", "字".repeat(SUMMARY_LINE_BYTES / 3))),
            "{clipped}"
        );
        assert!(!clipped.contains('\n'));
        assert!(clipped.len() < SUMMARY_LINE_BYTES + 80, "{}", clipped.len());
        assert_eq!(clip_to_bytes("字字", 4), Some("字"));
        assert_eq!(clip_to_bytes("字字", 6), None);
        assert_eq!(clip_to_bytes("abc", 2), Some("ab"));
        assert_eq!(clip_to_bytes("", 0), None);
    }
}
