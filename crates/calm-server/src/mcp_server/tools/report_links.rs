//! Planner-only discovery reads for track-report links.

use std::sync::Arc;

use serde_json::{Value, json};

use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{
    AppContext, ToolCallIdentity, ToolDescriptor, ToolHandler, ToolHandlerFuture, ToolRegistry,
    read_only_annotations,
};
use crate::mcp_server::tools::paging::{self, Page};
use crate::model::CardRole;
use crate::track_report_read::load_report_read_snapshot;

pub const TOOL_AREA_LS: &str = "neige_area_ls";
pub const TOOL_LINK_LS: &str = "neige_link_ls";

const TRACKS_PER_PAGE: usize = 50;
const MAX_BLOCKS_PER_TRACK: usize = 40;
const LINKS_PER_PAGE: usize = 100;

/// The one input of both listings: the previous page's `next_cursor`.
fn cursor_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "cursor": { "type": "string" } },
        "additionalProperties": false
    })
}

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
        name: TOOL_AREA_LS.into(),
        description: include_str!("../../../prompts/tools/neige_area_ls.md")
            .trim_end()
            .to_string(),
        input_schema: cursor_schema(),
        annotations: Some(read_only_annotations()),
        roles: &[CardRole::Planner],
        listed_for: &[CardRole::Planner],
    }
}

fn backlinks_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_LINK_LS.into(),
        description: include_str!("../../../prompts/tools/neige_link_ls.md")
            .trim_end()
            .to_string(),
        input_schema: cursor_schema(),
        annotations: Some(read_only_annotations()),
        roles: &[CardRole::Planner],
        listed_for: &[CardRole::Planner],
    }
}

async fn area_outline(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    let cursor = paging::cursor_arg(&args, TOOL_AREA_LS)?;
    let mut cards = ctx
        .repo
        .track_report_cards_by_area(identity.area_id.as_str())
        .await
        .map_err(|error| RpcError::internal(format!("neige_area_ls: {error}")))?;
    cards.sort_by(|left, right| left.track_id.as_str().cmp(right.track_id.as_str()));
    if let Some(cursor) = cursor
        && !cards.iter().any(|card| card.track_id.as_str() == cursor)
    {
        return Err(paging::foreign_cursor(TOOL_AREA_LS, cursor));
    }

    // Keyset on the track id: the page resumes after the last row's track.
    let mut page = Page::new(TRACKS_PER_PAGE);
    for card in cards
        .into_iter()
        .filter(|card| cursor.is_none_or(|after| card.track_id.as_str() > after))
    {
        let track = ctx
            .repo
            .track_get(card.track_id.as_str())
            .await
            .map_err(|error| RpcError::internal(format!("neige_area_ls: {error}")))?
            .ok_or_else(|| RpcError::internal("neige_area_ls: track vanished mid-read"))?;
        let snapshot = load_report_read_snapshot(ctx.repo.as_ref(), card.id.as_str())
            .await
            .map_err(|error| RpcError::internal(format!("neige_area_ls: {error}")))?;
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
        let row = json!({
            "track_id": track.id,
            "title": track.title,
            "closed_at": crate::time_format::at_opt(track.closed_at),
            "blocks": blocks,
            "blocks_truncated": snapshot.blocks.len().saturating_sub(MAX_BLOCKS_PER_TRACK),
        });
        if !page.push(track.id.as_str().to_string(), row) {
            break;
        }
    }
    let (tracks, next_cursor) = page.finish(false);
    Ok(json!({ "tracks": tracks, "next_cursor": next_cursor }))
}

/// The refusal of a block selection naming `id`, which is no block of `blocks`: the report's blocks
/// as `<id>  <heading>` lines, so the caller can pick again.
pub(crate) fn unknown_block(
    blocks: &[calm_types::track_report::ReportBlock],
    id: &str,
) -> RpcError {
    let listed: Vec<String> = blocks
        .iter()
        .map(|block| {
            format!("  {}  {}", block.id, block_heading(block))
                .trim_end()
                .to_string()
        })
        .collect();
    RpcError::invalid_params(format!(
        "unknown block id `{id}`; this report's blocks are:\n{}",
        listed.join("\n")
    ))
}

/// The refusal of a section selection that names no single H1 section: the report's sections (or the
/// candidates of a duplicated heading), the section twin of [`unknown_block`].
pub(crate) fn unknown_section(
    blocks: &[calm_types::track_report::ReportBlock],
    error: &crate::report_sections::SectionError,
) -> RpcError {
    RpcError::invalid_params(crate::report_sections::section_error_message(blocks, error))
}

/// A block's one-line heading as the outline lists it; also the chat `@` mention label (#1881).
pub(crate) fn block_heading(block: &calm_types::track_report::ReportBlock) -> String {
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
    args: Value,
) -> Result<Value, RpcError> {
    let cursor = paging::cursor_arg(&args, TOOL_LINK_LS)?;
    let after = cursor
        .map(|cursor| {
            crate::report_backlinks::parse_cursor(cursor)
                .ok_or_else(|| paging::foreign_cursor(TOOL_LINK_LS, cursor))
        })
        .transpose()?;
    let track_id = identity
        .track_id
        .ok_or_else(|| RpcError::forbidden("neige_link_ls requires a track-scoped caller"))?;
    crate::report_backlinks::mcp_page(
        ctx.repo.as_ref(),
        &track_id,
        after,
        LINKS_PER_PAGE,
        paging::PAGE_BYTES,
    )
    .await
    .map_err(|error| RpcError::internal(format!("neige_link_ls: {error}")))?
    .ok_or_else(|| paging::foreign_cursor(TOOL_LINK_LS, cursor.unwrap_or_default()))
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
