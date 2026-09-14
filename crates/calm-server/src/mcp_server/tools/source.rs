//! #1669 §2.2 — `calm.source.capture` and `calm.source.list`, the
//! Planner's way to turn a plugin result it just read into a stable,
//! verifiable source and to name anchors in it.
//!
//! Both handlers check `require_role(Planner)` themselves: discovery
//! hiding and the worker allowlist are not kernel refusals, tools still
//! dispatch by name. `MCP_TOOL_ALLOWLIST` (`dedicated_codex/policy.rs`)
//! deliberately does not list these (design §6).
//!
//! `capture` has three mutually exclusive shapes (§2.2 field matrix):
//!
//! | branch | required | forbidden |
//! |---|---|---|
//! | `call` | `call.tool`, `provenance ∈ {full_text, summary, web_page}`, `title` | `manual`, `source_id` |
//! | `manual` | `manual.text`, `provenance == "manual"`, `title` | `call`, `source_id` |
//! | append | `source_id`, non-empty `quotes` | `call`, `manual`, `title`, `provenance` |
//!
//! Errors: shape, missing fields, an unresolvable call, a quote that is
//! not a byte substring, a branch conflict → `-32602` with the reason;
//! a non-Planner → `-32602` (`require_role`); the per-track quota →
//! `-32403` (`CalmError::Forbidden`, reason `quota`).

use std::sync::Arc;

use serde_json::{Map, Value, json};

use crate::error::CalmError;
use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{
    AppContext, ToolCallIdentity, ToolDescriptor, ToolHandler, ToolHandlerFuture, ToolRegistry,
    read_only_annotations, require_role, role_gated_write_annotations,
};
use crate::mcp_server::transport::worker_grants::codex_sanitized;
use crate::model::CardRole;
use crate::plugin_results::{ARGS_CANON_VERSION, Recorded, ResultStatus, args_sha256};
use crate::report_sources::{
    self, Detail, MAX_BODY_BYTES, MAX_BODY_BYTES_PER_TRACK, MAX_CONTENT_ID_BYTES,
    MAX_PUBLISHED_AT_BYTES, MAX_SOURCES_PER_TRACK, MAX_TITLE_BYTES, MAX_URL_BYTES, NewSource,
    Origin, Provenance, Quote, SourceRow, append_quotes, captured_at_text, store,
};
use calm_types::report_source_links::is_source_id;

pub const TOOL_SOURCE_CAPTURE: &str = "calm.source.capture";
pub const TOOL_SOURCE_LIST: &str = "calm.source.list";

/// Wording shared by every "nothing recorded" refusal, so the Planner
/// reads the same list of causes whatever the lookup that missed.
const NO_RECORD: &str = "no recorded result for this call in this track \
                         (expired, evicted, never made, or made by a worker)";

pub fn register_into(registry: &mut ToolRegistry) {
    registry.register(capture_descriptor(), wrap(source_capture));
    registry.register(list_descriptor(), wrap(source_list));
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

fn capture_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_SOURCE_CAPTURE.into(),
        description: include_str!("../../../prompts/tools/calm.source.capture.md")
            .trim_end()
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "call": {
                    "type": "object",
                    "properties": {
                        "tool": { "type": "string" },
                        "args": {}
                    },
                    "required": ["tool"],
                    "additionalProperties": false
                },
                "manual": {
                    "type": "object",
                    "properties": {
                        "text": { "type": "string" },
                        "url": { "type": "string" }
                    },
                    "required": ["text"],
                    "additionalProperties": false
                },
                "source_id": { "type": "string" },
                "provenance": {
                    "type": "string",
                    "enum": ["full_text", "summary", "web_page", "manual"]
                },
                "title": { "type": "string" },
                "published_at": { "type": "string" },
                "content_id": { "type": "string" },
                "quotes": { "type": "array", "items": { "type": "string" } }
            },
            "additionalProperties": false
        }),
        annotations: Some(role_gated_write_annotations()),
        visible_to_roles: &[CardRole::Planner],
    }
}

fn list_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_SOURCE_LIST.into(),
        description: include_str!("../../../prompts/tools/calm.source.list.md")
            .trim_end()
            .to_string(),
        input_schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        annotations: Some(read_only_annotations()),
        visible_to_roles: &[CardRole::Planner],
    }
}

// ---------------------------------------------------------------------------
// calm.source.capture
// ---------------------------------------------------------------------------

const CAPTURE_KEYS: &[&str] = &[
    "call",
    "manual",
    "source_id",
    "provenance",
    "title",
    "published_at",
    "content_id",
    "quotes",
];

