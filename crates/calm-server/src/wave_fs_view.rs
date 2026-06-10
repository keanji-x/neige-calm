//! Shared read-only file views for a wave.
//!
//! This module owns the path projection used by both the MCP
//! `calm.wave.{ls,cat}` tools and the authenticated HTTP wave file
//! endpoints. Callers are responsible for their own entry gates:
//! MCP resolves the wave from the bound card identity, while HTTP
//! resolves it from the route path and session middleware.

use crate::db::{RouteRepo, WaveEvent};
use crate::error::CalmError;
use crate::event::{Event, EventScope};
use crate::ids::{ActorId, CardId};
use crate::model::{Card, CardRole, Wave};
use crate::runtime_lookup::{
    project_runtime_into_card_payload, project_runtime_into_cards_payload,
};
use crate::state::WriteContext;
use crate::wave_report::WaveReportPayload;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use utoipa::ToSchema;

pub(crate) const RESERVED_RUN_KEYS: &[&str] = &["index"];

#[derive(Clone)]
pub struct WaveFsView<'a> {
    repo: &'a dyn RouteRepo,
    write: &'a WriteContext,
}

impl<'a> WaveFsView<'a> {
    pub fn new(repo: &'a dyn RouteRepo, write: &'a WriteContext) -> Self {
        Self { repo, write }
    }

    pub async fn ls(
        &self,
        wave: &Wave,
        path: Option<&str>,
    ) -> Result<Vec<WaveFsEntry>, WaveFsError> {
        let path = path.map(normalize_path).unwrap_or_default();
        match path.as_str() {
            "" => {
                let cards = self.cards_for_wave(wave).await?;
                let runs = self.runs_for_wave(wave).await?;
                Ok(vec![
                    entry_file("index.md", None, Some(wave.updated_at)),
                    entry_file("wave.json", None, Some(wave.updated_at)),
                    entry_file("report.md", None, Some(wave.updated_at)),
                    entry_dir("cards/", Some(cards.len()), None),
                    entry_dir(
                        "runs/",
                        Some(runs.len().saturating_mul(2).saturating_add(1)),
                        runs_updated_at(wave, &runs),
                    ),
                ])
            }
            "cards" => {
                let cards = self.cards_for_wave(wave).await?;
                let cards_updated_at = cards_updated_at(wave, &cards);
                let mut entries = Vec::with_capacity(cards.len() + 1);
                entries.push(entry_file("index.json", None, Some(cards_updated_at)));
                entries.extend(cards.iter().map(|card| {
                    entry_dir(
                        &format!("{}/", card.id.as_str()),
                        None,
                        Some(card.updated_at),
                    )
                }));
                Ok(entries)
            }
            "runs" => {
                let runs = self.runs_for_wave(wave).await?;
                let mut entries =
                    Vec::with_capacity(runs.len().saturating_mul(2).saturating_add(1));
                entries.push(entry_file("index.json", None, runs_updated_at(wave, &runs)));
                for run in &runs {
                    entries.push(run_listing_entry(run, "md"));
                    entries.push(run_listing_entry(run, "json"));
                }
                Ok(entries)
            }
            path if path.starts_with("cards/") => {
                let parts: Vec<&str> = path.split('/').collect();
                if parts.len() != 2 {
                    return Err(path_not_available(path));
                }
                let card = self.card_in_wave(wave, parts[1]).await?;
                let hook_events = self.hook_events_for_card(wave, &card.id).await?;
                let hook_events_updated_at = hook_events_updated_at(&card, &hook_events);
                let runtime = self
                    .repo
                    .runtime_get_projectable_for_card(&card.id.to_string())
                    .await
                    .map_err(|e| {
                        WaveFsError::Internal(format!("wave_file: runtime lookup: {e}"))
                    })?;
                let runtime_updated_at = runtime
                    .as_ref()
                    .map(|runtime| runtime.updated_at_ms)
                    .unwrap_or(card.updated_at);
                Ok(vec![
                    entry_file("meta.json", None, Some(card.updated_at)),
                    entry_file("payload.json", None, Some(card.updated_at)),
                    entry_file("runtime.json", None, Some(runtime_updated_at)),
                    entry_file("events.json", None, Some(hook_events_updated_at)),
                    entry_file("conversation.md", None, Some(hook_events_updated_at)),
                ])
            }
            other => Err(path_not_available(other)),
        }
    }

