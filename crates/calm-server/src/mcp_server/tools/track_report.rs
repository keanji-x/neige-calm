//! Track-report MCP tools: `calm.report.read` / `.write` / `.edit`, shaped 1:1 after codex's `Read`/`Edit`/`Write` file tools.
//! Writes are Planner-only; `read` also admits the Assistant (the only source of `docRev` / per-block `rev`).

use crate::decision_sink::{CardDecisionSink, ReportOpCommit};
use crate::error::CalmError;
use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{
    AppContext, ToolCallIdentity, ToolDescriptor, ToolHandler, ToolHandlerFuture, ToolRegistry,
    read_only_annotations, require_role, require_role_any, role_gated_write_annotations,
};
use crate::mcp_server::result::ToolResult;
use crate::mcp_server::tools::lifecycle_args::{
    lifecycle_schema, message_schema, parse_write_args,
};
use crate::mcp_server::tools::track_report_hydrate::{hydrated_block_index, parse_resolve_arg};
use crate::model::{Card, CardRole, Track, TrackLifecycle};
use crate::track_report::{ReportDocOp, TrackReportPayload};
use crate::track_report_read::load_report_read_snapshot;
use serde_json::{Value, json};
use std::sync::Arc;

pub const TOOL_REPORT_READ: &str = "calm.report.read";
pub const TOOL_REPORT_WRITE: &str = "calm.report.write";
pub const TOOL_REPORT_EDIT: &str = "calm.report.edit";

pub fn register_into(registry: &mut ToolRegistry) {
    registry.register(read_descriptor(), wrap_with(report_read, read_result));
    registry.register(write_descriptor(), wrap(report_write));
    registry.register(edit_descriptor(), wrap(report_edit));
}