async fn source_capture(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Planner)?;
    let tool = TOOL_SOURCE_CAPTURE;
    let track_id = identity
        .track_id
        .clone()
        .ok_or_else(|| RpcError::invalid_params(format!("{tool}: caller has no track")))?;
    let obj = require_object(&args, tool)?;
    reject_unknown_keys(obj, CAPTURE_KEYS, tool)?;
    // A key is "present" when it is in the object, whatever its value: a
    // `manual: null` next to `call` is still two branches named at once,
    // and `title: null` on the append branch is still a field that would
    // be dropped. (`call.args: null` alone means "omitted"; the tool
    // description says so.)
    let has = |key: &str| obj.contains_key(key);
    match (has("call"), has("manual"), has("source_id")) {
        (true, false, false) => capture_call(&ctx, &track_id, obj, tool).await,
        (false, true, false) => capture_manual(&ctx, &track_id, obj, tool).await,
        (false, false, true) => append_anchors(&ctx, &track_id, obj, tool).await,
        (false, false, false) => Err(RpcError::invalid_params(format!(
            "{tool}: one of `call`, `manual`, `source_id` is required"
        ))),
        _ => Err(RpcError::invalid_params(format!(
            "{tool}: `call`, `manual` and `source_id` are mutually exclusive"
        ))),
    }
}

/// The fields every new source needs, parsed the same way on both the
/// `call` and the `manual` branch.
struct NewSourceFields {
    provenance: Provenance,
    title: String,
    published_at: Option<String>,
    content_id: Option<String>,
    quotes: Vec<String>,
}

fn new_source_fields(obj: &Map<String, Value>, tool: &str) -> Result<NewSourceFields, RpcError> {
    let provenance_text = required_string(obj, "provenance", tool)?;
    let provenance = Provenance::parse(&provenance_text).ok_or_else(|| {
        RpcError::invalid_params(format!(
            "{tool}: `provenance` must be one of full_text, summary, web_page, manual"
        ))
    })?;
    let title = required_string(obj, "title", tool)?;
    if title.trim().is_empty() {
        return Err(RpcError::invalid_params(format!(
            "{tool}: `title` is empty"
        )));
    }
    bounded(&title, MAX_TITLE_BYTES, "title", tool)?;
    let published_at = optional_string(obj, "published_at", tool)?;
    if let Some(published_at) = published_at.as_deref() {
        bounded(published_at, MAX_PUBLISHED_AT_BYTES, "published_at", tool)?;
        validate_published_at(published_at, tool)?;
    }
    let content_id = optional_string(obj, "content_id", tool)?;
    if let Some(content_id) = content_id.as_deref() {
        bounded(content_id, MAX_CONTENT_ID_BYTES, "content_id", tool)?;
    }
    let quotes = optional_quotes(obj, tool)?.unwrap_or_default();
    Ok(NewSourceFields {
        provenance,
        title,
        published_at,
        content_id,
        quotes,
    })
}

/// `YYYY-MM-DD` or an RFC 3339 timestamp; stored as given.
fn validate_published_at(text: &str, tool: &str) -> Result<(), RpcError> {
    let date_ok = chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d").is_ok();
    let datetime_ok = chrono::DateTime::parse_from_rfc3339(text).is_ok();
    if date_ok || datetime_ok {
        Ok(())
    } else {
        Err(RpcError::invalid_params(format!(
            "{tool}: `published_at` must be an ISO date (YYYY-MM-DD) or RFC 3339 timestamp"
        )))
    }
}

