//! Read-only MCP file views for the current wave.
//!
//! `calm.wave.ls` and `calm.wave.cat` expose a small path-based view
//! rooted at the wave bound to the caller's MCP connection. The wave is
//! always derived from [`CardIdentity`]; callers never provide a
//! `wave_id`.

use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{
    AppContext, CardIdentity, ToolDescriptor, ToolHandler, ToolHandlerFuture, ToolRegistry,
    require_role,
};
use crate::mcp_server::tools::wave_report;
use crate::model::{Card, CardRole, Wave};
use serde_json::{Value, json};
use std::sync::Arc;

pub const TOOL_WAVE_LS: &str = "calm.wave.ls";
pub const TOOL_WAVE_CAT: &str = "calm.wave.cat";

pub fn register_into(registry: &mut ToolRegistry) {
    registry.register(ls_descriptor(), wrap(wave_ls));
    registry.register(cat_descriptor(), wrap(wave_cat));
}

fn wrap<F, Fut>(f: F) -> ToolHandler
where
    F: Fn(Arc<AppContext>, CardIdentity, Value) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<Value, RpcError>> + Send + 'static,
{
    Arc::new(move |ctx, identity, args| -> ToolHandlerFuture { Box::pin(f(ctx, identity, args)) })
}

fn ls_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_WAVE_LS.into(),
        description: "Spec-only: list file-like read views for the current MCP-bound wave. \
             Accepts optional `{ path }`; `/` lists `index.md`, `wave.json`, \
             `report.md`, and `cards/`. The wave is derived from the bound \
             card identity, never from arguments."
            .into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" }
            }
        }),
    }
}

fn cat_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_WAVE_CAT.into(),
        description: "Spec-only: read one file-like view from the current MCP-bound wave. \
             Supports `index.md`, `wave.json`, `report.md`, `cards/index.json`, \
             `cards/<card_id>/meta.json`, and `cards/<card_id>/payload.json`."
            .into(),
        input_schema: json!({
            "type": "object",
            "required": ["path"],
            "properties": {
                "path": { "type": "string" }
            }
        }),
    }
}

async fn wave_ls(
    ctx: Arc<AppContext>,
    identity: CardIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Spec)?;
    let path = parse_path_arg(&args, false)?;
    let (_, wave) = resolve_wave_for_identity(&ctx, &identity).await?;

    match path.as_str() {
        "" => {
            let cards = cards_for_wave(&ctx, &wave).await?;
            Ok(json!([
                entry_file("index.md", None, Some(wave.updated_at)),
                entry_file("wave.json", None, Some(wave.updated_at)),
                entry_file("report.md", None, Some(wave.updated_at)),
                entry_dir("cards/", Some(cards.len()), None),
            ]))
        }
        "cards" => {
            let cards = cards_for_wave(&ctx, &wave).await?;
            let mut entries = Vec::with_capacity(cards.len() + 1);
            entries.push(entry_file("index.json", None, Some(wave.updated_at)));
            entries.extend(cards.iter().map(|card| {
                entry_dir(
                    &format!("{}/", card.id.as_str()),
                    None,
                    Some(card.updated_at),
                )
            }));
            Ok(Value::Array(entries))
        }
        path if path.starts_with("cards/") => {
            let parts: Vec<&str> = path.split('/').collect();
            if parts.len() != 2 {
                return Err(path_not_available(path));
            }
            let card = card_in_wave(&ctx, &wave, parts[1]).await?;
            Ok(json!([
                entry_file("meta.json", None, Some(card.updated_at)),
                entry_file("payload.json", None, Some(card.updated_at)),
            ]))
        }
        other => Err(path_not_available(other)),
    }
}

async fn wave_cat(
    ctx: Arc<AppContext>,
    identity: CardIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Spec)?;
    let path = parse_path_arg(&args, true)?;
    let (_, wave) = resolve_wave_for_identity(&ctx, &identity).await?;

    match path.as_str() {
        "index.md" => {
            let cards = cards_for_wave(&ctx, &wave).await?;
            Ok(content_markdown(index_markdown(&wave, cards.len())))
        }
        "wave.json" => content_json(&wave),
        "report.md" => {
            let report = wave_report::report_read(ctx, identity, json!({})).await?;
            let body = report
                .get("body")
                .and_then(Value::as_str)
                .ok_or_else(|| RpcError::internal("wave_file: report read returned no body"))?;
            Ok(content_markdown(body.to_string()))
        }
        "cards/index.json" => {
            let cards = cards_for_wave(&ctx, &wave).await?;
            let metas: Vec<Value> = cards.iter().map(|card| card_meta(&ctx, card)).collect();
            content_json(&metas)
        }
        path if path.starts_with("cards/") => {
            let parts: Vec<&str> = path.split('/').collect();
            if parts.len() != 3 {
                return Err(path_not_available(path));
            }
            let card = card_in_wave(&ctx, &wave, parts[1]).await?;
            match parts[2] {
                "meta.json" => content_json(&card_meta(&ctx, &card)),
                "payload.json" => content_json(&card.payload),
                _ => Err(path_not_available(path)),
            }
        }
        other => Err(path_not_available(other)),
    }
}