fn wrap<F, Fut>(f: F) -> ToolHandler
where
    F: Fn(Arc<AppContext>, ToolCallIdentity, Value) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<Value, RpcError>> + Send + 'static,
{
    wrap_with(f, ToolResult::structured)
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
                    "description": concat!(
                        "What to return: \"full\" (default; `text` + index), \"index\" ",
                        "(`docRev`, `summary`, `blocks`, `taskDiagnostics` — no `text`), or ",
                        "`{ \"blocks\": [\"b_x\", …] }` (index + `text` holding only those blocks ",
                        "in document order, each preceded by its `<!-- neige:b_x -->` marker line; ",
                        "an unknown id is an error)."
                    ),
                    "oneOf": [
                        { "type": "string", "enum": ["full", "index"] },
                        {
                            "type": "object",
                            "required": ["blocks"],
                            "properties": {
                                "blocks": { "type": "array", "items": { "type": "string" }, "minItems": 1 }
                            },
                            "additionalProperties": false
                        }
                    ]
                },
                "with_markers": {
                    "type": "boolean",
                    "description": concat!(
                        "Inject a `<!-- neige:b_xxxx -->` marker line before each block in ",
                        "`text` (default false; always on for `select.blocks`)."
                    )
                },
                "resolve": {
                    "type": "object",
                    "description": concat!(
                        "Per-block hydration override for `chart.series`, live `table`, and `view.live` ",
                        "blocks: `{ [block_id]: \"full\" | \"none\" }`. ",
                        "Default (no entry) is the summary."
                    ),
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
    // Assistant reads too: this is the ONLY source of `docRev` / per-block `rev`s, which every block-channel write needs. The write channel stays Planner-only.
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
    let snapshot = load_report_read_snapshot(
        ctx.repo.as_ref(),
        report_card.id.as_str(),
        ctx.task_budget_default,
    )
    .await
    .map_err(|e| RpcError::internal(format!("track_report: {e}")))?;
    // The index is always present; it is what a `docRev` / `if_rev` retry needs.
    let text = match &select {
        ReadSelect::Index => None,
        ReadSelect::Blocks(ids) => {
            // Markers are unconditional here: a partial text is only addressable through them. An unknown id is the caller's mistake.
            for id in ids {
                if !snapshot.blocks.iter().any(|block| &block.id == id) {
                    return Err(RpcError::invalid_params(format!(
                        "calm.report.read: select.blocks: unknown block id `{id}`"
                    )));
                }
            }
            let mut text = String::new();
            for block in snapshot
                .blocks
                .iter()
                .filter(|block| ids.contains(&block.id))
            {
                calm_types::report_blocks::append_block_text(
                    &mut text,
                    &format!(
                        "{}{}",
                        calm_types::report_blocks::marker_line(&block.id),
                        calm_types::report_blocks::flat_text(block)
                    ),
                );
            }
            Some(text)
        }
        ReadSelect::Full if with_markers => {
            let mut text = String::new();
            for block in &snapshot.blocks {
                calm_types::report_blocks::append_block_text(
                    &mut text,
                    &format!(
                        "{}{}",
                        calm_types::report_blocks::marker_line(&block.id),
                        calm_types::report_blocks::flat_text(block)
                    ),
                );
            }
            Some(text)
        }
        ReadSelect::Full => Some(snapshot.body.clone()),
    };
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
    if let Some(text) = text {
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
    /// The index plus a `text` of exactly these blocks, in document order.
    Blocks(Vec<String>),
}

fn parse_select_arg(args: &Value, tool: &str) -> Result<ReadSelect, RpcError> {
    const SHAPE: &str = "must be \"full\", \"index\" or { \"blocks\": [block id, …] } if provided";
    match args.get("select") {
        None | Some(Value::Null) => Ok(ReadSelect::Full),
        Some(Value::String(mode)) if mode == "full" => Ok(ReadSelect::Full),
        Some(Value::String(mode)) if mode == "index" => Ok(ReadSelect::Index),
        Some(Value::Object(map)) if map.len() == 1 && map.contains_key("blocks") => {
            let ids = map["blocks"]
                .as_array()
                .filter(|ids| !ids.is_empty())
                .and_then(|ids| {
                    ids.iter()
                        .map(|id| id.as_str().map(str::to_string))
                        .collect::<Option<Vec<_>>>()
                })
                .ok_or_else(|| {
                    RpcError::invalid_params(format!(
                        "{tool}: `select.blocks` must be a non-empty array of block ids"
                    ))
                })?;
            Ok(ReadSelect::Blocks(ids))
        }
        Some(_) => Err(RpcError::invalid_params(format!(
            "{tool}: `select` {SHAPE}"
        ))),
    }
}

fn write_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_REPORT_WRITE.into(),
        description: include_str!("../../../prompts/tools/calm.report.write.md")
            .trim_end()
            .to_string(),
        input_schema: json!({
            "type": "object",
            "required": ["body", "message", "if_doc_rev"],
            "properties": {
                "body": { "type": "string" },
                "if_doc_rev": { "type": "integer", "minimum": 0, "description": "The document-wide docRev returned by calm.report.read; not a block rev." },
                "summary": { "type": "string" },
                "message": message_schema(),
                "lifecycle": lifecycle_schema()
            }
        }),
        annotations: Some(role_gated_write_annotations()),
        visible_to_roles: &[CardRole::Planner],
    }
}

async fn report_write(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Planner)?;
    let write_args = parse_write_args(&args, "calm.report.write")?;
    let obj = args.as_object().ok_or_else(|| {
        RpcError::invalid_params("calm.report.write: arguments must be an object")
    })?;
    let body = obj
        .get("body")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::invalid_params("calm.report.write: missing `body` (string)"))?
        .to_string();
    let if_doc_rev = required_doc_rev(obj, "calm.report.write")?;
    let summary_override = match obj.get("summary") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => {
            return Err(RpcError::invalid_params(
                "calm.report.write: `summary` must be a string if provided",
            ));
        }
    };

    let (track, _, report_card, current) = resolve_report_for_caller(&ctx, &identity).await?;
    // Omitted summary = keep the existing one, resolved by the persist layer INSIDE the transaction; resolving from `current` here would let a concurrent summary write be silently reverted.
    commit_report_write_for_identity(
        &ctx,
        &identity,
        ReportSinkCall {
            track,
            report_card,
            current_payload: current,
            summary: summary_override,
            body,
            agent_message: write_args.message,
            lifecycle: write_args.lifecycle,
            if_doc_rev,
        },
    )
    .await
}