async fn capture_call(
    ctx: &Arc<AppContext>,
    track_id: &str,
    obj: &Map<String, Value>,
    tool: &str,
) -> Result<Value, RpcError> {
    let fields = new_source_fields(obj, tool)?;
    if fields.provenance == Provenance::Manual {
        return Err(RpcError::invalid_params(format!(
            "{tool}: provenance `manual` requires `manual`, not `call`"
        )));
    }
    let call = obj
        .get("call")
        .and_then(Value::as_object)
        .ok_or_else(|| RpcError::invalid_params(format!("{tool}: `call` must be an object")))?;
    reject_unknown_keys(call, &["tool", "args"], tool)?;
    let requested_tool = required_string(call, "tool", tool)?;
    let call_args = match call.get("args") {
        None | Some(Value::Null) => None,
        Some(args) => Some(args.clone()),
    };

    let (plugin_id, tool_name) = resolve_recorded_tool(ctx, track_id, &requested_tool, tool)?;
    let recorded = match &call_args {
        Some(args) => ctx
            .plugin_results
            .get(track_id, &plugin_id, &tool_name, &args_sha256(args)),
        None => ctx.plugin_results.latest(track_id, &plugin_id, &tool_name),
    }
    .ok_or_else(|| RpcError::invalid_params(format!("{tool}: {NO_RECORD}")))?;
    let body = recorded_body(&recorded, tool)?;
    let matched_args = match (&call_args, recorded.args_canonical.as_deref()) {
        (_, Some(canonical)) => serde_json::from_str::<Value>(canonical)
            .map_err(|e| RpcError::internal(format!("{tool}: recorded args are not JSON: {e}")))?,
        (Some(args), None) => args.clone(),
        (None, None) => Value::Null,
    };
    let matched_call = json!({
        "tool": recorded.registry_name(),
        "args": matched_args,
        "completed_at": captured_at_text(recorded.completed_at),
    });
    let origin = Origin::Plugin {
        plugin_id,
        tool: tool_name,
        args_sha256: recorded.args_sha256.clone(),
        args_canon: ARGS_CANON_VERSION.to_string(),
        content_id: fields.content_id.clone(),
    };
    let mut receipt = insert_source(ctx, track_id, fields, origin, body, tool).await?;
    receipt
        .as_object_mut()
        .expect("receipt is an object")
        .insert("matched_call".into(), matched_call);
    Ok(receipt)
}

/// `call.tool` → the recorded `(plugin_id, tool_name)` it names. Candidates
/// are only the tools with a live entry in this track (the resolver's
/// universe, §2.2): an exact registry name wins; otherwise the
/// Codex-sanitized spelling must match exactly one of them.
fn resolve_recorded_tool(
    ctx: &Arc<AppContext>,
    track_id: &str,
    requested: &str,
    tool: &str,
) -> Result<(String, String), RpcError> {
    let recorded = ctx.plugin_results.recorded_tools(track_id);
    if let Some(exact) = recorded.iter().find(|(plugin_id, tool_name)| {
        crate::plugin_results::registry_name(plugin_id, tool_name) == requested
    }) {
        return Ok(exact.clone());
    }
    let key = codex_sanitized(requested);
    let hits: Vec<&(String, String)> = recorded
        .iter()
        .filter(|(plugin_id, tool_name)| {
            codex_sanitized(&crate::plugin_results::registry_name(plugin_id, tool_name)) == key
        })
        .collect();
    match hits.as_slice() {
        [one] => Ok((*one).clone()),
        [] => Err(RpcError::invalid_params(format!(
            "{tool}: `call.tool` {requested:?}: {NO_RECORD}"
        ))),
        several => {
            let names: Vec<String> = several
                .iter()
                .map(|(plugin_id, tool_name)| {
                    crate::plugin_results::registry_name(plugin_id, tool_name)
                })
                .collect();
            Err(RpcError::invalid_params(format!(
                "{tool}: `call.tool` {requested:?} is ambiguous; use one exact registry name: {}",
                names.join(", ")
            )))
        }
    }
}

fn recorded_body(recorded: &Recorded, tool: &str) -> Result<String, RpcError> {
    let text = match &recorded.status {
        ResultStatus::Ok { text } => text,
        ResultStatus::Error => {
            return Err(RpcError::invalid_params(format!(
                "{tool}: the recorded call returned isError; call the tool again first"
            )));
        }
        ResultStatus::NoText => {
            return Err(RpcError::invalid_params(format!(
                "{tool}: the recorded call returned no text block"
            )));
        }
        ResultStatus::TooLarge => {
            return Err(RpcError::invalid_params(format!(
                "{tool}: the recorded result or its args exceed the size limit"
            )));
        }
    };
    if text.len() > MAX_BODY_BYTES {
        return Err(RpcError::invalid_params(format!(
            "{tool}: the recorded text is {} bytes; a source body is at most {MAX_BODY_BYTES}",
            text.len()
        )));
    }
    Ok(text.clone())
}

