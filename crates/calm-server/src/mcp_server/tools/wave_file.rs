//! Read-only MCP file views for the current wave.
//!
//! `calm.wave.ls` and `calm.wave.cat` expose a small path-based view
//! rooted at the wave bound to the caller's MCP connection. The wave is
//! always derived from [`ToolCallIdentity`]; callers never provide a
//! `wave_id`.

use crate::ids::ActorId;
use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{
    AppContext, ToolCallIdentity, ToolDescriptor, ToolHandler, ToolHandlerFuture, ToolRegistry,
    require_role_any,
};
use crate::mcp_server::tools::wave_report;
use crate::model::{Card, CardRole, Wave};
use crate::runtime_lookup::{
    project_runtime_into_card_payload, project_runtime_into_cards_payload,
};
use crate::state::WriteContext;
use crate::{db::WaveEvent, event::Event, event::EventScope};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

pub const TOOL_WAVE_LS: &str = "calm.wave.ls";
pub const TOOL_WAVE_CAT: &str = "calm.wave.cat";
const RESERVED_RUN_KEYS: &[&str] = &["index"];

pub fn register_into(registry: &mut ToolRegistry) {
    registry.register(ls_descriptor(), wrap(wave_ls));
    registry.register(cat_descriptor(), wrap(wave_cat));
}

fn wrap<F, Fut>(f: F) -> ToolHandler
where
    F: Fn(Arc<AppContext>, ToolCallIdentity, Value) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<Value, RpcError>> + Send + 'static,
{
    Arc::new(move |ctx, identity, args| -> ToolHandlerFuture { Box::pin(f(ctx, identity, args)) })
}

/// Return-shape contract consumed by `neige`: `calm.wave.ls` returns a bare
/// JSON array of `{ name, kind, ... }` entries; `calm.wave.cat` returns an
/// object `{ content, content_type }`.
fn ls_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_WAVE_LS.into(),
        description: "Spec/Worker: list file-like read views for the current MCP-bound wave. \
             Accepts optional `{ path }`; `/` lists `index.md`, `wave.json`, \
             `report.md`, `cards/`, and `runs/`; `cards/<card_id>` lists \
             `meta.json`, `payload.json`, `events.json`, and `conversation.md`. \
             The wave is derived from the bound \
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
        description: "Spec/Worker: read one file-like view from the current MCP-bound wave. \
             Supports `index.md`, `wave.json`, `report.md`, `cards/index.json`, \
             `cards/<card_id>/meta.json`, `cards/<card_id>/payload.json`, \
             `cards/<card_id>/events.json`, `cards/<card_id>/conversation.md`, \
             `runs/index.json`, `runs/<idempotency_key>.md`, and \
             `runs/<idempotency_key>.json`."
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
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role_any(&identity, &[CardRole::Spec, CardRole::Worker])?;
    let path = parse_path_arg(&args, false)?;
    let (_, wave) = resolve_wave_for_identity(&ctx, &identity).await?;

    match path.as_str() {
        "" => {
            let cards = cards_for_wave(&ctx, &wave).await?;
            let runs = runs_for_wave(&ctx, &wave).await?;
            Ok(json!([
                entry_file("index.md", None, Some(wave.updated_at)),
                entry_file("wave.json", None, Some(wave.updated_at)),
                entry_file("report.md", None, Some(wave.updated_at)),
                entry_dir("cards/", Some(cards.len()), None),
                entry_dir("runs/", Some(runs.len()), runs_updated_at(&wave, &runs)),
            ]))
        }
        "cards" => {
            let cards = cards_for_wave(&ctx, &wave).await?;
            let cards_updated_at = cards_updated_at(&wave, &cards);
            let mut entries = Vec::with_capacity(cards.len() + 1);
            entries.push(entry_file("index.json", None, Some(cards_updated_at)));
            entries.extend(cards.iter().map(|card| {
                entry_dir(
                    &format!("{}/", card.id.as_str()),
                    None,
                    Some(card.updated_at),
                )
            }));
            Ok(Value::Array(entries))
        }
        "runs" => {
            let runs = runs_for_wave(&ctx, &wave).await?;
            Ok(Value::Array(
                runs.iter().map(run_listing_entry).collect::<Vec<_>>(),
            ))
        }
        path if path.starts_with("cards/") => {
            let parts: Vec<&str> = path.split('/').collect();
            if parts.len() != 2 {
                return Err(path_not_available(path));
            }
            let card = card_in_wave(&ctx, &wave, parts[1]).await?;
            let hook_events = hook_events_for_card(&ctx, &wave, &card.id).await?;
            let hook_events_updated_at = hook_events_updated_at(&card, &hook_events);
            Ok(json!([
                entry_file("meta.json", None, Some(card.updated_at)),
                entry_file("payload.json", None, Some(card.updated_at)),
                entry_file("events.json", None, Some(hook_events_updated_at)),
                entry_file("conversation.md", None, Some(hook_events_updated_at)),
            ]))
        }
        other => Err(path_not_available(other)),
    }
}