    pub async fn cat(&self, wave: &Wave, path: &str) -> Result<WaveFsContent, WaveFsError> {
        let path = normalize_path(path);
        match path.as_str() {
            "index.md" => {
                let cards = self.cards_for_wave(wave).await?;
                Ok(content_markdown(index_markdown(wave, cards.len())))
            }
            "wave.json" => content_json(wave),
            "report.md" => {
                let payload = self.load_report_for_wave(wave).await?;
                Ok(content_markdown(payload.body))
            }
            "cards/index.json" => {
                let cards = self.cards_for_wave(wave).await?;
                let metas: Vec<Value> = cards.iter().map(|card| self.card_meta(card)).collect();
                content_json(&metas)
            }
            "runs/index.json" => {
                let runs = self.runs_for_wave(wave).await?;
                let summaries: Vec<Value> = runs.iter().map(run_index_entry).collect();
                content_json(&summaries)
            }
            path if path.starts_with("cards/") => {
                let parts: Vec<&str> = path.split('/').collect();
                if parts.len() != 3 {
                    return Err(path_not_available(path));
                }
                let card = self.card_in_wave(wave, parts[1]).await?;
                match parts[2] {
                    "meta.json" => content_json(&self.card_meta(&card)),
                    "payload.json" => {
                        let mut card = card;
                        project_runtime_into_card_payload(self.repo, &mut card)
                            .await
                            .map_err(|e| {
                                WaveFsError::Internal(format!("wave_file: runtime projection: {e}"))
                            })?;
                        content_json(&card.payload)
                    }
                    "runtime.json" => {
                        let mut card = card;
                        project_runtime_into_card_payload(self.repo, &mut card)
                            .await
                            .map_err(|e| {
                                WaveFsError::Internal(format!("wave_file: runtime projection: {e}"))
                            })?;
                        match &card.runtime {
                            Some(runtime) => content_json(runtime),
                            None => content_json(&Value::Null),
                        }
                    }
                    "events.json" => {
                        let hook_events = self.hook_events_for_card(wave, &card.id).await?;
                        content_json(&hook_events_json(&hook_events))
                    }
                    "conversation.md" => {
                        let hook_events = self.hook_events_for_card(wave, &card.id).await?;
                        Ok(content_markdown(conversation_markdown(
                            &card.id,
                            &hook_events,
                        )))
                    }
                    _ => Err(path_not_available(path)),
                }
            }
            path if path.starts_with("runs/") => {
                let runs = self.runs_for_wave(wave).await?;
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

    async fn cards_for_wave(&self, wave: &Wave) -> Result<Vec<Card>, WaveFsError> {
        self.repo
            .cards_by_wave(wave.id.as_str())
            .await
            .map_err(|e| WaveFsError::Internal(format!("wave_file: cards_by_wave: {e}")))
    }

    async fn card_in_wave(&self, wave: &Wave, card_id: &str) -> Result<Card, WaveFsError> {
        let card = self
            .repo
            .card_get(card_id)
            .await
            .map_err(|e| WaveFsError::Internal(format!("wave_file: card lookup: {e}")))?
            .ok_or_else(|| path_not_available(&format!("cards/{card_id}")))?;
        if card.wave_id != wave.id {
            return Err(WaveFsError::Forbidden(format!(
                "wave_file: forbidden: card {} is not in the caller's bound wave {}",
                card.id.as_str(),
                wave.id.as_str()
            )));
        }
        Ok(card)
    }

    async fn hook_events_for_card(
        &self,
        wave: &Wave,
        card_id: &CardId,
    ) -> Result<Vec<HookEventProjection>, WaveFsError> {
        let events = self
            .repo
            .events_for_wave(wave.id.as_str(), &["codex.hook", "claude.hook"], None)
            .await
            .map_err(|e| WaveFsError::Internal(format!("wave_file: events_for_wave: {e}")))?;

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

    async fn runs_for_wave(&self, wave: &Wave) -> Result<Vec<RunProjection>, WaveFsError> {
        let mut cards = self.cards_for_wave(wave).await?;
        project_runtime_into_cards_payload(self.repo, &mut cards)
            .await
            .map_err(|e| WaveFsError::Internal(format!("wave_file: runtime projection: {e}")))?;
        let events = self
            .repo
            .events_for_wave(
                wave.id.as_str(),
                &[
                    "codex.worker_requested",
                    "terminal.worker_requested",
                    "task.completed",
                    "task.failed",
                ],
                None,
            )
            .await
            .map_err(|e| WaveFsError::Internal(format!("wave_file: events_for_wave: {e}")))?;

        let runs = project_runs(self.write, cards, events);
        for run in &runs {
            if is_reserved_run_key(&run.idempotency_key) {
                tracing::error!(
                    target: "wave_file",
                    idempotency_key = %run.idempotency_key,
                    wave_id = %wave.id,
                    "runs projection: idempotency_key collides with reserved path `runs/<key>.json`"
                );
                return Err(WaveFsError::Internal(format!(
                    "runs projection unavailable: idempotency_key `{}` collides with reserved path. \
                     Remediation: stop submitting jobs with this key, or update RESERVED_RUN_KEYS.",
                    run.idempotency_key
                )));
            }
        }
        Ok(runs)
    }

    async fn load_report_for_wave(&self, wave: &Wave) -> Result<WaveReportPayload, WaveFsError> {
        let cards = self
            .repo
            .cards_by_wave(wave.id.as_str())
            .await
            .map_err(|e| WaveFsError::Internal(format!("wave_report: cards_by_wave: {e}")))?;
        let report_card = cards
            .into_iter()
            .find(|c| c.kind == "wave-report")
            .ok_or_else(|| {
                WaveFsError::Internal(format!(
                    "wave_report: wave {} has no wave-report card (invariant violation)",
                    wave.id.as_str()
                ))
            })?;
        serde_json::from_value(report_card.payload.clone()).map_err(|e| {
            WaveFsError::Internal(format!(
                "wave_report: malformed payload on card {}: {e}",
                report_card.id.as_str()
            ))
        })
    }

    fn card_meta(&self, card: &Card) -> Value {
        let role = self.write.verify_role(&card.id).unwrap_or_default();
        card_meta_value(card, serde_json::to_value(role).unwrap_or(Value::Null))
    }
}

pub(crate) fn card_meta_value(card: &Card, role: Value) -> Value {
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct WaveFsEntry {
    pub name: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,
    #[serde(default, flatten, skip_serializing_if = "serde_json::Map::is_empty")]
    #[schema(ignore)]
    pub extra: serde_json::Map<String, Value>,
}

impl WaveFsEntry {
    fn new(name: impl Into<String>, kind: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            kind: kind.into(),
            size: None,
            updated_at: None,
            extra: serde_json::Map::new(),
        }
    }

    fn with_size(mut self, size: Option<usize>) -> Self {
        self.size = size;
        self
    }

    fn with_updated_at(mut self, updated_at: Option<i64>) -> Self {
        self.updated_at = updated_at;
        self
    }

    fn with_extra(mut self, key: &str, value: Value) -> Self {
        self.extra.insert(key.to_string(), value);
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct WaveFsContent {
    pub content: String,
    pub content_type: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WaveFsError {
    PathNotAvailable(String),
    Forbidden(String),
    Internal(String),
}

impl From<WaveFsError> for CalmError {
    fn from(value: WaveFsError) -> Self {
        match value {
            WaveFsError::PathNotAvailable(message) => CalmError::BadRequest(message),
            WaveFsError::Forbidden(message) => CalmError::Forbidden(message),
            WaveFsError::Internal(message) => CalmError::Internal(message),
        }
    }
}

pub fn normalize_path(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed == "/" {
        return String::new();
    }
    trimmed
        .trim_start_matches('/')
        .trim_end_matches('/')
        .to_string()
}

fn cards_updated_at(wave: &Wave, cards: &[Card]) -> i64 {
    cards
        .iter()
        .map(|card| card.updated_at)
        .max()
        .unwrap_or(wave.updated_at)
}

#[derive(Clone, Debug)]
pub(crate) struct HookEventProjection {
    pub(crate) event_id: i64,
    pub(crate) at: i64,
    pub(crate) kind: &'static str,
    pub(crate) hook_kind: String,
    pub(crate) payload: Value,
}

fn hook_events_updated_at(card: &Card, events: &[HookEventProjection]) -> i64 {
    events
        .iter()
        .map(|event| event.at)
        .max()
        .unwrap_or(card.updated_at)
}

#[derive(Clone, Debug)]
pub(crate) struct RunEventProjection {
    pub(crate) event_id: i64,
    pub(crate) at: i64,
    pub(crate) kind: &'static str,
    pub(crate) payload: Value,
}

#[derive(Clone, Debug)]
pub(crate) struct RunVerdictProjection {
    pub(crate) status: String,
    pub(crate) reason: Option<String>,
    pub(crate) at: i64,
}

#[derive(Clone, Debug)]
pub(crate) struct RunProjection {
    pub(crate) idempotency_key: String,
    pub(crate) status: &'static str,
    pub(crate) kind: String,
    pub(crate) requested_at: Option<i64>,
    pub(crate) finished_at: Option<i64>,
    pub(crate) worker_card: Option<Card>,
    pub(crate) requested_event: Option<RunEventProjection>,
    pub(crate) completed_event: Option<RunEventProjection>,
    pub(crate) failed_event: Option<RunEventProjection>,
    pub(crate) verdict: Option<RunVerdictProjection>,
    pub(crate) verdict_event: Option<RunEventProjection>,
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
            Event::CodexWorkerRequested {
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
                        "codex.worker_requested",
                        row.event.payload_value(),
                    ),
                );
            }
            Event::TerminalWorkerRequested {
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
                        "terminal.worker_requested",
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
                .and_then(run_kind_from_card)
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

fn run_by_key<'a>(runs: &'a [RunProjection], key: &str) -> Result<&'a RunProjection, WaveFsError> {
    runs.iter()
        .find(|run| run.idempotency_key == key)
        .ok_or_else(|| path_not_available(&format!("runs/{key}")))
}

fn run_listing_entry(run: &RunProjection, extension: &str) -> WaveFsEntry {
    let mut entry = WaveFsEntry::new(format!("{}.{}", run.idempotency_key, extension), "file")
        .with_extra(
            "idempotency_key",
            Value::String(run.idempotency_key.clone()),
        )
        .with_extra("status", Value::String(run.status.into()))
        .with_extra("run_kind", Value::String(run.kind.clone()))
        .with_extra("verdict", run_verdict_index_json(run))
        .with_extra("requested_at", option_i64(run.requested_at))
        .with_extra("finished_at", option_i64(run.finished_at))
        .with_extra(
            "worker_card_id",
            run.worker_card
                .as_ref()
                .map(|card| Value::String(card.id.as_str().to_string()))
                .unwrap_or(Value::Null),
        );
    if let Some(updated_at) = run_listing_updated_at(run) {
        entry.updated_at = Some(updated_at);
    }
    entry
}

pub(crate) fn run_index_entry(run: &RunProjection) -> Value {
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

pub(crate) fn run_json(run: &RunProjection) -> Value {
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

pub(crate) fn hook_events_json(events: &[HookEventProjection]) -> Vec<Value> {
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

pub(crate) fn conversation_markdown(card_id: &CardId, events: &[HookEventProjection]) -> String {
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

pub(crate) fn run_markdown(run: &RunProjection) -> String {
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

pub(crate) fn index_markdown(wave: &Wave, card_count: usize) -> String {
    format!(
        "# Wave {}\n\n- Title: {}\n- Cards: {}\n- Report: [report.md](report.md)\n",
        wave.id.as_str(),
        wave.title,
        card_count
    )
}

pub(crate) fn content_markdown(content: String) -> WaveFsContent {
    WaveFsContent {
        content,
        content_type: "text/markdown".into(),
    }
}

pub(crate) fn content_json<T: Serialize>(value: &T) -> Result<WaveFsContent, WaveFsError> {
    let content = serde_json::to_string_pretty(value)
        .map_err(|e| WaveFsError::Internal(format!("wave_file: json serialization: {e}")))?;
    Ok(WaveFsContent {
        content,
        content_type: "application/json".into(),
    })
}

pub(crate) fn is_reserved_run_key(key: &str) -> bool {
    RESERVED_RUN_KEYS.contains(&key)
}

fn entry_dir(name: &str, size: Option<usize>, updated_at: Option<i64>) -> WaveFsEntry {
    entry(name, "dir", size, updated_at)
}

fn entry_file(name: &str, size: Option<usize>, updated_at: Option<i64>) -> WaveFsEntry {
    entry(name, "file", size, updated_at)
}

fn entry(name: &str, kind: &str, size: Option<usize>, updated_at: Option<i64>) -> WaveFsEntry {
    WaveFsEntry::new(name, kind)
        .with_size(size)
        .with_updated_at(updated_at)
}

fn path_not_available(path: &str) -> WaveFsError {
    WaveFsError::PathNotAvailable(format!(
        "calm.wave: path not available in this view: {}",
        if path.is_empty() { "/" } else { path }
    ))
}
