//! Planner-only discovery reads for track-report links.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{Map, Value, json};

use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{
    AppContext, ToolCallIdentity, ToolDescriptor, ToolHandler, ToolHandlerFuture, ToolRegistry,
    read_only_annotations, require_role,
};
use crate::model::CardRole;
use crate::track_report_read::load_report_read_snapshot;

pub const TOOL_AREA_OUTLINE: &str = "calm.area.outline";
pub const TOOL_REPORT_BACKLINKS: &str = "calm.report.links.backlinks";

const MAX_TRACKS: usize = 50;
const MAX_BLOCKS_PER_TRACK: usize = 40;
const MAX_RESPONSE_BYTES: usize = 32 * 1024;

pub fn register_into(registry: &mut ToolRegistry) {
    registry.register(outline_descriptor(), wrap(area_outline));
    registry.register(backlinks_descriptor(), wrap(report_backlinks));
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

fn outline_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_AREA_OUTLINE.into(),
        description: include_str!("../../../prompts/tools/calm.area.outline.md")
            .trim_end()
            .to_string(),
        input_schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        annotations: Some(read_only_annotations()),
        visible_to_roles: &[CardRole::Planner],
    }
}

fn backlinks_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_REPORT_BACKLINKS.into(),
        description: include_str!("../../../prompts/tools/calm.report.links.backlinks.md")
            .trim_end()
            .to_string(),
        input_schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        annotations: Some(read_only_annotations()),
        visible_to_roles: &[CardRole::Planner],
    }
}

async fn area_outline(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    _args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Planner)?;
    let mut cards = ctx
        .repo
        .track_report_cards_by_area(identity.area_id.as_str())
        .await
        .map_err(|error| RpcError::internal(format!("area_outline: {error}")))?;
    cards.sort_by(|left, right| left.track_id.as_str().cmp(right.track_id.as_str()));

    let total_tracks = cards.len();
    let mut tracks = Vec::new();
    let mut block_truncations = BTreeMap::new();
    for card in cards.into_iter().take(MAX_TRACKS) {
        let track = ctx
            .repo
            .track_get(card.track_id.as_str())
            .await
            .map_err(|error| RpcError::internal(format!("area_outline: {error}")))?
            .ok_or_else(|| RpcError::internal("area_outline: track vanished mid-read"))?;
        let snapshot =
            load_report_read_snapshot(ctx.repo.as_ref(), card.id.as_str(), ctx.task_budget_default)
                .await
                .map_err(|error| RpcError::internal(format!("area_outline: {error}")))?;
        let omitted = snapshot.blocks.len().saturating_sub(MAX_BLOCKS_PER_TRACK);
        if omitted > 0 {
            block_truncations.insert(track.id.as_str().to_string(), omitted);
        }
        let blocks: Vec<Value> = snapshot
            .blocks
            .iter()
            .take(MAX_BLOCKS_PER_TRACK)
            .map(|block| {
                json!({
                    "id": block.id,
                    "kind": block.kind,
                    "heading": block_heading(block),
                })
            })
            .collect();
        tracks.push(json!({
            "id": track.id,
            "title": track.title,
            "lifecycle": track.lifecycle,
            "blocks": blocks,
        }));
    }

    let mut omitted_tracks = total_tracks.saturating_sub(MAX_TRACKS);
    let initial = outline_response(tracks.clone(), omitted_tracks, &block_truncations, false);
    let mut estimated_bytes =
        serde_json::to_vec(&initial).map_or(usize::MAX, |serialized| serialized.len());
    let bytes_truncated = estimated_bytes > MAX_RESPONSE_BYTES;
    // Reserve room for truncation metadata.
    let target_bytes = MAX_RESPONSE_BYTES.saturating_sub(4096);
    if bytes_truncated {
        for track in tracks.iter_mut().rev() {
            let Some(track_id) = track.get("id").and_then(Value::as_str).map(str::to_owned) else {
                continue;
            };
            let Some(blocks) = track.get_mut("blocks").and_then(Value::as_array_mut) else {
                continue;
            };
            while estimated_bytes > target_bytes {
                let Some(block) = blocks.pop() else {
                    break;
                };
                estimated_bytes = estimated_bytes.saturating_sub(
                    serde_json::to_vec(&block).map_or(0, |serialized| serialized.len() + 1),
                );
                *block_truncations.entry(track_id.clone()).or_default() += 1;
            }
        }
        while estimated_bytes > target_bytes {
            let Some(track) = tracks.pop() else {
                break;
            };
            estimated_bytes = estimated_bytes.saturating_sub(
                serde_json::to_vec(&track).map_or(0, |serialized| serialized.len() + 1),
            );
            omitted_tracks += 1;
            if let Some(track_id) = track.get("id").and_then(Value::as_str) {
                block_truncations.remove(track_id);
            }
        }
    }
    let response = outline_response(tracks, omitted_tracks, &block_truncations, bytes_truncated);
    if serde_json::to_vec(&response).map_or(usize::MAX, |serialized| serialized.len())
        > MAX_RESPONSE_BYTES
    {
        return Err(RpcError::internal(
            "area_outline: truncation metadata exceeds response byte cap",
        ));
    }
    Ok(response)
}