async fn wave_cat(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role_any(&identity, &[CardRole::Spec, CardRole::Worker])?;
    let path = parse_path_arg(&args, true)?;
    let (_, wave) = resolve_wave_for_identity(&ctx, &identity).await?;

    match path.as_str() {
        "index.md" => {
            let cards = cards_for_wave(&ctx, &wave).await?;
            Ok(content_markdown(index_markdown(&wave, cards.len())))
        }
        "wave.json" => content_json(&wave),
        "report.md" => {
            let (_, payload) = wave_report::load_report_for_wave(&ctx, &wave).await?;
            Ok(content_markdown(payload.body))
        }
        "cards/index.json" => {
            let cards = cards_for_wave(&ctx, &wave).await?;
            let metas: Vec<Value> = cards.iter().map(|card| card_meta(&ctx, card)).collect();
            content_json(&metas)
        }
        "runs/index.json" => {
            let runs = runs_for_wave(&ctx, &wave).await?;
            let summaries: Vec<Value> = runs.iter().map(run_index_entry).collect();
            content_json(&summaries)
        }
        path if path.starts_with("cards/") => {
            let parts: Vec<&str> = path.split('/').collect();
            if parts.len() != 3 {
                return Err(path_not_available(path));
            }
            let card = card_in_wave(&ctx, &wave, parts[1]).await?;
            match parts[2] {
                "meta.json" => content_json(&card_meta(&ctx, &card)),
                "payload.json" => {
                    let mut card = card;
                    project_runtime_into_card_payload(ctx.repo.as_ref(), &mut card)
                        .await
                        .map_err(|e| {
                            RpcError::internal(format!("wave_file: runtime projection: {e}"))
                        })?;
                    content_json(&card.payload)
                }
                "events.json" => {
                    let hook_events = hook_events_for_card(&ctx, &wave, &card.id).await?;
                    content_json(&hook_events_json(&hook_events))
                }
                "conversation.md" => {
                    let hook_events = hook_events_for_card(&ctx, &wave, &card.id).await?;
                    Ok(content_markdown(conversation_markdown(
                        &card.id,
                        &hook_events,
                    )))
                }
                _ => Err(path_not_available(path)),
            }
        }
        path if path.starts_with("runs/") => {
            let runs = runs_for_wave(&ctx, &wave).await?;
            let run_path = path.trim_start_matches("runs/");
            if let Some(key) = run_path.strip_suffix(".md") {
                let run = run_by_key(&runs, key)?;
                Ok(content_markdown(run_markdown(run)))
            } else if let Some(key) = run_path.strip_suffix(".json") {
                let run = run_by_key(&runs, key)?;
                content_json(&run_json(run))
            } else {
                Err(path_not_available(path))
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
    identity: &ToolCallIdentity,
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

fn cards_updated_at(wave: &Wave, cards: &[Card]) -> i64 {
    cards
        .iter()
        .map(|card| card.updated_at)
        .max()
        .unwrap_or(wave.updated_at)
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

#[derive(Clone, Debug)]
struct HookEventProjection {
    event_id: i64,
    at: i64,
    kind: &'static str,
    hook_kind: String,
    payload: Value,
}

async fn hook_events_for_card(
    ctx: &Arc<AppContext>,
    wave: &Wave,
    card_id: &crate::ids::CardId,
) -> Result<Vec<HookEventProjection>, RpcError> {
    let events = ctx
        .repo
        .events_for_wave(wave.id.as_str(), &["codex.hook", "claude.hook"])
        .await
        .map_err(|e| RpcError::internal(format!("wave_file: events_for_wave: {e}")))?;

    let mut hooks = Vec::new();
    for row in events {
        if row.scope.card_id() != Some(card_id) {
            continue;
        }
        match row.event {
            Event::CodexHook { kind, payload, .. } => hooks.push(HookEventProjection {
                event_id: row.id,
                at: row.at,
                kind: "codex.hook",
                hook_kind: kind,
                payload,
            }),
            Event::ClaudeHook { kind, payload, .. } => hooks.push(HookEventProjection {
                event_id: row.id,
                at: row.at,
                kind: "claude.hook",
                hook_kind: kind,
                payload,
            }),
            _ => {}
        }
    }
    Ok(hooks)
}

fn hook_events_updated_at(card: &Card, events: &[HookEventProjection]) -> i64 {
    events
        .iter()
        .map(|event| event.at)
        .max()
        .unwrap_or(card.updated_at)
}

#[derive(Clone, Debug)]
struct RunEventProjection {
    event_id: i64,
    at: i64,
    kind: &'static str,
    payload: Value,
}

#[derive(Clone, Debug)]
struct RunVerdictProjection {
    status: String,
    reason: Option<String>,
    at: i64,
}

#[derive(Clone, Debug)]
struct RunProjection {
    idempotency_key: String,
    status: &'static str,
    kind: String,
    requested_at: Option<i64>,
    finished_at: Option<i64>,
    worker_card: Option<Card>,
    requested_event: Option<RunEventProjection>,
    completed_event: Option<RunEventProjection>,
    failed_event: Option<RunEventProjection>,
    verdict: Option<RunVerdictProjection>,
    verdict_event: Option<RunEventProjection>,
}

async fn runs_for_wave(ctx: &Arc<AppContext>, wave: &Wave) -> Result<Vec<RunProjection>, RpcError> {
    let mut cards = cards_for_wave(ctx, wave).await?;
    project_runtime_into_cards_payload(ctx.repo.as_ref(), &mut cards)
        .await
        .map_err(|e| RpcError::internal(format!("wave_file: runtime projection: {e}")))?;
    let events = ctx
        .repo
        .events_for_wave(
            wave.id.as_str(),
            &[
                "codex.job_requested",
                "terminal.job_requested",
                "task.completed",
                "task.failed",
            ],
        )
        .await
        .map_err(|e| RpcError::internal(format!("wave_file: events_for_wave: {e}")))?;

    let runs = project_runs(&ctx.write, cards, events);
    for run in &runs {
        if RESERVED_RUN_KEYS.contains(&run.idempotency_key.as_str()) {
            tracing::error!(
                target: "wave_file",
                idempotency_key = %run.idempotency_key,
                wave_id = %wave.id,
                "runs projection: idempotency_key collides with reserved path `runs/<key>.json`"
            );
            return Err(RpcError::internal(format!(
                "runs projection unavailable: idempotency_key `{}` collides with reserved path. \
                 Remediation: stop submitting jobs with this key, or update RESERVED_RUN_KEYS.",
                run.idempotency_key
            )));
        }
    }
    Ok(runs)
}

fn project_runs(
    write: &WriteContext,
    cards: Vec<Card>,
    events: Vec<WaveEvent>,
) -> Vec<RunProjection> {
    let mut keys = BTreeSet::new();
    let mut worker_cards = BTreeMap::new();
    for card in cards {
        if write.verify_role(&card.id) != Some(CardRole::Worker) {
            continue;
        }
        if let Some(key) = idempotency_key_from_payload(&card.payload) {
            keys.insert(key.to_string());
            worker_cards.entry(key.to_string()).or_insert(card);
        }
    }

    let mut requested = BTreeMap::<String, RunEventProjection>::new();
    let mut requested_kind = BTreeMap::<String, &'static str>::new();
    let mut completed = BTreeMap::<String, RunEventProjection>::new();
    let mut failed = BTreeMap::<String, RunEventProjection>::new();
    let mut verdict = BTreeMap::<String, RunEventProjection>::new();

    for row in events {
        match &row.event {
            Event::CodexJobRequested {
                idempotency_key, ..
            } => {
                keys.insert(idempotency_key.clone());
                requested_kind.insert(idempotency_key.clone(), "codex");
                record_earliest(
                    &mut requested,
                    idempotency_key,
                    run_event(
                        row.id,
                        row.at,
                        "codex.job_requested",
                        row.event.payload_value(),
                    ),
                );
            }
            Event::TerminalJobRequested {
                idempotency_key, ..
            } => {
                keys.insert(idempotency_key.clone());
                requested_kind.insert(idempotency_key.clone(), "terminal");
                record_earliest(
                    &mut requested,
                    idempotency_key,
                    run_event(
                        row.id,
                        row.at,
                        "terminal.job_requested",
                        row.event.payload_value(),
                    ),
                );
            }
            Event::TaskCompleted {
                idempotency_key, ..
            } => {
                let event = run_event(row.id, row.at, "task.completed", row.event.payload_value());
                if is_spec_verdict_event(&row.scope, &row.actor) {
                    record_latest(&mut verdict, idempotency_key, event);
                } else {
                    // Wave-scoped verdicts are routed to `verdict`, not `completed`.
                    // The remaining competition here is between worker self-reports
                    // for the same run, such as a dispatcher retry after spawn
                    // failure, so the latest completion is the most informative one.
                    record_latest(&mut completed, idempotency_key, event);
                }
            }
            Event::TaskFailed {
                idempotency_key, ..
            } => {
                let event = run_event(row.id, row.at, "task.failed", row.event.payload_value());
                if is_spec_verdict_event(&row.scope, &row.actor) {
                    record_latest(&mut verdict, idempotency_key, event);
                } else {
                    record_latest(&mut failed, idempotency_key, event);
                }
            }
            _ => {}
        }
    }

    keys.into_iter()
        .map(|key| {
            let worker_card = worker_cards.remove(&key);
            let requested_event = requested.remove(&key);
            let completed_event = completed.remove(&key);
            let failed_event = failed.remove(&key);
            let verdict_event = verdict.remove(&key);
            let verdict = verdict_event.as_ref().and_then(verdict_from_event);

            let final_event = match (failed_event.as_ref(), completed_event.as_ref()) {
                (Some(failed), Some(completed)) if completed.event_id > failed.event_id => {
                    Some(("completed", completed))
                }
                (Some(failed), _) => Some(("failed", failed)),
                (None, Some(completed)) => Some(("completed", completed)),
                (None, None) => None,
            };

            let (status, finished_at) = match (requested_event.as_ref(), final_event) {
                (Some(_), Some((kind, event))) => (kind, Some(event.at)),
                (Some(_), None) if worker_card.is_some() => ("running", None),
                (Some(_), None) => ("requested", None),
                (None, _) => ("unknown", None),
            };

            let kind = worker_card
                .as_ref()
                .and_then(|card| run_kind_from_card(card))
                .or_else(|| requested_kind.get(&key).copied())
                .unwrap_or("unknown")
                .to_string();

            RunProjection {
                idempotency_key: key,
                status,
                kind,
                requested_at: requested_event.as_ref().map(|event| event.at),
                finished_at,
                worker_card,
                requested_event,
                completed_event,
                failed_event,
                verdict,
                verdict_event,
            }
        })
        .collect()
}

fn run_event(event_id: i64, at: i64, kind: &'static str, payload: Value) -> RunEventProjection {
    RunEventProjection {
        event_id,
        at,
        kind,
        payload,
    }
}

fn record_earliest(
    map: &mut BTreeMap<String, RunEventProjection>,
    key: &str,
    event: RunEventProjection,
) {
    match map.get(key) {
        Some(existing) if existing.event_id <= event.event_id => {}
        _ => {
            map.insert(key.to_string(), event);
        }
    }
}

fn record_latest(
    map: &mut BTreeMap<String, RunEventProjection>,
    key: &str,
    event: RunEventProjection,
) {
    match map.get(key) {
        Some(existing) if existing.event_id >= event.event_id => {}
        _ => {
            map.insert(key.to_string(), event);
        }
    }
}

fn latest_final_event<'a>(
    completed: Option<&'a RunEventProjection>,
    failed: Option<&'a RunEventProjection>,
) -> Option<&'a RunEventProjection> {
    match (completed, failed) {
        (Some(done), Some(fail)) if done.event_id > fail.event_id => Some(done),
        (Some(_), Some(fail)) => Some(fail),
        (Some(done), None) => Some(done),
        (None, Some(fail)) => Some(fail),
        (None, None) => None,
    }
}

/// Spec verdicts are task terminal events emitted at Wave scope by the
/// `update_task_meta` MCP tool in `wave_state.rs`, where
/// `identity.to_actor_id()` produces the spec actor. Non-verdict task events
/// may also be Wave-scoped: the dispatcher spawn-failure path in
/// `dispatcher.rs` emits `Event::TaskFailed` as `ActorId::KernelDispatcher`
/// while preserving the request scope. Those dispatcher failures remain run
/// failures, not verdicts, even though they share the Wave scope.
fn is_spec_verdict_event(scope: &EventScope, actor: &ActorId) -> bool {
    matches!(scope, EventScope::Wave { .. }) && !matches!(actor, ActorId::KernelDispatcher)
}

fn verdict_from_event(event: &RunEventProjection) -> Option<RunVerdictProjection> {
    let (status, reason) = match event.kind {
        "task.completed" => {
            let result = event.payload.get("result")?;
            let status = result.get("status")?.as_str()?;
            (
                status,
                result
                    .get("reason")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            )
        }
        "task.failed" => (
            "rejected",
            event
                .payload
                .get("reason")
                .and_then(Value::as_str)
                .map(str::to_string),
        ),
        _ => return None,
    };
    Some(RunVerdictProjection {
        status: status.to_string(),
        reason,
        at: event.at,
    })
}

fn idempotency_key_from_payload(payload: &Value) -> Option<&str> {
    payload.get("idempotency_key").and_then(Value::as_str)
}

fn run_kind_from_card(card: &Card) -> Option<&'static str> {
    match card.kind.as_str() {
        "codex" => Some("codex"),
        "terminal" => Some("terminal"),
        _ => card
            .payload
            .get("role_request")
            .and_then(Value::as_str)
            .and_then(|kind| match kind {
                "codex" => Some("codex"),
                "terminal" => Some("terminal"),
                _ => None,
            }),
    }
}

fn runs_updated_at(wave: &Wave, runs: &[RunProjection]) -> Option<i64> {
    Some(
        runs.iter()
            .filter_map(run_listing_updated_at)
            .max()
            .unwrap_or(wave.updated_at),
    )
}

fn run_listing_updated_at(run: &RunProjection) -> Option<i64> {
    [
        run.requested_at,
        run.finished_at,
        run.verdict.as_ref().map(|verdict| verdict.at),
        run.worker_card.as_ref().map(|card| card.updated_at),
    ]
    .into_iter()
    .flatten()
    .max()
}

fn run_by_key<'a>(runs: &'a [RunProjection], key: &str) -> Result<&'a RunProjection, RpcError> {
    runs.iter()
        .find(|run| run.idempotency_key == key)
        .ok_or_else(|| path_not_available(&format!("runs/{key}")))
}

