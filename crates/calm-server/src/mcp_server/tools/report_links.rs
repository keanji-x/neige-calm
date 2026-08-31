//! Spec-only discovery reads for wave-report links (#967 S4).

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{Map, Value, json};

use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{
    AppContext, ToolCallIdentity, ToolDescriptor, ToolHandler, ToolHandlerFuture, ToolRegistry,
    read_only_annotations, require_role,
};
use crate::model::CardRole;
use crate::wave_report_read::load_report_read_snapshot;

pub const TOOL_COVE_OUTLINE: &str = "calm.cove.outline";
pub const TOOL_REPORT_BACKLINKS: &str = "calm.report.links.backlinks";

const MAX_WAVES: usize = 50;
const MAX_BLOCKS_PER_WAVE: usize = 40;
const MAX_RESPONSE_BYTES: usize = 32 * 1024;

pub fn register_into(registry: &mut ToolRegistry) {
    registry.register(outline_descriptor(), wrap(cove_outline));
    registry.register(backlinks_descriptor(), wrap(report_backlinks));
}

fn wrap<F, Fut>(f: F) -> ToolHandler
where
    F: Fn(Arc<AppContext>, ToolCallIdentity, Value) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<Value, RpcError>> + Send + 'static,
{
    Arc::new(move |ctx, identity, args| -> ToolHandlerFuture { Box::pin(f(ctx, identity, args)) })
}

fn outline_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_COVE_OUTLINE.into(),
        description: "Spec-only: list the reports and addressable block index for every wave in \
            the caller's cove. Takes no parameters. Create links as \
            `[label](neige://wave/<wave_id>#<block_id>)`; the `#<block_id>` fragment is optional. \
            Block ids come from this outline or `calm.report.read`. Links resolve only within the \
            cove. If an anchored block no longer exists, the link degrades to a whole-report link \
            instead of breaking. Returns `{ waves: [{ id, title, lifecycle, blocks: [{ id, kind, \
            heading }] }], truncated? }`; it never returns report bodies."
            .into(),
        input_schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        annotations: Some(read_only_annotations()),
        visible_to_roles: &[CardRole::Spec],
    }
}

fn backlinks_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_REPORT_BACKLINKS.into(),
        description: "Spec-only: list report links from waves in the same cove to the caller's \
            own wave. Takes no parameters. Link syntax is \
            `[label](neige://wave/<wave_id>#<block_id>)`; the fragment is optional, and block ids \
            come from `calm.cove.outline` or `calm.report.read`. Links resolve only within the \
            cove. An anchor whose block no longer exists degrades to a whole-report link rather \
            than breaking."
            .into(),
        input_schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        annotations: Some(read_only_annotations()),
        visible_to_roles: &[CardRole::Spec],
    }
}