async fn capture_manual(
    ctx: &Arc<AppContext>,
    track_id: &str,
    obj: &Map<String, Value>,
    tool: &str,
) -> Result<Value, RpcError> {
    let fields = new_source_fields(obj, tool)?;
    if fields.provenance != Provenance::Manual {
        return Err(RpcError::invalid_params(format!(
            "{tool}: `manual` requires provenance `manual`, got `{}`",
            fields.provenance.as_str()
        )));
    }
    let manual = obj
        .get("manual")
        .and_then(Value::as_object)
        .ok_or_else(|| RpcError::invalid_params(format!("{tool}: `manual` must be an object")))?;
    reject_unknown_keys(manual, &["text", "url"], tool)?;
    let text = required_string(manual, "text", tool)?;
    if text.is_empty() {
        return Err(RpcError::invalid_params(format!(
            "{tool}: `manual.text` is empty"
        )));
    }
    if text.len() > MAX_BODY_BYTES {
        return Err(RpcError::invalid_params(format!(
            "{tool}: `manual.text` is {} bytes; a source body is at most {MAX_BODY_BYTES}",
            text.len()
        )));
    }
    let url = optional_string(manual, "url", tool)?;
    if let Some(url) = url.as_deref() {
        bounded(url, MAX_URL_BYTES, "manual.url", tool)?;
    }
    let origin = Origin::Manual {
        url,
        content_id: fields.content_id.clone(),
    };
    insert_source(ctx, track_id, fields, origin, text, tool).await
}

/// The shared tail of both capture branches: anchors, quota, id, row.
async fn insert_source(
    ctx: &Arc<AppContext>,
    track_id: &str,
    fields: NewSourceFields,
    origin: Origin,
    body: String,
    tool: &str,
) -> Result<Value, RpcError> {
    let (quotes, mapping) =
        append_quotes(&body, &[], &fields.quotes).map_err(|e| map_err(tool, e))?;
    let body_sha256 = report_sources::sha256_hex(body.as_bytes());
    let body_bytes = body.len();
    let captured_at = crate::model::now_ms();
    let row = NewSource {
        source_id: String::new(),
        provenance: fields.provenance,
        origin,
        title: fields.title,
        published_at: fields.published_at,
        body,
        body_sha256: body_sha256.clone(),
        captured_at,
        quotes,
    };
    let track = track_id.to_string();
    let source_id = crate::db::write_in_tx_typed(ctx.repo.as_ref(), move |tx| {
        Box::pin(async move {
            let (count, bytes) = store::count_and_bytes_tx(tx, &track).await?;
            if count >= MAX_SOURCES_PER_TRACK {
                return Err(CalmError::Forbidden(format!(
                    "quota: track already has {MAX_SOURCES_PER_TRACK} sources"
                )));
            }
            let body_len = i64::try_from(row.body.len()).unwrap_or(i64::MAX);
            if bytes.saturating_add(body_len) > MAX_BODY_BYTES_PER_TRACK {
                return Err(CalmError::Forbidden(format!(
                    "quota: track source bodies would exceed {MAX_BODY_BYTES_PER_TRACK} bytes"
                )));
            }
            let mut row = row;
            row.source_id = mint_unique_source_id_tx(tx, &track, mint_source_id).await?;
            store::insert_tx(tx, &track, &row).await?;
            Ok(row.source_id)
        })
    })
    .await
    .map_err(|e| map_err(tool, e))?;
    Ok(json!({
        "source_id": source_id,
        "provenance": fields.provenance.as_str(),
        "quotes": quotes_json(&mapping),
        "body_bytes": body_bytes,
        "body_sha256": body_sha256,
    }))
}

/// `src_` + 8 lowercase hex digits. Not a secret: uniqueness is per track
/// and [`mint_unique_source_id_tx`] re-mints on a collision.
fn mint_source_id() -> String {
    format!("src_{:08x}", rand::random::<u32>())
}

/// How many mints a capture tries before giving up. 32 random bits against
/// at most [`MAX_SOURCES_PER_TRACK`] rows never gets near this in practice;
/// the bound exists so a broken generator cannot spin the transaction
/// forever.
const MAX_MINT_ATTEMPTS: usize = 8;

/// A source id no row of `track_id` uses, from up to [`MAX_MINT_ATTEMPTS`]
/// draws of `mint`; `Internal` when every draw collided.
async fn mint_unique_source_id_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    track_id: &str,
    mut mint: impl FnMut() -> String,
) -> Result<String, CalmError> {
    for _ in 0..MAX_MINT_ATTEMPTS {
        let candidate = mint();
        if !store::exists_tx(tx, track_id, &candidate).await? {
            return Ok(candidate);
        }
    }
    Err(CalmError::Internal(format!(
        "report_sources: no free source id after {MAX_MINT_ATTEMPTS} attempts"
    )))
}