fn card_meta(ctx: &Arc<AppContext>, card: &Card) -> Value {
    let role = ctx.write.verify_role(&card.id).unwrap_or_default();
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

fn run_listing_entry(run: &RunProjection) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert(
        "name".into(),
        Value::String(format!("{}.md", run.idempotency_key)),
    );
    obj.insert("kind".into(), Value::String("file".into()));
    obj.insert(
        "idempotency_key".into(),
        Value::String(run.idempotency_key.clone()),
    );
    obj.insert("status".into(), Value::String(run.status.into()));
    obj.insert("run_kind".into(), Value::String(run.kind.clone()));
    obj.insert("verdict".into(), run_verdict_index_json(run));
    obj.insert("requested_at".into(), option_i64(run.requested_at));
    obj.insert("finished_at".into(), option_i64(run.finished_at));
    obj.insert(
        "worker_card_id".into(),
        run.worker_card
            .as_ref()
            .map(|card| Value::String(card.id.as_str().to_string()))
            .unwrap_or(Value::Null),
    );
    if let Some(updated_at) = run_listing_updated_at(run) {
        obj.insert("updated_at".into(), json!(updated_at));
    }
    Value::Object(obj)
}

fn run_index_entry(run: &RunProjection) -> Value {
    json!({
        "idempotency_key": run.idempotency_key,
        "status": run.status,
        "kind": run.kind,
        "verdict": run_verdict_index_json(run),
        "requested_at": run.requested_at,
        "finished_at": run.finished_at,
        "worker_card_id": run.worker_card.as_ref().map(|card| card.id.as_str()),
    })
}