fn edit_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_REPORT_EDIT.into(),
        description: include_str!("../../../prompts/tools/calm.report.edit.md")
            .trim_end()
            .to_string(),
        input_schema: json!({
            "type": "object",
            "required": ["old_string", "new_string", "message", "if_doc_rev"],
            "properties": {
                "old_string": { "type": "string", "minLength": 1 },
                "new_string": { "type": "string" },
                "if_doc_rev": { "type": "integer", "minimum": 0, "description": "The document-wide docRev returned by calm.report.read; not a block rev." },
                "replace_all": { "type": "boolean" },
                "message": message_schema(),
                "lifecycle": lifecycle_schema()
            }
        }),
        annotations: Some(role_gated_write_annotations()),
        visible_to_roles: &[CardRole::Planner],
    }
}

async fn report_edit(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Planner)?;
    let write_args = parse_write_args(&args, "calm.report.edit")?;
    let obj = args
        .as_object()
        .ok_or_else(|| RpcError::invalid_params("calm.report.edit: arguments must be an object"))?;
    let old_string = obj
        .get("old_string")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            RpcError::invalid_params("calm.report.edit: missing `old_string` (non-empty string)")
        })?
        .to_string();
    let new_string = obj
        .get("new_string")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            RpcError::invalid_params(
                "calm.report.edit: missing `new_string` (string; empty is allowed)",
            )
        })?
        .to_string();
    let replace_all = match obj.get("replace_all") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => {
            return Err(RpcError::invalid_params(
                "calm.report.edit: `replace_all` must be a boolean if provided",
            ));
        }
    };
    let if_doc_rev = required_doc_rev(obj, "calm.report.edit")?;

    let (track, _, report_card, current) = resolve_report_for_caller(&ctx, &identity).await?;
    // Match exactly the projection served by read; never construct the replacement from the obsolete body cache. The write still checks CAS in-tx.
    let snapshot = load_report_read_snapshot(
        ctx.repo.as_ref(),
        report_card.id.as_str(),
        ctx.task_budget_default,
    )
    .await
    .map_err(|e| RpcError::internal(format!("track_report: {e}")))?;
    if snapshot.doc_rev != if_doc_rev {
        return Err(
            crate::mcp_server::tools::track_report_blocks::rev_conflict_error(format!(
                "document revision conflict: current doc_rev is {}, expected if_doc_rev {if_doc_rev}",
                snapshot.doc_rev
            )),
        );
    }

    // No `old_string == new_string` short-circuit: equal strings fall through to the persist boundary and emit the same event pair as `report.write`.
    let occurrences = count_matches(&snapshot.body, &old_string);
    if occurrences == 0 {
        return Err(RpcError::invalid_params(
            "calm.report.edit: old_string not found in body",
        ));
    }
    if occurrences > 1 && !replace_all {
        return Err(RpcError::invalid_params(format!(
            "calm.report.edit: old_string is not unique ({occurrences} matches); \
             pass replace_all=true to replace all matches"
        )));
    }
    let new_body = if replace_all || occurrences > 1 {
        snapshot.body.replace(&old_string, &new_string)
    } else {
        snapshot.body.replacen(&old_string, &new_string, 1)
    };

    // `edit` never touches the summary: `None` keeps whatever the doc holds at commit time.
    commit_report_write_for_identity(
        &ctx,
        &identity,
        ReportSinkCall {
            track,
            report_card,
            current_payload: current,
            summary: None,
            body: new_body,
            agent_message: write_args.message,
            lifecycle: write_args.lifecycle,
            if_doc_rev,
        },
    )
    .await
}