async fn cove_outline(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    _args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Spec)?;
    let mut cards = ctx
        .repo
        .wave_report_cards_by_cove(identity.cove_id.as_str())
        .await
        .map_err(|error| RpcError::internal(format!("cove_outline: {error}")))?;
    cards.sort_by(|left, right| left.wave_id.as_str().cmp(right.wave_id.as_str()));

    let total_waves = cards.len();
    let mut waves = Vec::new();
    let mut block_truncations = BTreeMap::new();
    for card in cards.into_iter().take(MAX_WAVES) {
        let wave = ctx
            .repo
            .wave_get(card.wave_id.as_str())
            .await
            .map_err(|error| RpcError::internal(format!("cove_outline: {error}")))?
            .ok_or_else(|| RpcError::internal("cove_outline: wave vanished mid-read"))?;
        let snapshot = load_report_read_snapshot(ctx.repo.as_ref(), card.id.as_str())
            .await
            .map_err(|error| RpcError::internal(format!("cove_outline: {error}")))?;
        let omitted = snapshot.blocks.len().saturating_sub(MAX_BLOCKS_PER_WAVE);
        if omitted > 0 {
            block_truncations.insert(wave.id.as_str().to_string(), omitted);
        }
        let blocks: Vec<Value> = snapshot
            .blocks
            .iter()
            .take(MAX_BLOCKS_PER_WAVE)
            .map(|block| {
                json!({
                    "id": block.id,
                    "kind": block.kind,
                    "heading": block_heading(block),
                })
            })
            .collect();
        waves.push(json!({
            "id": wave.id,
            "title": wave.title,
            "lifecycle": wave.lifecycle,
            "blocks": blocks,
        }));
    }

    let mut omitted_waves = total_waves.saturating_sub(MAX_WAVES);
    let initial = outline_response(waves.clone(), omitted_waves, &block_truncations, false);
    let mut estimated_bytes =
        serde_json::to_vec(&initial).map_or(usize::MAX, |serialized| serialized.len());
    let bytes_truncated = estimated_bytes > MAX_RESPONSE_BYTES;
    // Reserve room for truncation metadata, then remove entries in one reverse
    // pass. Each removed value is serialized once; the whole response is only
    // serialized again for the final cap confirmation.
    let target_bytes = MAX_RESPONSE_BYTES.saturating_sub(4096);
    if bytes_truncated {
        for wave in waves.iter_mut().rev() {
            let Some(wave_id) = wave.get("id").and_then(Value::as_str).map(str::to_owned) else {
                continue;
            };
            let Some(blocks) = wave.get_mut("blocks").and_then(Value::as_array_mut) else {
                continue;
            };
            while estimated_bytes > target_bytes {
                let Some(block) = blocks.pop() else {
                    break;
                };
                estimated_bytes = estimated_bytes.saturating_sub(
                    serde_json::to_vec(&block).map_or(0, |serialized| serialized.len() + 1),
                );
                *block_truncations.entry(wave_id.clone()).or_default() += 1;
            }
        }
        while estimated_bytes > target_bytes {
            let Some(wave) = waves.pop() else {
                break;
            };
            estimated_bytes = estimated_bytes.saturating_sub(
                serde_json::to_vec(&wave).map_or(0, |serialized| serialized.len() + 1),
            );
            omitted_waves += 1;
            if let Some(wave_id) = wave.get("id").and_then(Value::as_str) {
                block_truncations.remove(wave_id);
            }
        }
    }
    let response = outline_response(waves, omitted_waves, &block_truncations, bytes_truncated);
    if serde_json::to_vec(&response).map_or(usize::MAX, |serialized| serialized.len())
        > MAX_RESPONSE_BYTES
    {
        return Err(RpcError::internal(
            "cove_outline: truncation metadata exceeds response byte cap",
        ));
    }
    Ok(response)
}