fn outline_response(
    tracks: Vec<Value>,
    omitted_tracks: usize,
    block_truncations: &BTreeMap<String, usize>,
    bytes_truncated: bool,
) -> Value {
    let mut response = Map::from_iter([("tracks".into(), Value::Array(tracks))]);
    let mut truncated = Map::new();
    if omitted_tracks > 0 {
        truncated.insert("tracks".into(), json!(omitted_tracks));
    }
    if !block_truncations.is_empty() {
        truncated.insert("blocks".into(), json!(block_truncations));
    }
    if bytes_truncated {
        truncated.insert("bytes".into(), Value::Bool(true));
    }
    if !truncated.is_empty() {
        response.insert("truncated".into(), Value::Object(truncated));
    }
    Value::Object(response)
}

fn block_heading(block: &calm_types::track_report::ReportBlock) -> String {
    if block.kind != calm_types::report_blocks::KIND_PROSE {
        if block.kind == calm_types::report_blocks::KIND_TASK {
            let field = if block.payload.get("kind").and_then(Value::as_str) == Some("terminal") {
                "command"
            } else {
                "goal"
            };
            if let Some(instruction) = block.payload.get(field).and_then(Value::as_str) {
                let rendered = calm_types::report_links::scan_links(instruction).plain;
                let rendered = rendered.trim();
                if !rendered.is_empty() {
                    return truncate_chars(&format!("{}: {field}={rendered}", block.kind), 60);
                }
            }
            return truncate_chars(
                &block
                    .payload
                    .get("key")
                    .and_then(Value::as_str)
                    .map_or_else(
                        || block.kind.clone(),
                        |key| format!("{}: key={key}", block.kind),
                    ),
                60,
            );
        }
        let identifying = ["symbol", "src", "caption", "title"]
            .into_iter()
            .find_map(|key| {
                block
                    .payload
                    .get(key)
                    .and_then(Value::as_str)
                    .map(|value| (key, value))
            });
        return truncate_chars(
            &identifying.map_or_else(
                || block.kind.clone(),
                |(key, value)| format!("{}: {key}={value}", block.kind),
            ),
            60,
        );
    }
    let markdown = block
        .payload
        .get("markdown")
        .and_then(Value::as_str)
        .unwrap_or_default();
    // The title comes from the shared `plain` projection (also used for backlink quotes) so an outline title
    // and a backlink quote never disagree. Known exception: a standalone image's alt is dropped, so an image-only block gets an empty title.
    let plain = calm_types::report_links::scan_links(markdown).plain;
    let heading = plain
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    truncate_chars(heading, 60)
}

fn truncate_chars(value: &str, limit: usize) -> String {
    value.chars().take(limit).collect()
}

async fn report_backlinks(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    _args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Planner)?;
    let track_id = identity.track_id.ok_or_else(|| {
        RpcError::invalid_params("calm.report.links.backlinks requires a track-scoped caller")
    })?;
    let page = crate::report_backlinks::backlinks_for_track(
        ctx.repo.as_ref(),
        &track_id,
        ctx.task_budget_default,
    )
    .await
    .map_err(|error| RpcError::internal(format!("report_backlinks: {error}")))?;
    Ok(crate::report_backlinks::mcp_payload(&page))
}

#[cfg(test)]
mod tests {
    use super::block_heading;
    use calm_types::track_report::ReportBlock;
    use serde_json::json;

    fn block(kind: &str, payload: serde_json::Value) -> ReportBlock {
        ReportBlock {
            id: "b_0000".into(),
            kind: kind.into(),
            rev: 1,
            payload,
        }
    }

    fn prose(markdown: &str) -> ReportBlock {
        block(
            calm_types::report_blocks::KIND_PROSE,
            json!({ "markdown": markdown }),
        )
    }

