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
    let first_line = markdown.lines().next().unwrap_or("");
    let first_non_empty = markdown
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("");
    let hashes = first_line.bytes().take_while(|byte| *byte == b'#').count();
    let heading = if (1..=6).contains(&hashes)
        && first_line
            .as_bytes()
            .get(hashes)
            .is_none_or(|byte| *byte == b' ')
    {
        first_line
            .trim_start_matches('#')
            .strip_prefix(' ')
            .unwrap_or(first_line)
    } else {
        first_non_empty.trim()
    };
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