fn run_json(run: &RunProjection) -> Value {
    json!({
        "idempotency_key": run.idempotency_key,
        "status": run.status,
        "kind": run.kind,
        "verdict": run_verdict_full_json(run),
        "requested_at": run.requested_at,
        "finished_at": run.finished_at,
        "worker_card_id": run.worker_card.as_ref().map(|card| card.id.as_str()),
        "worker_card_payload": run.worker_card.as_ref().map(|card| card.payload.clone()),
        "events": {
            "requested": run.requested_event.as_ref().map(event_json),
            "completed": run.completed_event.as_ref().map(event_json),
            "failed": run.failed_event.as_ref().map(event_json),
            "verdict": run.verdict_event.as_ref().map(event_json),
        },
    })
}

fn run_verdict_index_json(run: &RunProjection) -> Value {
    run.verdict
        .as_ref()
        .map(|verdict| {
            json!({
                "status": verdict.status,
                "at": verdict.at,
            })
        })
        .unwrap_or(Value::Null)
}

fn run_verdict_full_json(run: &RunProjection) -> Value {
    run.verdict
        .as_ref()
        .map(|verdict| {
            json!({
                "status": verdict.status,
                "reason": verdict.reason,
                "at": verdict.at,
            })
        })
        .unwrap_or(Value::Null)
}