    /// A CommonMark HTML block of type 2 is not terminated by a blank line, so a real contract is one node spanning blank lines.
    const CONTRACT: &str = "<!-- 报告维护契约（渲染时被丢弃，读 body 源码的主体看得到）\n\
        \n\
        这份报告自带的结构就是规则：维护它，不要重写它。\n\
        \n\
        写作方式：散文正文控制在 1000 字以内。\n\
        -->\n\n";

    #[test]
    fn task_headings_render_discriminated_instruction_then_use_fallbacks() {
        assert_eq!(
            block_heading(&block(
                calm_types::report_blocks::KIND_TASK,
                json!({
                    "kind": "terminal",
                    "command": "cargo test",
                    "key": "test"
                }),
            )),
            "task: command=cargo test"
        );
        assert_eq!(
            block_heading(&block(
                calm_types::report_blocks::KIND_TASK,
                json!({
                    "goal": "[Ship task headings](https://example.com/raw-task-goal)",
                    "key": "ship-heading"
                }),
            )),
            "task: goal=Ship task headings"
        );
        assert_eq!(
            block_heading(&block(
                calm_types::report_blocks::KIND_TASK,
                json!({
                    "goal": "[](https://example.com/hidden-empty-goal)",
                    "key": "empty-goal-fallback"
                }),
            )),
            "task: key=empty-goal-fallback"
        );
        assert_eq!(
            block_heading(&block(
                calm_types::report_blocks::KIND_TASK,
                json!({ "key": "retired-heading", "tombstone": {} }),
            )),
            "task: key=retired-heading"
        );
        assert_eq!(
            block_heading(&block(calm_types::report_blocks::KIND_TASK, json!({}))),
            "task"
        );
        assert_eq!(
            block_heading(&block(
                "extension",
                json!({ "goal": "Not a task goal", "key": "not-a-task-key" }),
            )),
            "extension"
        );
    }

    #[test]
    fn a_comment_only_prose_block_has_an_empty_heading() {
        assert_eq!(block_heading(&prose(CONTRACT)), "");
    }

    #[test]
    fn an_atx_heading_is_still_the_title() {
        assert_eq!(block_heading(&prose("# 概要\n\n本轮结论。\n")), "概要");
    }

    #[test]
    fn a_contract_followed_by_prose_takes_the_first_line_of_the_prose() {
        assert_eq!(
            block_heading(&prose(&format!("{CONTRACT}# 概要\n\n本轮结论。\n"))),
            "概要"
        );
        assert_eq!(
            block_heading(&prose(&format!("{CONTRACT}本轮结论。\n"))),
            "本轮结论。"
        );
    }

    #[test]
    fn an_unterminated_comment_is_stripped_to_the_end() {
        // An unclosed block-level `<!--` runs to the end of the document in CommonMark.
        assert_eq!(
            block_heading(&prose("<!-- 报告维护契约\n\n# 概要\n\n本轮结论。\n")),
            ""
        );
    }

    #[test]
    fn a_comment_inside_inline_code_is_visible_text_and_keeps_its_characters() {
        assert_eq!(
            block_heading(&prose("`<!-- x -->` inline code first\n")),
            "<!-- x --> inline code first"
        );
    }

    #[test]
    fn an_unterminated_comment_inside_a_paragraph_does_not_swallow_the_line() {
        // Not at block start and never closed, so CommonMark leaves it as literal text.
        assert_eq!(
            block_heading(&prose("结论 <!-- 内部备注 继续写\n")),
            "结论 <!-- 内部备注 继续写"
        );
    }

    #[test]
    fn a_fenced_code_block_is_visible_text_even_when_it_contains_a_comment() {
        assert_eq!(
            block_heading(&prose("```html\n<!-- 示例 -->\n```\n")),
            "<!-- 示例 -->"
        );
    }

    #[test]
    fn a_leading_blank_line_does_not_turn_the_heading_into_its_hashes() {
        assert_eq!(block_heading(&prose("\n# 概要\n\n本轮结论。\n")), "概要");
    }

    #[test]
    fn a_hash_that_is_visible_code_keeps_its_hash() {
        assert_eq!(
            block_heading(&prose("`# x` not a heading\n")),
            "# x not a heading"
        );
    }

    #[test]
    fn an_image_only_block_gets_an_empty_heading() {
        assert_eq!(block_heading(&prose("![一张图](chart.png)\n")), "");

        // An image INSIDE a link keeps its alt, because that alt is the link's label.
        assert_eq!(
            block_heading(&prose("[![一张图](chart.png)](https://example.com)\n")),
            "一张图"
        );
    }
}