async fn append_anchors(
    ctx: &Arc<AppContext>,
    track_id: &str,
    obj: &Map<String, Value>,
    tool: &str,
) -> Result<Value, RpcError> {
    for forbidden in ["title", "provenance", "published_at", "content_id"] {
        if obj.contains_key(forbidden) {
            return Err(RpcError::invalid_params(format!(
                "{tool}: `{forbidden}` is not accepted with `source_id`"
            )));
        }
    }
    let source_id = required_string(obj, "source_id", tool)?;
    if !is_source_id(&source_id) {
        return Err(RpcError::invalid_params(format!(
            "{tool}: `source_id` {source_id:?} is not a source id"
        )));
    }
    let requested = optional_quotes(obj, tool)?.unwrap_or_default();
    if requested.is_empty() {
        return Err(RpcError::invalid_params(format!(
            "{tool}: `quotes` must be a non-empty array with `source_id`"
        )));
    }
    let track = track_id.to_string();
    let id = source_id.clone();
    let (provenance, mapping) = crate::db::write_in_tx_typed(ctx.repo.as_ref(), move |tx| {
        Box::pin(async move {
            let row = store::get_tx(tx, &track, &id, Detail::Full)
                .await?
                .ok_or_else(|| {
                    CalmError::BadRequest(format!("source `{id}` not found in this track"))
                })?;
            let body = row.body.unwrap_or_default();
            let (quotes, mapping) = append_quotes(&body, &row.quotes, &requested)?;
            if quotes.len() != row.quotes.len() {
                store::set_quotes_tx(tx, &track, &id, &quotes).await?;
            }
            Ok((row.provenance, mapping))
        })
    })
    .await
    .map_err(|e| map_err(tool, e))?;
    Ok(json!({
        "source_id": source_id,
        "provenance": provenance.as_str(),
        "quotes": quotes_json(&mapping),
    }))
}

fn quotes_json(mapping: &[(String, String)]) -> Vec<Value> {
    mapping
        .iter()
        .map(|(text, id)| json!({ "id": id, "text": text }))
        .collect()
}

// ---------------------------------------------------------------------------
// calm.source.list
// ---------------------------------------------------------------------------

async fn source_list(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Planner)?;
    let tool = TOOL_SOURCE_LIST;
    let track_id = identity
        .track_id
        .as_deref()
        .ok_or_else(|| RpcError::invalid_params(format!("{tool}: caller has no track")))?;
    if !args.is_null() {
        let obj = require_object(&args, tool)?;
        reject_unknown_keys(obj, &[], tool)?;
    }
    let pool = ctx
        .sqlite_pool
        .as_ref()
        .ok_or_else(|| RpcError::internal(format!("{tool}: requires a sqlite-backed repo")))?;
    let rows = store::list(pool, track_id)
        .await
        .map_err(|e| map_err(tool, e))?;
    let sources: Vec<Value> = rows.iter().map(list_entry).collect();
    Ok(json!({ "sources": sources }))
}

/// One `calm.source.list` entry: everything but the body.
pub fn list_entry(row: &SourceRow) -> Value {
    let mut entry = json!({
        "source_id": row.source_id,
        "provenance": row.provenance.as_str(),
        "title": row.title,
        "published_at": row.published_at,
        "quotes": row
            .quotes
            .iter()
            .map(|Quote { id, text, .. }| json!({ "id": id, "text": text }))
            .collect::<Vec<_>>(),
        "body_bytes": row.body_bytes,
        "captured_at": captured_at_text(row.captured_at),
    });
    let object = entry.as_object_mut().expect("entry is an object");
    if let Some(content_id) = row.origin.content_id() {
        object.insert("content_id".into(), json!(content_id));
    }
    if let Some(url) = row.origin.url() {
        object.insert("url".into(), json!(url));
    }
    entry
}

// ---------------------------------------------------------------------------
// Shared plumbing
// ---------------------------------------------------------------------------

fn map_err(tool: &str, e: CalmError) -> RpcError {
    match e {
        CalmError::BadRequest(m) => RpcError::invalid_params(format!("{tool}: {m}")),
        CalmError::Forbidden(m) => RpcError::custom(-32403, format!("{tool}: forbidden: {m}")),
        other => RpcError::internal(format!("{tool}: {other}")),
    }
}

fn require_object<'a>(args: &'a Value, tool: &str) -> Result<&'a Map<String, Value>, RpcError> {
    args.as_object()
        .ok_or_else(|| RpcError::invalid_params(format!("{tool}: arguments must be an object")))
}