fn outline_response(
    waves: Vec<Value>,
    omitted_waves: usize,
    block_truncations: &BTreeMap<String, usize>,
    bytes_truncated: bool,
) -> Value {
    let mut response = Map::from_iter([("waves".into(), Value::Array(waves))]);
    let mut truncated = Map::new();
    if omitted_waves > 0 {
        truncated.insert("waves".into(), json!(omitted_waves));
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

fn block_heading(block: &calm_types::wave_report::ReportBlock) -> String {
    if block.kind != calm_types::report_blocks::KIND_PROSE {
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
    // An outline summarizes what a reader will see, so the title is taken from
    // the block's VISIBLE TEXT, not from its source. `scan_links().plain` is
    // the workspace's one CommonMark projection of that — it drops `Event::Html`
    // and `Event::InlineHtml` and keeps `Event::Text` / `Event::Code`
    // (`calm_types::report_links`), and `report_backlinks` already quotes from
    // the same projection, so an outline title and a backlink quote can never
    // disagree about what a block says.
    //
    // Reusing it, rather than scanning the source for `<!-- … -->`, is the
    // whole point: a substring scanner does not respect Markdown boundaries,
    // so a block starting with the inline code `` `<!-- x -->` `` would render
    // those characters and be titled without them. The case that makes any of
    // this matter is a document carrying its own maintenance contract in a
    // leading comment (#1185 §0.7): it renders as nothing, so it may not echo
    // into the outline of every wave — as noise and against the response byte
    // budget. The block itself stays, with an empty heading; dropping it would
    // make it undeep-linkable, and this outline is the only source of block ids.
    //
    // No ATX handling here, deliberately: `plain` already emits a heading's
    // text without its `#` markers. Re-stripping leading `#` would eat the
    // visible characters of a block whose first line is the *code* `# x`.
    //
    // Known divergence, accepted: `plain` drops the alt text of a standalone
    // image (`report_links.rs`'s `standalone_image_alt_is_not_plain_text`),
    // while `fe/` renders alt as the `<img>`'s accessible name. So an
    // image-only prose block gets an empty title. That is the right trade —
    // the block keeps its id and stays linkable, the outline is a text index
    // and an alt string is not the image, and the alternative is a second
    // hand-written projection of Markdown to text, which is exactly the class
    // of bug this replaced. If image-only blocks ever need titles, teach
    // `plain` about them once, for both readers.
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
    require_role(&identity, CardRole::Spec)?;
    let wave_id = identity.wave_id.ok_or_else(|| {
        RpcError::invalid_params("calm.report.links.backlinks requires a wave-scoped caller")
    })?;
    let page = crate::report_backlinks::backlinks_for_wave(ctx.repo.as_ref(), &wave_id)
        .await
        .map_err(|error| RpcError::internal(format!("report_backlinks: {error}")))?;
    Ok(crate::report_backlinks::mcp_payload(&page))
}

#[cfg(test)]
mod tests {
    use super::block_heading;
    use calm_types::wave_report::ReportBlock;
    use serde_json::json;

    fn prose(markdown: &str) -> ReportBlock {
        ReportBlock {
            id: "b_0000".into(),
            kind: calm_types::report_blocks::KIND_PROSE.into(),
            rev: 1,
            payload: json!({ "markdown": markdown }),
        }
    }

    /// The multi-line shape matters: a CommonMark HTML block of type 2 is *not*
    /// terminated by a blank line, so a real maintenance contract is one node
    /// spanning blank lines (#1185 §4.4 F).
    const CONTRACT: &str = "<!-- 报告维护契约（渲染时被丢弃，读 body 源码的主体看得到）\n\
        \n\
        这份报告自带的结构就是规则：维护它，不要重写它。\n\
        \n\
        写作方式：散文正文控制在 1000 字以内。\n\
        -->\n\n";

    #[test]
    fn a_comment_only_prose_block_has_an_empty_heading() {
        // The block stays in the outline (it must remain deep-linkable); only
        // its title is empty, because it renders as nothing.
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
        // A block-level `<!--` that is never closed runs to the end of the
        // document in CommonMark, so the renderer shows nothing and the outline
        // must not resurrect text nobody can see.
        assert_eq!(
            block_heading(&prose("<!-- 报告维护契约\n\n# 概要\n\n本轮结论。\n")),
            ""
        );
    }

    #[test]
    fn a_comment_inside_inline_code_is_visible_text_and_keeps_its_characters() {
        // The regression a source-substring scanner introduced: a renderer
        // prints these characters, so the outline must title the block with
        // them. `plain` keeps `Event::Code` and drops only real HTML nodes.
        assert_eq!(
            block_heading(&prose("`<!-- x -->` inline code first\n")),
            "<!-- x --> inline code first"
        );
    }

    #[test]
    fn an_unterminated_comment_inside_a_paragraph_does_not_swallow_the_line() {
        // Not at the start of a block, so the HTML-block rule does not apply;
        // and with no `-->` it is not a valid inline HTML comment either, so
        // CommonMark leaves it as literal TEXT. Both renderers print those
        // characters, so the outline keeps them. The old source scanner cut
        // from `<!--` to the end of the block and lost the whole line.
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
        // The old scanner took `lines().next()` for the ATX test and the first
        // non-empty line for the text, so a body starting with a blank line was
        // titled `# 概要`. `plain` never emits the markers at all.
        assert_eq!(block_heading(&prose("\n# 概要\n\n本轮结论。\n")), "概要");
    }

    #[test]
    fn a_hash_that_is_visible_code_keeps_its_hash() {
        // The other half of the same fix: nothing re-strips `#` after `plain`,
        // so a block whose first visible characters are the code `# x` keeps
        // them.
        assert_eq!(
            block_heading(&prose("`# x` not a heading\n")),
            "# x not a heading"
        );
    }

    #[test]
    fn an_image_only_block_gets_an_empty_heading() {
        // Documented divergence from the front ends, which render alt text:
        // `plain` drops a standalone image's alt. The block keeps its id, so it
        // stays linkable; see the note on `block_heading`.
        assert_eq!(block_heading(&prose("![一张图](chart.png)\n")), "");
    }
}