fn event_json(event: &RunEventProjection) -> Value {
    json!({
        "event_id": event.event_id,
        "kind": event.kind,
        "created_at": event.at,
        "payload": event.payload,
    })
}

fn hook_events_json(events: &[HookEventProjection]) -> Vec<Value> {
    events
        .iter()
        .map(|event| {
            json!({
                "event_id": event.event_id,
                "kind": event.kind,
                "hook_kind": event.hook_kind,
                "created_at": event.at,
                "payload": event.payload,
            })
        })
        .collect()
}

fn option_i64(value: Option<i64>) -> Value {
    value.map(Value::from).unwrap_or(Value::Null)
}

fn conversation_markdown(card_id: &crate::ids::CardId, events: &[HookEventProjection]) -> String {
    let mut out = String::new();
    out.push_str("> READ-ONLY PROJECTION: derived from persisted wave hook events. This is not the source of truth.\n\n");
    out.push_str(&format!("# Conversation — card {}\n\n", card_id.as_str()));

    if events.is_empty() {
        out.push_str("_No hook events recorded._\n");
        return out;
    }

    for event in events {
        if hook_event_is(event, "user_prompt_submit", "UserPromptSubmit") {
            if let Some(prompt) = event.payload.get("prompt").and_then(Value::as_str) {
                out.push_str("## User\n\n");
                out.push_str(prompt);
                out.push_str("\n\n");
            }
        } else if hook_event_is(event, "stop", "Stop") {
            if let Some(message) = event
                .payload
                .get("last_assistant_message")
                .and_then(Value::as_str)
            {
                out.push_str("## Assistant\n\n");
                out.push_str(message);
                out.push_str("\n\n");
            }
        } else if hook_event_is_tool_use(event)
            && let Some(tool_name) = event.payload.get("tool_name").and_then(Value::as_str)
        {
            out.push_str(&format!("- tool: {tool_name}\n\n"));
        }
    }
    out
}