/// Non-overlapping, left-to-right — matches `str::replace`'s scan behavior.
fn count_matches(haystack: &str, needle: &str) -> usize {
    if needle.is_empty() {
        // `str::matches("")` returns infinitely many.
        return 0;
    }
    haystack.matches(needle).count()
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

/// MCP-side thin wrapper around [`CardDecisionSink::commit_report_write`]; only this path can name an `EditAuthor`, taken from the role.
struct ReportSinkCall {
    track: Track,
    report_card: Card,
    current_payload: TrackReportPayload,
    /// `None` keeps the current summary, resolved by the persist layer inside the transaction.
    summary: Option<String>,
    body: String,
    agent_message: String,
    lifecycle: Option<TrackLifecycle>,
    if_doc_rev: u64,
}

async fn commit_report_write_for_identity(
    ctx: &Arc<AppContext>,
    identity: &ToolCallIdentity,
    call: ReportSinkCall,
) -> Result<Value, RpcError> {
    match CardDecisionSink::from_app_context(ctx)
        .commit_report_op(
            identity,
            call.track,
            call.report_card,
            call.current_payload,
            ReportDocOp::Replace {
                summary: call.summary,
                body: call.body,
                if_doc_rev: call.if_doc_rev,
            },
            Some(call.agent_message),
            call.lifecycle,
        )
        .await
    {
        Ok(ReportOpCommit {
            card: updated,
            warnings,
            ..
        }) => {
            let doc_rev = updated_report_doc_rev(&updated, "track_report")?;
            Ok(json!({
                "updated_at": updated.updated_at,
                "docRev": doc_rev,
                "warnings": warnings,
            }))
        }
        Err(CalmError::Forbidden(msg)) => Err(RpcError::custom(
            -32403,
            format!("track_report: forbidden: {msg}"),
        )),
        // In-tx guard/validation of `ReportDocOp::Replace` surfaces as BadRequest and must map to -32602, not internal.
        Err(CalmError::BadRequest(msg)) => {
            Err(RpcError::invalid_params(format!("track_report: {msg}")))
        }
        Err(CalmError::Conflict(msg)) => Err(
            crate::mcp_server::tools::track_report_blocks::rev_conflict_error(format!(
                "track_report: {msg}"
            )),
        ),
        Err(e) => Err(RpcError::internal(format!("track_report: {e}"))),
    }
}

pub(crate) fn updated_report_doc_rev(card: &Card, tool: &str) -> Result<u64, RpcError> {
    card.payload
        .get("docRev")
        .and_then(Value::as_u64)
        .ok_or_else(|| RpcError::internal(format!("{tool}: updated report payload has no docRev")))
}

fn required_doc_rev(obj: &serde_json::Map<String, Value>, tool: &str) -> Result<u64, RpcError> {
    obj.get("if_doc_rev")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            RpcError::invalid_params(format!(
                "{tool}: missing `if_doc_rev` (document-wide docRev; use 0 for a new document)"
            ))
        })
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
            ReadSelect::Blocks(vec!["b_2".into(), "b_1".into()])
        );
        for bad in [
            json!({"select": "all"}),
            json!({"select": 1}),
            json!({"select": {}}),
            json!({"select": {"blocks": []}}),
            json!({"select": {"blocks": "b_1"}}),
            json!({"select": {"blocks": [1]}}),
            json!({"select": {"blocks": ["b_1"], "with_markers": true}}),
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

    #[test]
    fn count_matches_empty_needle_returns_zero() {
        assert_eq!(count_matches("abc", ""), 0);
    }

    #[test]
    fn count_matches_basic() {
        assert_eq!(count_matches("abcabc", "abc"), 2);
        assert_eq!(count_matches("aaa", "aa"), 1); // non-overlapping
        assert_eq!(count_matches("abc", "xyz"), 0);
        assert_eq!(count_matches("# Goal\n\n# Goal\n", "# Goal"), 2);
    }

    #[test]
    #[allow(clippy::no_effect_replace)] // pins that `str::replace(s, s)` is the identity map
    fn edit_equal_strings_replace_is_identity() {
        let body = "the body XYZ";
        assert_eq!(body.replace("XYZ", "XYZ"), body);
        assert_eq!(body.replacen("XYZ", "XYZ", 1), body);
    }
}