fn parse_path_arg(args: &Value, required: bool) -> Result<String, RpcError> {
    let obj = args
        .as_object()
        .ok_or_else(|| RpcError::invalid_params("calm.wave: arguments must be an object"))?;
    let Some(raw) = obj.get("path") else {
        if required {
            return Err(RpcError::invalid_params(
                "calm.wave.cat: missing `path` (string)",
            ));
        }
        return Ok(String::new());
    };
    let path = raw
        .as_str()
        .ok_or_else(|| RpcError::invalid_params("calm.wave: `path` must be a string"))?;
    Ok(normalize_path(path))
}

fn normalize_path(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed == "/" {
        return String::new();
    }
    trimmed
        .trim_start_matches('/')
        .trim_end_matches('/')
        .to_string()
}

async fn resolve_wave_for_identity(
    ctx: &Arc<AppContext>,
    identity: &CardIdentity,
) -> Result<(Card, Wave), RpcError> {
    let card_id_str = identity.card_id.as_str().to_string();
    let card = ctx
        .repo
        .card_get(&card_id_str)
        .await
        .map_err(|e| RpcError::internal(format!("wave_file: card lookup: {e}")))?
        .ok_or_else(|| {
            RpcError::internal(format!(
                "wave_file: bound card {card_id_str} not found (deleted mid-connection?)"
            ))
        })?;
    let wave = ctx
        .repo
        .wave_get(card.wave_id.as_str())
        .await
        .map_err(|e| RpcError::internal(format!("wave_file: wave lookup: {e}")))?
        .ok_or_else(|| {
            RpcError::internal(format!(
                "wave_file: wave {} for card {} not found",
                card.wave_id.as_str(),
                card_id_str
            ))
        })?;
    Ok((card, wave))
}

async fn cards_for_wave(ctx: &Arc<AppContext>, wave: &Wave) -> Result<Vec<Card>, RpcError> {
    ctx.repo
        .cards_by_wave(wave.id.as_str())
        .await
        .map_err(|e| RpcError::internal(format!("wave_file: cards_by_wave: {e}")))
}

async fn card_in_wave(ctx: &Arc<AppContext>, wave: &Wave, card_id: &str) -> Result<Card, RpcError> {
    let card = ctx
        .repo
        .card_get(card_id)
        .await
        .map_err(|e| RpcError::internal(format!("wave_file: card lookup: {e}")))?
        .ok_or_else(|| path_not_available(&format!("cards/{card_id}")))?;
    if card.wave_id != wave.id {
        return Err(RpcError::custom(
            -32403,
            format!(
                "wave_file: forbidden: card {} is not in the caller's bound wave {}",
                card.id.as_str(),
                wave.id.as_str()
            ),
        ));
    }
    Ok(card)
}

fn card_meta(ctx: &Arc<AppContext>, card: &Card) -> Value {
    let role = ctx.card_role_cache.get(&card.id).unwrap_or_default();
    json!({
        "id": card.id,
        "kind": card.kind,
        "role": role,
        "sort": card.sort,
        "deletable": card.deletable,
        "created_at": card.created_at,
        "updated_at": card.updated_at,
    })
}

fn index_markdown(wave: &Wave, card_count: usize) -> String {
    format!(
        "# Wave {}\n\n- Title: {}\n- Cards: {}\n- Report: [report.md](report.md)\n",
        wave.id.as_str(),
        wave.title,
        card_count
    )
}

fn content_markdown(content: String) -> Value {
    json!({
        "content": content,
        "content_type": "text/markdown",
    })
}

fn content_json<T: serde::Serialize>(value: &T) -> Result<Value, RpcError> {
    let content = serde_json::to_string_pretty(value)
        .map_err(|e| RpcError::internal(format!("wave_file: json serialization: {e}")))?;
    Ok(json!({
        "content": content,
        "content_type": "application/json",
    }))
}

fn entry_dir(name: &str, size: Option<usize>, updated_at: Option<i64>) -> Value {
    entry(name, "dir", size, updated_at)
}

fn entry_file(name: &str, size: Option<usize>, updated_at: Option<i64>) -> Value {
    entry(name, "file", size, updated_at)
}

fn entry(name: &str, kind: &str, size: Option<usize>, updated_at: Option<i64>) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert("name".into(), Value::String(name.to_string()));
    obj.insert("kind".into(), Value::String(kind.to_string()));
    if let Some(size) = size {
        obj.insert("size".into(), json!(size));
    }
    if let Some(updated_at) = updated_at {
        obj.insert("updated_at".into(), json!(updated_at));
    }
    Value::Object(obj)
}

fn path_not_available(path: &str) -> RpcError {
    RpcError::invalid_params(format!(
        "calm.wave: path not available in this view: {}",
        if path.is_empty() { "/" } else { path }
    ))
}