fn hook_event_is(event: &HookEventProjection, snake_suffix: &str, pascal_name: &str) -> bool {
    event
        .hook_kind
        .rsplit('.')
        .next()
        .is_some_and(|segment| segment.eq_ignore_ascii_case(snake_suffix))
        || event
            .payload
            .get("hook_event_name")
            .and_then(Value::as_str)
            .is_some_and(|name| {
                normalize_hook_event_name(name) == normalize_hook_event_name(pascal_name)
            })
}

fn hook_event_is_tool_use(event: &HookEventProjection) -> bool {
    let hook_kind = event.hook_kind.to_ascii_lowercase();
    if hook_kind.contains("tool_use") {
        return true;
    }
    event
        .payload
        .get("hook_event_name")
        .and_then(Value::as_str)
        .is_some_and(|name| normalize_hook_event_name(name).contains("tooluse"))
}

fn normalize_hook_event_name(name: &str) -> String {
    name.chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn run_markdown(run: &RunProjection) -> String {
    let mut out = String::new();
    out.push_str("> READ-ONLY PROJECTION: derived from wave events and worker card payloads. This is not the source of truth.\n\n");
    out.push_str(&format!("# Run `{}`\n\n", run.idempotency_key));
    out.push_str(&format!("- Status: {}\n", run.status));
    out.push_str(&format!("- Kind: {}\n", run.kind));
    out.push_str(&format!(
        "- Worker card: {}\n",
        run.worker_card
            .as_ref()
            .map(|card| format!(
                "[{}](../cards/{}/payload.json)",
                card.id.as_str(),
                card.id.as_str()
            ))
            .unwrap_or_else(|| "not materialized".into())
    ));
    out.push_str(&format!(
        "- Requested at: {}\n",
        format_optional_i64(run.requested_at)
    ));
    out.push_str(&format!(
        "- Finished at: {}\n",
        format_optional_i64(run.finished_at)
    ));

    if let Some(verdict) = run.verdict.as_ref() {
        let reason = verdict.reason.as_deref().unwrap_or("");
        out.push_str(&format!(
            "\n## Verdict\n\nVerdict: {} by spec at {}: {}\n",
            verdict.status, verdict.at, reason
        ));
    }

    if let Some(card) = run.worker_card.as_ref() {
        append_payload_field(&mut out, &card.payload, "goal", "Goal");
        append_payload_json_field(&mut out, &card.payload, "context", "Context");
        append_payload_field(
            &mut out,
            &card.payload,
            "acceptance_criteria",
            "Acceptance Criteria",
        );
        append_payload_field(&mut out, &card.payload, "prompt", "Prompt");
    }

    out.push_str("\n## Final Event\n\n");
    match latest_final_event(run.completed_event.as_ref(), run.failed_event.as_ref()) {
        Some(event) if event.kind == "task.failed" => {
            let reason = event
                .payload
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("unknown failure");
            out.push_str(&format!("- TaskFailed: {}\n", reason));
        }
        Some(event) => {
            out.push_str("- TaskCompleted:\n\n");
            out.push_str("```json\n");
            out.push_str(&final_result_summary(event));
            out.push_str("\n```\n");
        }
        None => out.push_str("- No TaskCompleted or TaskFailed event has been recorded.\n"),
    }
    out
}

fn append_payload_field(out: &mut String, payload: &Value, key: &str, label: &str) {
    if let Some(value) = payload.get(key).and_then(Value::as_str) {
        out.push_str(&format!("\n## {label}\n\n{value}\n"));
    }
}

fn append_payload_json_field(out: &mut String, payload: &Value, key: &str, label: &str) {
    if let Some(value) = payload.get(key) {
        out.push_str(&format!("\n## {label}\n\n```json\n"));
        out.push_str(&pretty_json(value));
        out.push_str("\n```\n");
    }
}

fn final_result_summary(event: &RunEventProjection) -> String {
    let result = event.payload.get("result").unwrap_or(&Value::Null);
    if let Some(summary) = result.get("summary").and_then(Value::as_str) {
        return summary.to_string();
    }
    if let Some(summary) = result.as_str() {
        return summary.to_string();
    }
    pretty_json(result)
}

fn pretty_json(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| "null".into())
}

fn format_optional_i64(value: Option<i64>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "null".into())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptors_document_spec_worker_access() {
        let ls = ls_descriptor();
        let cat = cat_descriptor();

        assert!(
            ls.description.starts_with("Spec/Worker:"),
            "ls descriptor should advertise Spec/Worker access: {}",
            ls.description
        );
        assert!(
            cat.description.starts_with("Spec/Worker:"),
            "cat descriptor should advertise Spec/Worker access: {}",
            cat.description
        );
        assert!(
            !ls.description.contains("Spec-only") && !cat.description.contains("Spec-only"),
            "wave file descriptors must not claim spec-only access"
        );
    }
}