fn reject_unknown_keys(
    obj: &Map<String, Value>,
    allowed: &[&str],
    tool: &str,
) -> Result<(), RpcError> {
    if let Some(key) = obj.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(RpcError::invalid_params(format!(
            "{tool}: unknown key `{key}`"
        )));
    }
    Ok(())
}

fn required_string(obj: &Map<String, Value>, key: &str, tool: &str) -> Result<String, RpcError> {
    obj.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| RpcError::invalid_params(format!("{tool}: missing `{key}` (string)")))
}

fn optional_string(
    obj: &Map<String, Value>,
    key: &str,
    tool: &str,
) -> Result<Option<String>, RpcError> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(RpcError::invalid_params(format!(
            "{tool}: `{key}` must be a string if provided"
        ))),
    }
}

fn optional_quotes(obj: &Map<String, Value>, tool: &str) -> Result<Option<Vec<String>>, RpcError> {
    match obj.get("quotes") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(items)) => items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                item.as_str().map(str::to_string).ok_or_else(|| {
                    RpcError::invalid_params(format!("{tool}: quotes[{index}] must be a string"))
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some),
        Some(_) => Err(RpcError::invalid_params(format!(
            "{tool}: `quotes` must be an array of strings"
        ))),
    }
}

fn bounded(value: &str, max: usize, key: &str, tool: &str) -> Result<(), RpcError> {
    if value.len() > max {
        return Err(RpcError::invalid_params(format!(
            "{tool}: `{key}` exceeds {max} bytes"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod mint_tests {
    use super::*;
    use crate::db::sqlite::SqlxRepo;
    use crate::db::write_in_tx_typed;

    async fn seed(repo: &SqlxRepo, track_id: &str, source_id: &str) {
        let (track, id) = (track_id.to_string(), source_id.to_string());
        write_in_tx_typed(repo, move |tx| {
            Box::pin(async move {
                sqlx::query(concat!(
                    "INSERT INTO report_sources ",
                    "(track_id, source_id, provenance, origin, title, published_at, ",
                    " body, body_sha256, captured_at, quotes) ",
                    "VALUES (?1, ?2, 'manual', '{\"kind\":\"manual\"}', 't', NULL, ",
                    " 'b', 'h', 1, '[]')"
                ))
                .bind(track)
                .bind(id)
                .execute(&mut **tx)
                .await?;
                Ok(())
            })
        })
        .await
        .expect("seed row");
    }

    async fn repo_with_track() -> (SqlxRepo, String) {
        let repo = SqlxRepo::open("sqlite::memory:").await.expect("sqlite");
        let track = {
            use crate::db::RepoSyncDomainRaw;
            let area = repo
                .area_create(crate::model::NewArea {
                    name: "mint".into(),
                    color: "#000".into(),
                    sort: None,
                })
                .await
                .expect("area");
            repo.track_create(crate::model::NewTrack {
                template_input: None,
                area_id: area.id,
                title: "mint".into(),
                sort: None,
                cwd: String::new(),
                template_id: None,
                plugin_scope: None,
                attach_folder: false,
                theme: crate::routes::theme::RequestTheme::default_dark(),
            })
            .await
            .expect("track")
            .id
            .to_string()
        };
        (repo, track)
    }

    #[tokio::test]
    async fn mint_skips_a_collision_and_gives_up_after_the_bound() {
        let (repo, track) = repo_with_track().await;
        seed(&repo, &track, "src_00000001").await;
        // One collision, then a free id.
        let t = track.clone();
        let minted = write_in_tx_typed(&repo, move |tx| {
            Box::pin(async move {
                let mut draws = ["src_00000001", "src_00000002"].into_iter();
                mint_unique_source_id_tx(tx, &t, || draws.next().expect("draw").to_string()).await
            })
        })
        .await
        .expect("minted");
        assert_eq!(minted, "src_00000002");
        // Every draw collides: bounded, Internal.
        let t = track.clone();
        let mut draws = 0usize;
        let err = write_in_tx_typed(&repo, move |tx| {
            Box::pin(async move {
                let result = mint_unique_source_id_tx(tx, &t, || {
                    draws += 1;
                    "src_00000001".to_string()
                })
                .await;
                result
                    .map(|id| (id, draws))
                    .map_err(|e| CalmError::Internal(format!("{e} after {draws} draws")))
            })
        })
        .await
        .unwrap_err();
        assert!(
            matches!(&err, CalmError::Internal(m) if m.contains("no free source id") && m.contains("after 8 draws")),
            "{err}"
        );
    }
}
