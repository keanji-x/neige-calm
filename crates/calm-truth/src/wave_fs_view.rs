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
use crate::model::{Card, CardRole, CardRuntimeView, Wave};
use crate::session_projection_lookup::runtime_view_from_runtime;
use crate::state::WriteContext;
use crate::wave_fs_dto::{
    WaveFsCardMeta, WaveFsHookEvent, WaveFsRunDetail, WaveFsRunEventRef, WaveFsRunEvents,
    WaveFsRunIndexEntry, WaveFsRunStatus, WaveFsRunVerdict, WaveFsRunVerdictSummary,
};
use crate::wave_report::WaveReportPayload;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use utoipa::ToSchema;

pub(crate) const RESERVED_RUN_KEYS: &[&str] = &["index"];
pub(crate) const HOOK_EVENT_TRANSCRIPT_CAP: usize = 500;

#[derive(Clone)]
pub struct WaveFsView<'a> {
    repo: &'a dyn RouteRepo,
    write: &'a WriteContext,
    /// Issue #644 PR-C (§6.5) — `plan/<key>/gate.log` access:
    /// `(caller role, gate-logs dir)`. `None` (the default) keeps the
    /// path unavailable — surfaces that don't carry a card identity
    /// (HTTP wave routes) never expose gate logs. Even when wired, only
    /// `CardRole::Spec` passes (§6.7: workers must not read gate
    /// material).
    gate_log_access: Option<(CardRole, std::path::PathBuf)>,
}

impl<'a> WaveFsView<'a> {
    pub fn new(repo: &'a dyn RouteRepo, write: &'a WriteContext) -> Self {
        Self {
            repo,
            write,
            gate_log_access: None,
        }
    }

    /// Enable the `plan/<key>/gate.log` view for a caller with the
    /// given card role (issue #644 PR-C). The role gate itself is
    /// enforced at read time so a worker gets a Forbidden, not a 404.
    pub fn with_gate_log_access(
        mut self,
        role: CardRole,
        gate_logs_dir: std::path::PathBuf,
    ) -> Self {
        self.gate_log_access = Some((role, gate_logs_dir));
        self
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
                    .session_projection_projectable_for_card(&card.id.to_string())
                    .await
                    .map_err(|e| {
                        WaveFsError::Internal(format!("wave_file: runtime lookup: {e}"))
                    })?;
                let runtime_updated_at = runtime
                    .as_ref()
                    .map(|runtime| runtime.updated_at_ms)
                    .unwrap_or(card.updated_at);
                Ok(vec![
                    entry_file(".meta.json", None, Some(card.updated_at)),
                    entry_file(".payload.json", None, Some(card.updated_at)),
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
                let metas: Vec<_> = cards.iter().map(|card| self.card_meta(card)).collect();
                content_json(&metas)
            }
            "runs/index.json" => {
                let runs = self.runs_for_wave(wave).await?;
                let summaries: Vec<_> = runs.iter().map(run_index_entry).collect();
                content_json(&summaries)
            }
            path if path.starts_with("cards/") => {
                let parts: Vec<&str> = path.split('/').collect();
                if parts.len() != 3 {
                    return Err(path_not_available(path));
                }
                let card = self.card_in_wave(wave, parts[1]).await?;
                match parts[2] {
                    ".meta.json" => content_json(&self.card_meta(&card)),
                    ".payload.json" => content_json(&card.payload),
                    "runtime.json" => {
                        let runtime = self.runtime_for_card(&card).await?;
                        content_json(&runtime)
                    }
                    "events.json" => {
                        let hook_events = self.hook_events_for_card(wave, &card.id).await?;
                        content_json(&hook_events_json(&hook_events))
                    }
                    "conversation.md" => {
                        // #695 PR3: prefer the captured worker-flow transcript
                        // when the sink has populated `worker_flow_items` for
                        // this card. The SOURCES that feed the table land in
                        // PR4, so for real cards it is empty today — fall back
                        // to the existing hook-event projection (no regression).
                        //
                        // Page through ALL rows: the db layer clamps `limit` to
                        // 500, so a single call would drop the tail (including
                        // the final answer) for sessions with >500 flow items.
                        // The hook path renders the full transcript uncapped, so
                        // this path must too.
                        let rows = worker_flow_rows_all(self.repo, card.id.as_str()).await?;
                        if rows.is_empty() {
                            let hook_events = self.hook_events_for_card(wave, &card.id).await?;
                            Ok(content_markdown(conversation_markdown(
                                &card.id,
                                &hook_events,
                            )))
                        } else {
                            // Version-tolerant: a row whose payload fails to
                            // deserialize (a future variant this binary does
                            // not know) becomes an `Unknown` placeholder rather
                            // than failing the whole read.
                            let items: Vec<calm_types::worker_flow::WorkerFlowItem> = rows
                                .iter()
                                .map(|row| deserialize_flow_row(&row.kind, &row.payload))
                                .collect();
                            Ok(content_markdown(worker_flow_markdown(&card.id, &items)))
                        }
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
            // Issue #644 PR-C (§6.5) — the gate runner's log for the
            // task's CURRENT gate attempt, read straight off disk
            // (file-backed; the row's `gate_result_json.log_tail` is
            // only the trailing 8 KiB).
            path if path.starts_with("plan/") => {
                let parts: Vec<&str> = path.split('/').collect();
                if parts.len() != 3 || parts[2] != "gate.log" {
                    return Err(path_not_available(path));
                }
                self.cat_gate_log(wave, parts[1]).await
            }
            other => Err(path_not_available(other)),
        }
    }

    /// `plan/<key>/gate.log` (issue #644 PR-C): spec-role-gated read of
    /// `<gate_logs_dir>/{task_id}-g{gate_attempt}.log`. Advisory
    /// content per §6.7 — the log is worker-reachable on disk; the
    /// verdict rides the wrapper exit status, never this file.
    async fn cat_gate_log(&self, wave: &Wave, key: &str) -> Result<WaveFsContent, WaveFsError> {
        let Some((role, gate_logs_dir)) = &self.gate_log_access else {
            return Err(WaveFsError::Forbidden(
                "wave_file: forbidden: plan/<key>/gate.log is not available on this surface"
                    .to_string(),
            ));
        };
        if *role != CardRole::Spec {
            return Err(WaveFsError::Forbidden(format!(
                "wave_file: forbidden: plan/{key}/gate.log is spec-only (§6.7); caller role {role:?}"
            )));
        }
        let task_id = format!("{}:{key}", wave.id.as_str());
        let task = self
            .repo
            .task_get(&task_id)
            .await
            .map_err(|e| WaveFsError::Internal(format!("wave_file: task lookup: {e}")))?
            .ok_or_else(|| path_not_available(&format!("plan/{key}/gate.log")))?;
        if task.gate_json.is_none() {
            return Err(path_not_available(&format!(
                "plan/{key}/gate.log (task declares no gate)"
            )));
        }
        if task.gate_attempt < 1 {
            return Err(path_not_available(&format!(
                "plan/{key}/gate.log (no gate attempt has run yet)"
            )));
        }
        let log_path = gate_logs_dir.join(format!("{task_id}-g{}.log", task.gate_attempt));
        match tokio::fs::read_to_string(&log_path).await {
            Ok(content) => Ok(WaveFsContent {
                content,
                content_type: "text/plain".into(),
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(path_not_available(
                &format!("plan/{key}/gate.log (log file not present yet)"),
            )),
            Err(e) => Err(WaveFsError::Internal(format!(
                "wave_file: gate log read {}: {e}",
                log_path.display()
            ))),
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

    async fn runtime_for_card(&self, card: &Card) -> Result<Option<CardRuntimeView>, WaveFsError> {
        self.repo
            .session_projection_projectable_for_card(&card.id.to_string())
            .await
            .map(|runtime| runtime.map(|runtime| runtime_view_from_runtime(&runtime)))
            .map_err(|e| WaveFsError::Internal(format!("wave_file: runtime projection: {e}")))
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
        if hooks.len() > HOOK_EVENT_TRANSCRIPT_CAP {
            hooks = hooks.split_off(hooks.len() - HOOK_EVENT_TRANSCRIPT_CAP);
        }
        Ok(hooks)
    }

    async fn runs_for_wave(&self, wave: &Wave) -> Result<Vec<RunProjection>, WaveFsError> {
        let cards = self.cards_for_wave(wave).await?;
        let events = self
            .repo
            .events_for_wave(
                wave.id.as_str(),
                &[
                    "codex.worker_requested",
                    "terminal.worker_requested",
                    "task.dispatched",
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

    fn card_meta(&self, card: &Card) -> WaveFsCardMeta {
        let role = self.write.verify_role(&card.id).unwrap_or_default();
        card_meta_value(card, role)
    }
}

pub(crate) fn card_meta_value(card: &Card, role: CardRole) -> WaveFsCardMeta {
    WaveFsCardMeta {
        id: card.id.clone(),
        kind: card.kind.clone(),
        role,
        sort: card.sort,
        deletable: card.deletable,
        created_at: card.created_at,
        updated_at: card.updated_at,
    }
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
    pub(crate) status: WaveFsRunStatus,
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
    let mut dispatched = BTreeMap::<String, RunEventProjection>::new();
    let mut dispatched_kind = BTreeMap::<String, &'static str>::new();
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
            // Issue #644 PR-B — the scheduler's claim record (§5.6).
            // Collected separately and merged below as the fallback
            // requested-record for keys with no `*.worker_requested`
            // event (scheduler-dispatched tasks emit none).
            Event::TaskDispatched {
                idempotency_key,
                kind,
                ..
            } => {
                keys.insert(idempotency_key.clone());
                dispatched_kind.insert(idempotency_key.clone(), run_kind_static(kind));
                record_earliest(
                    &mut dispatched,
                    idempotency_key,
                    run_event(row.id, row.at, "task.dispatched", row.event.payload_value()),
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

    // §5.6 fallback: a key with a `task.dispatched` record but no
    // `*.worker_requested` event treats the dispatch record as its
    // requested-record (`requested_at`, kind, requested/running status).
    for (key, event) in dispatched {
        requested.entry(key).or_insert(event);
    }
    for (key, kind) in dispatched_kind {
        requested_kind.entry(key).or_insert(kind);
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
                (Some(_), Some(("completed", event))) => {
                    (WaveFsRunStatus::Completed, Some(event.at))
                }
                (Some(_), Some(("failed", event))) => (WaveFsRunStatus::Failed, Some(event.at)),
                (Some(_), Some((_, event))) => (WaveFsRunStatus::Unknown, Some(event.at)),
                (Some(_), None) if worker_card.is_some() => (WaveFsRunStatus::Running, None),
                (Some(_), None) => (WaveFsRunStatus::Requested, None),
                (None, _) => (WaveFsRunStatus::Unknown, None),
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

/// Map a `task.dispatched` event's worker-kind field onto the static
/// run-kind vocabulary the projection uses. Unknown values degrade to
/// `"unknown"` (same convention as a key with no kind source at all).
pub(crate) fn run_kind_static(kind: &str) -> &'static str {
    match kind {
        "codex" => "codex",
        "claude" => "claude",
        "terminal" => "terminal",
        _ => "unknown",
    }
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
        "claude" => Some("claude"),
        "terminal" => Some("terminal"),
        _ => card
            .payload
            .get("role_request")
            .and_then(Value::as_str)
            .and_then(|kind| match kind {
                "codex" => Some("codex"),
                "claude" => Some("claude"),
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
        .with_extra("status", Value::String(run.status.as_str().into()))
        .with_extra("run_kind", Value::String(run.kind.clone()))
        .with_extra(
            "verdict",
            serde_json::to_value(run_verdict_index(run)).unwrap_or(Value::Null),
        )
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

pub(crate) fn run_index_entry(run: &RunProjection) -> WaveFsRunIndexEntry {
    WaveFsRunIndexEntry {
        idempotency_key: run.idempotency_key.clone(),
        status: run.status,
        kind: run.kind.clone(),
        verdict: run_verdict_index(run),
        requested_at: run.requested_at,
        finished_at: run.finished_at,
        worker_card_id: run.worker_card.as_ref().map(|card| card.id.clone()),
    }
}

pub(crate) fn run_json(run: &RunProjection) -> WaveFsRunDetail {
    WaveFsRunDetail {
        idempotency_key: run.idempotency_key.clone(),
        status: run.status,
        kind: run.kind.clone(),
        verdict: run_verdict_full(run),
        requested_at: run.requested_at,
        finished_at: run.finished_at,
        worker_card_id: run.worker_card.as_ref().map(|card| card.id.clone()),
        worker_card_payload: run.worker_card.as_ref().map(|card| card.payload.clone()),
        events: WaveFsRunEvents {
            requested: run.requested_event.as_ref().map(event_ref),
            completed: run.completed_event.as_ref().map(event_ref),
            failed: run.failed_event.as_ref().map(event_ref),
            verdict: run.verdict_event.as_ref().map(event_ref),
        },
    }
}

fn run_verdict_index(run: &RunProjection) -> Option<WaveFsRunVerdictSummary> {
    run.verdict.as_ref().map(|verdict| WaveFsRunVerdictSummary {
        status: verdict.status.clone(),
        at: verdict.at,
    })
}

fn run_verdict_full(run: &RunProjection) -> Option<WaveFsRunVerdict> {
    run.verdict.as_ref().map(|verdict| WaveFsRunVerdict {
        status: verdict.status.clone(),
        reason: verdict.reason.clone(),
        at: verdict.at,
    })
}

fn event_ref(event: &RunEventProjection) -> WaveFsRunEventRef {
    WaveFsRunEventRef {
        event_id: event.event_id,
        kind: event.kind.to_string(),
        created_at: event.at,
        payload: event.payload.clone(),
    }
}

pub(crate) fn hook_events_json(events: &[HookEventProjection]) -> Vec<WaveFsHookEvent> {
    events
        .iter()
        .map(|event| WaveFsHookEvent {
            event_id: event.event_id,
            kind: event.kind.to_string(),
            hook_kind: event.hook_kind.clone(),
            created_at: event.at,
            payload: event.payload.clone(),
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

/// Max length of any rendered string fragment before it is elided.
const FLOW_MD_TRUNCATE: usize = 120;

/// Truncate `s` to ~`FLOW_MD_TRUNCATE` chars (char-boundary safe), appending
/// an ellipsis when clipped. Multi-line strings collapse to the first line.
fn flow_truncate(s: &str) -> String {
    let first = s.lines().next().unwrap_or("").trim();
    if first.chars().count() <= FLOW_MD_TRUNCATE {
        return first.to_string();
    }
    let head: String = first.chars().take(FLOW_MD_TRUNCATE).collect();
    format!("{head}…")
}

/// #695 PR3 — render the captured worker-flow transcript a verifying spec
/// agent reads via `cards/<id>/conversation.md`. This is the meaningful
/// transcript (messages, commands + outcomes, file changes, tool calls),
/// not a bare tool log. Items are grouped by `env().turn` and rendered in
/// the order given (callers pass them in ascending `seq`).
pub(crate) fn worker_flow_markdown(
    card_id: &CardId,
    items: &[calm_types::worker_flow::WorkerFlowItem],
) -> String {
    use calm_types::worker_flow::{
        ExecStatus, FileChangeKind, McpStatus, PlanStatus, ReviewKind, WorkerFlowItem,
    };

    let mut out = String::new();
    out.push_str("> READ-ONLY PROJECTION: derived from persisted worker flow items. This is not the source of truth.\n\n");
    out.push_str(&format!("# Conversation — card {}\n\n", card_id.as_str()));

    if items.is_empty() {
        out.push_str("_No worker-flow items recorded._\n");
        return out;
    }

    // Pair tool results to their calls so a generic ToolCall can render its
    // outcome inline; ToolResult rows are then skipped on their own pass.
    let mut result_for_call: BTreeMap<&str, (bool, Option<String>)> = BTreeMap::new();
    for item in items {
        if let WorkerFlowItem::ToolResult {
            call_id,
            ok,
            output_summary,
            error,
            ..
        } = item
        {
            let summary = output_summary
                .clone()
                .or_else(|| error.as_ref().map(|e| e.message.clone()));
            result_for_call.insert(call_id.as_str(), (*ok, summary));
        }
    }

    let mut latest_file_change_for_call: BTreeMap<&str, usize> = BTreeMap::new();
    let mut latest_mcp_for_call: BTreeMap<&str, usize> = BTreeMap::new();
    let mut latest_web_search_for_call: BTreeMap<&str, usize> = BTreeMap::new();
    for (idx, item) in items.iter().enumerate() {
        match item {
            WorkerFlowItem::FileChange {
                call_id: Some(call_id),
                ..
            } => {
                latest_file_change_for_call.insert(call_id.as_str(), idx);
            }
            WorkerFlowItem::McpToolCall { call_id, .. } => {
                latest_mcp_for_call.insert(call_id.as_str(), idx);
            }
            WorkerFlowItem::WebSearch {
                call_id: Some(call_id),
                ..
            } => {
                latest_web_search_for_call.insert(call_id.as_str(), idx);
            }
            _ => {}
        }
    }

    let mut last_turn: Option<u32> = None;
    for (idx, item) in items.iter().enumerate() {
        let turn = item.env().turn;
        if last_turn != Some(turn) {
            out.push_str(&format!("### Turn {turn}\n\n"));
            last_turn = Some(turn);
        }

        match item {
            WorkerFlowItem::UserMessage { content, .. } => {
                let text = message_blocks_text(content);
                out.push_str("## User\n\n");
                out.push_str(&flow_truncate(&text));
                out.push_str("\n\n");
            }
            WorkerFlowItem::AgentMessage { text, is_final, .. } => {
                if *is_final {
                    out.push_str("## Assistant\n\n");
                    out.push_str(&flow_truncate(text));
                    out.push_str("\n\n");
                } else {
                    out.push_str(&flow_truncate(text));
                    out.push_str("\n\n");
                }
            }
            WorkerFlowItem::Reasoning { summary, .. } => {
                if let Some(line) = summary.iter().find(|s| !s.trim().is_empty()) {
                    out.push_str(&format!("_{}_\n\n", flow_truncate(line)));
                }
            }
            WorkerFlowItem::CommandExecution {
                command,
                exit_code,
                status,
                aggregated_output,
                ..
            } => {
                out.push_str(&format!("- ran `{}`", flow_truncate(command)));
                let failed =
                    matches!(status, ExecStatus::Failed) || exit_code.is_some_and(|c| c != 0);
                if failed {
                    match exit_code {
                        Some(code) => out.push_str(&format!(" ✗ exit {code}")),
                        None => out.push_str(" ✗"),
                    }
                    if let Some(o) = aggregated_output
                        .as_deref()
                        .filter(|o| !o.trim().is_empty())
                    {
                        out.push_str(&format!(" — {}", flow_truncate(o)));
                    }
                } else {
                    match status {
                        ExecStatus::InProgress => out.push_str(" ⋯ running"),
                        ExecStatus::Declined => out.push_str(" ⊘ declined"),
                        ExecStatus::Completed => out.push_str(" ✓"),
                        ExecStatus::Failed => unreachable!("matched above"),
                    }
                }
                out.push('\n');
            }
            WorkerFlowItem::FileChange {
                call_id, changes, ..
            } => {
                let should_render = call_id.as_ref().is_none_or(|call_id| {
                    latest_file_change_for_call
                        .get(call_id.as_str())
                        .is_none_or(|latest| *latest == idx)
                });
                if should_render {
                    for change in changes {
                        let verb = match &change.kind {
                            FileChangeKind::Add => "add",
                            FileChangeKind::Delete => "delete",
                            FileChangeKind::Update { .. } => "edit",
                        };
                        out.push_str(&format!("- {verb} {}\n", flow_truncate(&change.path)));
                    }
                }
            }
            WorkerFlowItem::ToolCall { call_id, name, .. } => {
                out.push_str(&format!("- {}", flow_truncate(name)));
                if let Some((ok, summary)) = result_for_call.get(call_id.as_str()) {
                    out.push_str(if *ok { " ✓" } else { " ✗" });
                    if let Some(s) = summary.as_deref().filter(|s| !s.trim().is_empty()) {
                        out.push_str(&format!(" — {}", flow_truncate(s)));
                    }
                }
                out.push('\n');
            }
            WorkerFlowItem::ToolResult { .. } => {
                // Rendered inline with its paired ToolCall above.
            }
            WorkerFlowItem::McpToolCall {
                call_id,
                server,
                tool,
                status,
                ..
            } => {
                let should_render = latest_mcp_for_call
                    .get(call_id.as_str())
                    .is_none_or(|latest| *latest == idx);
                if should_render {
                    let name = match server {
                        Some(s) => format!("{s}.{tool}"),
                        None => tool.clone(),
                    };
                    out.push_str(&format!("- {}", flow_truncate(&name)));
                    match status {
                        McpStatus::Completed => out.push_str(" ✓"),
                        McpStatus::Failed => out.push_str(" ✗"),
                        McpStatus::InProgress => {}
                    }
                    out.push('\n');
                }
            }
            WorkerFlowItem::WebSearch { call_id, query, .. } => {
                let should_render = call_id.as_ref().is_none_or(|call_id| {
                    latest_web_search_for_call
                        .get(call_id.as_str())
                        .is_none_or(|latest| *latest == idx)
                });
                if should_render {
                    let q = query.as_deref().unwrap_or("");
                    out.push_str(&format!("- searched: {}\n", flow_truncate(q)));
                }
            }
            WorkerFlowItem::Plan { entries, .. } => {
                for entry in entries {
                    let box_ = match entry.status {
                        PlanStatus::Completed => "[x]",
                        PlanStatus::Pending | PlanStatus::InProgress => "[ ]",
                    };
                    out.push_str(&format!("- {box_} {}\n", flow_truncate(&entry.text)));
                }
            }
            WorkerFlowItem::Subagent { tool, .. } => {
                let label = tool.as_deref().unwrap_or("task");
                out.push_str(&format!("- subagent: {}\n", flow_truncate(label)));
            }
            WorkerFlowItem::Compaction { .. } => {
                out.push_str("- _(context compacted)_\n");
            }
            WorkerFlowItem::ReviewBoundary { kind, label, .. } => {
                let verb = match kind {
                    ReviewKind::Entered => "entered",
                    ReviewKind::Exited => "exited",
                };
                let label = label.as_deref().unwrap_or("review");
                out.push_str(&format!("- _{} {}_\n", verb, flow_truncate(label)));
            }
            WorkerFlowItem::Unknown { raw_type, .. } => {
                out.push_str(&format!("- ({})\n", flow_truncate(raw_type)));
            }
        }
    }
    out
}

/// Deserialize a `worker_flow_items` row payload into a [`WorkerFlowItem`],
/// degrading to an `Unknown` placeholder (carrying the row's `kind`) when the
/// payload cannot be parsed by this binary — forward-version tolerance so one
/// future-shaped row never blanks the whole transcript.
/// Page through EVERY `worker_flow_items` row for a card in ascending `id`
/// order. The db layer clamps `limit` to 500 (see `worker_flow_item_list_by_card`),
/// so a single call would silently drop the tail of a long session — including
/// the final `AgentMessage{is_final:true}` answer. We advance the exclusive
/// cursor by the last row's id and stop on a short page (table exhausted),
/// preserving order and turn grouping. Mirrors the hook path's
/// render-everything contract (no artificial bound).
async fn worker_flow_rows_all(
    repo: &dyn RouteRepo,
    card_id: &str,
) -> Result<Vec<crate::db::rows::WorkerFlowItemRow>, WaveFsError> {
    let mut all_rows = Vec::new();
    let mut after_id = 0i64;
    loop {
        let page = repo
            .worker_flow_item_list_by_card(card_id, after_id, 500, false)
            .await
            .map_err(|e| {
                WaveFsError::Internal(format!("wave_file: worker_flow_item_list_by_card: {e}"))
            })?;
        let n = page.len();
        if let Some(last) = page.last() {
            after_id = last.id;
        }
        all_rows.extend(page);
        if n < 500 {
            break; // short page = exhausted
        }
    }
    Ok(all_rows)
}

fn deserialize_flow_row(kind: &str, payload: &str) -> calm_types::worker_flow::WorkerFlowItem {
    use calm_types::worker::{WorkerProviderKind, WorkerSessionId};
    use calm_types::worker_flow::{FlowEnvelope, WorkerFlowItem};
    serde_json::from_str::<WorkerFlowItem>(payload).unwrap_or_else(|_| WorkerFlowItem::Unknown {
        env: FlowEnvelope {
            seq: 0,
            turn: 0,
            session_id: WorkerSessionId::from(""),
            provider: WorkerProviderKind::Codex,
            timestamp: None,
            source_uuid: None,
            provider_extra: None,
            raw_ref: None,
        },
        raw_type: kind.to_string(),
    })
}

/// Flatten a user message's blocks into a single rendered string.
fn message_blocks_text(blocks: &[calm_types::worker_flow::MessageBlock]) -> String {
    use calm_types::worker_flow::MessageBlock;
    let mut parts = Vec::new();
    for block in blocks {
        match block {
            MessageBlock::Text { text } => parts.push(text.clone()),
            MessageBlock::Image { .. } => parts.push("[image]".to_string()),
            MessageBlock::FileRef { path } => parts.push(format!("@{path}")),
            MessageBlock::Mention { name, .. } => parts.push(format!("@{name}")),
        }
    }
    parts.join(" ")
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
                "[{}](../cards/{}/.payload.json)",
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::WaveId;
    use serde::Serialize;
    use serde_json::{Value, json};

    fn pretty_json<T: Serialize>(value: &T) -> String {
        serde_json::to_string_pretty(value).expect("value serializes")
    }

    fn assert_same_json_bytes<T: Serialize>(new_value: &T, old_value: &Value) {
        assert_eq!(pretty_json(new_value), pretty_json(old_value));
    }

    fn test_card(id: &str, kind: &str, payload: Value) -> Card {
        Card {
            id: CardId::from(id),
            wave_id: WaveId::from("wave-test"),
            kind: kind.to_string(),
            sort: 1.25,
            payload,
            runtime: None,
            deletable: true,
            created_at: 1000,
            updated_at: 2000,
        }
    }

    fn run_event(event_id: i64, at: i64, kind: &'static str, payload: Value) -> RunEventProjection {
        RunEventProjection {
            event_id,
            at,
            kind,
            payload,
        }
    }

    fn old_card_meta_value(card: &Card, role: Value) -> Value {
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

    fn old_run_index_entry(run: &RunProjection) -> Value {
        json!({
            "idempotency_key": run.idempotency_key,
            "status": run.status.as_str(),
            "kind": run.kind,
            "verdict": old_run_verdict_index_json(run),
            "requested_at": run.requested_at,
            "finished_at": run.finished_at,
            "worker_card_id": run.worker_card.as_ref().map(|card| card.id.as_str()),
        })
    }

    fn old_run_json(run: &RunProjection) -> Value {
        json!({
            "idempotency_key": run.idempotency_key,
            "status": run.status.as_str(),
            "kind": run.kind,
            "verdict": old_run_verdict_full_json(run),
            "requested_at": run.requested_at,
            "finished_at": run.finished_at,
            "worker_card_id": run.worker_card.as_ref().map(|card| card.id.as_str()),
            "worker_card_payload": run.worker_card.as_ref().map(|card| card.payload.clone()),
            "events": {
                "requested": run.requested_event.as_ref().map(old_event_json),
                "completed": run.completed_event.as_ref().map(old_event_json),
                "failed": run.failed_event.as_ref().map(old_event_json),
                "verdict": run.verdict_event.as_ref().map(old_event_json),
            },
        })
    }

    fn old_run_verdict_index_json(run: &RunProjection) -> Value {
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

    fn old_run_verdict_full_json(run: &RunProjection) -> Value {
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

    fn old_event_json(event: &RunEventProjection) -> Value {
        json!({
            "event_id": event.event_id,
            "kind": event.kind,
            "created_at": event.at,
            "payload": event.payload,
        })
    }

    fn old_hook_events_json(events: &[HookEventProjection]) -> Vec<Value> {
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

    fn run_with_verdict_and_events() -> RunProjection {
        let worker_card = test_card(
            "card-worker",
            "codex",
            json!({
                "idempotency_key": "run-full",
                "prompt": "do the thing",
                "context": {"priority": "high"},
            }),
        );
        let requested_event = run_event(
            10,
            1100,
            "codex.worker_requested",
            json!({"idempotency_key": "run-full", "goal": "ship it"}),
        );
        let completed_event = run_event(
            11,
            1200,
            "task.completed",
            json!({"idempotency_key": "run-full", "result": {"summary": "done"}}),
        );
        let verdict_event = run_event(
            12,
            1300,
            "task.completed",
            json!({"idempotency_key": "run-full", "result": {"status": "accepted"}}),
        );
        RunProjection {
            idempotency_key: "run-full".into(),
            status: WaveFsRunStatus::Completed,
            kind: "codex".into(),
            requested_at: Some(requested_event.at),
            finished_at: Some(completed_event.at),
            worker_card: Some(worker_card),
            requested_event: Some(requested_event),
            completed_event: Some(completed_event),
            failed_event: None,
            verdict: Some(RunVerdictProjection {
                status: "accepted".into(),
                reason: None,
                at: verdict_event.at,
            }),
            verdict_event: Some(verdict_event),
        }
    }

    fn run_without_verdict_or_events() -> RunProjection {
        RunProjection {
            idempotency_key: "run-empty".into(),
            status: WaveFsRunStatus::Unknown,
            kind: "unknown".into(),
            requested_at: None,
            finished_at: None,
            worker_card: None,
            requested_event: None,
            completed_event: None,
            failed_event: None,
            verdict: None,
            verdict_event: None,
        }
    }

    #[test]
    fn card_meta_dto_serializes_like_old_json_builder() {
        let card = test_card("card-meta", "terminal", json!({"terminal_id": "term-1"}));

        assert_same_json_bytes(
            &card_meta_value(&card, CardRole::Worker),
            &old_card_meta_value(&card, json!("worker")),
        );
        assert_same_json_bytes(
            &card_meta_value(&card, CardRole::default()),
            &old_card_meta_value(&card, json!("worker")),
        );
        assert_same_json_bytes(
            &card_meta_value(&card, CardRole::Spec),
            &old_card_meta_value(&card, json!("spec")),
        );
        assert_same_json_bytes(
            &card_meta_value(&card, CardRole::ReportCard),
            &old_card_meta_value(&card, json!("reportcard")),
        );
    }

    #[test]
    fn run_index_entry_dto_serializes_like_old_json_builder() {
        let full = run_with_verdict_and_events();
        assert_same_json_bytes(&run_index_entry(&full), &old_run_index_entry(&full));

        let full_value = serde_json::to_value(run_index_entry(&full)).expect("dto serializes");
        assert!(
            full_value["verdict"].get("reason").is_none(),
            "index verdict must not include reason: {full_value:?}"
        );

        let empty = run_without_verdict_or_events();
        assert_same_json_bytes(&run_index_entry(&empty), &old_run_index_entry(&empty));
        let empty_value = serde_json::to_value(run_index_entry(&empty)).expect("dto serializes");
        assert!(empty_value["verdict"].is_null());
        assert!(empty_value["worker_card_id"].is_null());
        assert!(empty_value["requested_at"].is_null());
        assert!(empty_value["finished_at"].is_null());
    }

    #[test]
    fn run_detail_dto_serializes_like_old_json_builder() {
        let full = run_with_verdict_and_events();
        assert_same_json_bytes(&run_json(&full), &old_run_json(&full));
        let full_value = serde_json::to_value(run_json(&full)).expect("dto serializes");
        assert!(full_value["events"]["requested"].is_object());
        assert!(full_value["events"]["completed"].is_object());
        assert!(full_value["events"]["failed"].is_null());
        assert!(full_value["events"]["verdict"].is_object());
        assert!(
            full_value["verdict"]["reason"].is_null(),
            "detail verdict must emit explicit null reason: {full_value:?}"
        );

        let empty = run_without_verdict_or_events();
        assert_same_json_bytes(&run_json(&empty), &old_run_json(&empty));
        let empty_value = serde_json::to_value(run_json(&empty)).expect("dto serializes");
        assert!(empty_value["verdict"].is_null());
        assert!(empty_value["worker_card_id"].is_null());
        assert!(empty_value["worker_card_payload"].is_null());
        assert!(empty_value["events"]["requested"].is_null());
        assert!(empty_value["events"]["completed"].is_null());
        assert!(empty_value["events"]["failed"].is_null());
        assert!(empty_value["events"]["verdict"].is_null());
    }

    // ------------------------------------------------------------------
    // Issue #644 PR-B — §5.6 requested-record fallback: a key with a
    // `task.dispatched` claim record but no `*.worker_requested` event
    // projects from the dispatch record (requested_at, kind, the
    // requested/running/terminal statuses).
    // ------------------------------------------------------------------

    fn fallback_write() -> WriteContext {
        WriteContext::new(
            crate::card_role_cache::CardRoleCache::new(),
            crate::wave_cove_cache::WaveCoveCache::new(),
        )
    }

    fn wave_scoped(id: i64, at: i64, actor: ActorId, event: Event) -> WaveEvent {
        WaveEvent {
            id,
            at,
            actor,
            scope: EventScope::Wave {
                wave: WaveId::from("wave-test"),
                cove: crate::ids::CoveId::from("cove-test"),
            },
            event,
        }
    }

    fn dispatched_event(id: i64, at: i64, key: &str, kind: &str) -> WaveEvent {
        wave_scoped(
            id,
            at,
            ActorId::KernelDispatcher,
            Event::TaskDispatched {
                idempotency_key: key.into(),
                kind: kind.into(),
                agent_message: None,
            },
        )
    }

    #[test]
    fn project_runs_uses_task_dispatched_as_requested_record_fallback() {
        let write = fallback_write();
        let runs = project_runs(
            &write,
            vec![],
            vec![dispatched_event(5, 500, "w:k", "codex")],
        );
        assert_eq!(runs.len(), 1);
        let run = &runs[0];
        assert_eq!(run.idempotency_key, "w:k");
        assert_eq!(
            run.status,
            WaveFsRunStatus::Requested,
            "dispatch record alone (no worker card visible) → requested"
        );
        assert_eq!(
            run.requested_at,
            Some(500),
            "requested_at from the claim record"
        );
        assert_eq!(run.kind, "codex", "kind from the claim record");
        assert_eq!(
            run.requested_event.as_ref().map(|e| e.kind),
            Some("task.dispatched"),
            "the dispatch record IS the requested-record"
        );
    }

    #[test]
    fn project_runs_dispatched_then_completed_resolves_terminal_status() {
        let write = fallback_write();
        let completed = wave_scoped(
            6,
            600,
            // Kernel-emitted completion (terminal-exit path) — actor
            // KernelDispatcher means NOT a spec verdict.
            ActorId::KernelDispatcher,
            Event::TaskCompleted {
                idempotency_key: "w:k".into(),
                result: json!({"exit_code": 0}),
                artifacts: vec![],
                agent_message: None,
            },
        );
        let runs = project_runs(
            &write,
            vec![],
            vec![dispatched_event(5, 500, "w:k", "terminal"), completed],
        );
        assert_eq!(runs.len(), 1);
        let run = &runs[0];
        assert_eq!(run.status, WaveFsRunStatus::Completed);
        assert_eq!(run.requested_at, Some(500));
        assert_eq!(run.finished_at, Some(600));
        assert_eq!(run.kind, "terminal");
        assert!(
            run.verdict.is_none(),
            "KernelDispatcher completion must never classify as a spec verdict"
        );
    }

    #[test]
    fn project_runs_real_requested_event_wins_over_dispatch_record() {
        // Legacy `calm.task.dispatch` keys keep their `*.worker_requested`
        // record even if a dispatch record ever coexisted; the fallback
        // is fallback-only.
        let write = fallback_write();
        let requested = wave_scoped(
            2,
            200,
            ActorId::User,
            Event::TerminalWorkerRequested {
                idempotency_key: "w:k".into(),
                cmd: "ls".into(),
                cwd: None,
                agent_message: None,
            },
        );
        let runs = project_runs(
            &write,
            vec![],
            vec![requested, dispatched_event(5, 500, "w:k", "codex")],
        );
        assert_eq!(runs.len(), 1);
        let run = &runs[0];
        assert_eq!(
            run.requested_event.as_ref().map(|e| e.kind),
            Some("terminal.worker_requested"),
            "real requested event wins"
        );
        assert_eq!(run.requested_at, Some(200));
        assert_eq!(
            run.kind, "terminal",
            "requested kind wins over dispatch-record kind"
        );
    }

    #[test]
    fn run_kind_static_vocabulary() {
        assert_eq!(run_kind_static("codex"), "codex");
        assert_eq!(run_kind_static("terminal"), "terminal");
        assert_eq!(run_kind_static("claude"), "claude");
    }

    #[test]
    fn hook_events_dto_serializes_like_old_json_builder() {
        let events = vec![
            HookEventProjection {
                event_id: 21,
                at: 2100,
                kind: "codex.hook",
                hook_kind: "hook.codex.user_prompt_submit".into(),
                payload: json!({"prompt": "hello"}),
            },
            HookEventProjection {
                event_id: 22,
                at: 2200,
                kind: "claude.hook",
                hook_kind: "Stop".into(),
                payload: json!({"last_assistant_message": "done"}),
            },
        ];

        assert_same_json_bytes(
            &hook_events_json(&events),
            &json!(old_hook_events_json(&events)),
        );
    }

    // ---- #695 PR3: worker-flow markdown projection -------------------------

    fn flow_env(seq: u64, turn: u32) -> calm_types::worker_flow::FlowEnvelope {
        use calm_types::worker::{WorkerProviderKind, WorkerSessionId};
        calm_types::worker_flow::FlowEnvelope {
            seq,
            turn,
            session_id: WorkerSessionId::from("sess-1"),
            provider: WorkerProviderKind::Codex,
            timestamp: None,
            source_uuid: None,
            provider_extra: None,
            raw_ref: None,
        }
    }

    #[test]
    fn worker_flow_markdown_renders_meaningful_transcript() {
        use calm_types::worker_flow::{
            ExecSource, ExecStatus, FileChangeKind, FileEdit, MessageBlock, PatchStatus,
            ToolCallId, WorkerFlowItem,
        };

        let items = vec![
            WorkerFlowItem::UserMessage {
                env: flow_env(0, 1),
                content: vec![MessageBlock::Text {
                    text: "Fix the build".into(),
                }],
            },
            WorkerFlowItem::CommandExecution {
                env: flow_env(1, 1),
                call_id: Some(ToolCallId::from("c1")),
                command: "cargo build".into(),
                cwd: None,
                parsed_actions: vec![],
                aggregated_output: None,
                exit_code: Some(0),
                duration_ms: None,
                status: ExecStatus::Completed,
                source: ExecSource::Agent,
            },
            WorkerFlowItem::CommandExecution {
                env: flow_env(2, 1),
                call_id: Some(ToolCallId::from("c2")),
                command: "cargo test".into(),
                cwd: None,
                parsed_actions: vec![],
                aggregated_output: Some("assertion failed: foo".into()),
                exit_code: Some(101),
                duration_ms: None,
                status: ExecStatus::Failed,
                source: ExecSource::Agent,
            },
            WorkerFlowItem::FileChange {
                env: flow_env(3, 1),
                call_id: None,
                changes: vec![FileEdit {
                    path: "src/lib.rs".into(),
                    kind: FileChangeKind::Update { move_path: None },
                    diff: None,
                }],
                status: PatchStatus::Completed,
            },
            WorkerFlowItem::AgentMessage {
                env: flow_env(4, 1),
                text: "All green now.".into(),
                is_final: true,
                phase: None,
            },
            WorkerFlowItem::Unknown {
                env: flow_env(5, 1),
                raw_type: "future.provider.thing".into(),
            },
        ];

        let md = worker_flow_markdown(&CardId::from("card-9"), &items);

        assert!(md.starts_with(
            "> READ-ONLY PROJECTION: derived from persisted worker flow items. This is not the source of truth."
        ));
        assert!(md.contains("# Conversation — card card-9"));
        assert!(md.contains("## User\n\nFix the build"));
        assert!(md.contains("- ran `cargo build` ✓"), "md = {md}");
        assert!(
            md.contains("- ran `cargo test` ✗ exit 101 — assertion failed: foo"),
            "md = {md}"
        );
        assert!(md.contains("- edit src/lib.rs"), "md = {md}");
        assert!(md.contains("## Assistant\n\nAll green now."), "md = {md}");
        assert!(md.contains("- (future.provider.thing)"), "md = {md}");
    }

    #[test]
    fn worker_flow_markdown_coalesces_file_changes_by_call_id() {
        use calm_types::worker_flow::{
            FileChangeKind, FileEdit, PatchStatus, ToolCallId, WorkerFlowItem,
        };

        let edit_a = FileEdit {
            path: "a.rs".into(),
            kind: FileChangeKind::Update { move_path: None },
            diff: None,
        };
        let edit_b = FileEdit {
            path: "b.rs".into(),
            kind: FileChangeKind::Update { move_path: None },
            diff: None,
        };
        let add_free = FileEdit {
            path: "free.rs".into(),
            kind: FileChangeKind::Add,
            diff: None,
        };
        let items = vec![
            WorkerFlowItem::FileChange {
                env: flow_env(0, 1),
                call_id: Some(ToolCallId::from("c1")),
                changes: vec![edit_a.clone()],
                status: PatchStatus::InProgress,
            },
            WorkerFlowItem::FileChange {
                env: flow_env(1, 1),
                call_id: Some(ToolCallId::from("c1")),
                changes: vec![edit_a],
                status: PatchStatus::Completed,
            },
            WorkerFlowItem::FileChange {
                env: flow_env(2, 1),
                call_id: Some(ToolCallId::from("c2")),
                changes: vec![edit_b],
                status: PatchStatus::InProgress,
            },
            WorkerFlowItem::FileChange {
                env: flow_env(3, 1),
                call_id: None,
                changes: vec![add_free],
                status: PatchStatus::Completed,
            },
        ];

        let md = worker_flow_markdown(&CardId::from("card-file-coalesce"), &items);

        assert_eq!(md.matches("- edit a.rs\n").count(), 1, "md = {md}");
        assert_eq!(md.matches("- edit b.rs\n").count(), 1, "md = {md}");
        assert_eq!(md.matches("- add free.rs\n").count(), 1, "md = {md}");
    }

    #[test]
    fn worker_flow_markdown_coalesces_web_searches_by_call_id() {
        use calm_types::worker_flow::{ToolCallId, WorkerFlowItem};

        let items = vec![
            WorkerFlowItem::WebSearch {
                env: flow_env(0, 1),
                call_id: Some(ToolCallId::from("w1")),
                query: Some("rust serde".into()),
                results_summary: None,
            },
            WorkerFlowItem::WebSearch {
                env: flow_env(1, 1),
                call_id: Some(ToolCallId::from("w1")),
                query: Some("rust serde".into()),
                results_summary: Some("3 results".into()),
            },
        ];

        let md = worker_flow_markdown(&CardId::from("card-web-coalesce"), &items);

        assert_eq!(
            md.matches("- searched: rust serde\n").count(),
            1,
            "md = {md}"
        );
    }

    #[test]
    fn worker_flow_markdown_coalesces_mcp_tool_calls_by_call_id() {
        use calm_types::worker_flow::{McpStatus, ToolCallId, WorkerFlowItem};

        let items = vec![
            WorkerFlowItem::McpToolCall {
                env: flow_env(0, 1),
                call_id: ToolCallId::from("m1"),
                server: Some("fs".into()),
                tool: "read".into(),
                arguments: json!({"path": "a.txt"}),
                status: McpStatus::InProgress,
                result: None,
                error: None,
                duration_ms: None,
            },
            WorkerFlowItem::McpToolCall {
                env: flow_env(1, 1),
                call_id: ToolCallId::from("m1"),
                server: Some("fs".into()),
                tool: "read".into(),
                arguments: json!({"path": "a.txt"}),
                status: McpStatus::Completed,
                result: Some(json!({"content": "ok"})),
                error: None,
                duration_ms: Some(12),
            },
        ];

        let md = worker_flow_markdown(&CardId::from("card-mcp-coalesce"), &items);

        assert_eq!(md.matches("- fs.read").count(), 1, "md = {md}");
        assert!(md.contains("- fs.read ✓"), "md = {md}");

        let distinct = vec![
            WorkerFlowItem::McpToolCall {
                env: flow_env(0, 1),
                call_id: ToolCallId::from("m2"),
                server: Some("fs".into()),
                tool: "first".into(),
                arguments: json!({}),
                status: McpStatus::Completed,
                result: Some(json!({"ok": true})),
                error: None,
                duration_ms: None,
            },
            WorkerFlowItem::McpToolCall {
                env: flow_env(1, 1),
                call_id: ToolCallId::from("m3"),
                server: Some("fs".into()),
                tool: "second".into(),
                arguments: json!({}),
                status: McpStatus::Completed,
                result: Some(json!({"ok": true})),
                error: None,
                duration_ms: None,
            },
        ];

        let md = worker_flow_markdown(&CardId::from("card-mcp-distinct"), &distinct);

        assert_eq!(md.matches("- fs.first ✓\n").count(), 1, "md = {md}");
        assert_eq!(md.matches("- fs.second ✓\n").count(), 1, "md = {md}");
    }

    #[test]
    fn worker_flow_markdown_renders_command_execution_statuses() {
        use calm_types::worker_flow::{ExecSource, ExecStatus, WorkerFlowItem};

        fn command_item(
            seq: u64,
            command: &str,
            status: ExecStatus,
            exit_code: Option<i32>,
            aggregated_output: Option<&str>,
        ) -> WorkerFlowItem {
            WorkerFlowItem::CommandExecution {
                env: flow_env(seq, 1),
                call_id: None,
                command: command.into(),
                cwd: None,
                parsed_actions: vec![],
                aggregated_output: aggregated_output.map(str::to_string),
                exit_code,
                duration_ms: None,
                status,
                source: ExecSource::Agent,
            }
        }

        let items = vec![
            command_item(0, "cargo check", ExecStatus::InProgress, None, None),
            command_item(1, "cargo fmt", ExecStatus::Completed, Some(0), None),
            command_item(2, "cargo test", ExecStatus::Failed, Some(1), Some("boom")),
            command_item(3, "dangerous command", ExecStatus::Declined, None, None),
        ];

        let md = worker_flow_markdown(&CardId::from("card-statuses"), &items);

        assert!(md.contains("- ran `cargo check` ⋯ running"), "md = {md}");
        assert!(!md.contains("- ran `cargo check` ✓"), "md = {md}");
        assert!(md.contains("- ran `cargo fmt` ✓"), "md = {md}");
        assert!(
            md.contains("- ran `cargo test` ✗ exit 1 — boom"),
            "md = {md}"
        );
        assert!(
            md.contains("- ran `dangerous command` ⊘ declined"),
            "md = {md}"
        );
    }

    #[test]
    fn worker_flow_markdown_empty_reports_no_items() {
        let md = worker_flow_markdown(&CardId::from("card-0"), &[]);
        assert!(md.contains("_No worker-flow items recorded._"), "md = {md}");
    }

    /// Regression for #695 PR3: a worker session with >500 flow items must
    /// render the WHOLE transcript. The db layer clamps `limit` to 500, so a
    /// single `worker_flow_item_list_by_card(.., 0, 1000, false)` returns only
    /// the OLDEST 500 rows (ascending) and DROPS the tail — including the final
    /// `AgentMessage{is_final:true}` answer. `worker_flow_rows_all` (the same
    /// paging path the `conversation.md` cat branch uses) must page through all
    /// of them. Without the fix this test fails: the final answer (highest id)
    /// lands past row 500 and never reaches the rendered markdown.
    #[tokio::test]
    async fn conversation_md_paging_renders_full_transcript_over_500_items() {
        use crate::db::sqlite::{
            SqlxRepo, card_create_with_id_tx, cove_create_tx, session_insert_tx, wave_create_tx,
            worker_flow_item_insert_tx,
        };
        use crate::model::{NewCard, NewCove, NewWave, RequestTheme};
        use calm_types::worker::{
            LivenessTag, SessionMode, WorkerContract, WorkerProviderKind, WorkerSession,
            WorkerSessionId, WorkerSessionState,
        };
        use calm_types::worker_flow::{MessageBlock, WorkerFlowItem};

        const FIRST_USER: &str = "FIRST-USER-MESSAGE-MARKER";
        const FINAL_ANSWER: &str = "FINAL-ANSWER-MARKER-TAIL-NOT-DROPPED";

        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();

        // Seed a real cove → wave → card chain (FK target) and bulk-insert
        // 600 flow items in ONE transaction for speed: a UserMessage first,
        // 598 CommandExecution rows, then the final AgentMessage LAST (highest
        // id). `flow_env(seq, turn)` keeps every item in turn 1.
        let mut tx = repo.pool().begin().await.unwrap();
        let cove = cove_create_tx(
            &mut tx,
            NewCove {
                name: "c".into(),
                color: "#fff".into(),
                sort: None,
            },
        )
        .await
        .unwrap();
        let wave = wave_create_tx(
            &mut tx,
            NewWave {
                cove_id: cove.id.clone(),
                title: "w".into(),
                sort: None,
                cwd: "/tmp".into(),
                workflow_id: None,
                attach_folder: false,
                theme: RequestTheme::default_dark(),
            },
            repo.wave_cove_cache(),
        )
        .await
        .unwrap();
        let card = card_create_with_id_tx(
            &mut tx,
            "card-big".into(),
            NewCard {
                wave_id: wave.id.clone(),
                kind: "worker".into(),
                sort: None,
                payload: serde_json::json!({}),
            },
            CardRole::Worker,
            true,
            repo.card_role_cache(),
        )
        .await
        .unwrap();
        let card_id = card.id.to_string();
        let session_id = "rt-card-big";
        session_insert_tx(
            &mut tx,
            WorkerSession {
                id: WorkerSessionId::from(session_id),
                wave_id: wave.id.clone(),
                provider: WorkerProviderKind::Codex,
                mode: SessionMode::Resumable,
                contract: WorkerContract::Executor,
                parent_session_id: None,
                requester_session_id: None,
                state: WorkerSessionState::Running,
                mcp_token_hash: None,
                thread_id: Some("thread-card-big".into()),
                agent_session_id: Some("agent-card-big".into()),
                active_turn_id: None,
                terminal_run_id: None,
                card_id: Some(calm_types::ids::CardId(card_id.clone())),
                handle_state_json: None,
                liveness: LivenessTag::Alive,
                liveness_probed_at_ms: None,
                exit_code: None,
                exit_interpretation: None,
                spawn_op_id: None,
                last_activity_ms: None,
                last_thread_status: None,
                created_at_ms: 1,
                updated_at_ms: 1,
                completed_at_ms: None,
            },
        )
        .await
        .unwrap();

        // First user message (lowest id).
        let first = WorkerFlowItem::UserMessage {
            env: flow_env(0, 1),
            content: vec![MessageBlock::Text {
                text: FIRST_USER.into(),
            }],
        };
        worker_flow_item_insert_tx(
            &mut tx,
            Some(&card_id),
            Some(session_id),
            Some(wave.id.as_str()),
            Some(session_id),
            "user_message",
            &serde_json::to_string(&first).unwrap(),
            1,
        )
        .await
        .unwrap();

        // 598 command executions in the middle.
        for n in 0..598u64 {
            let item = WorkerFlowItem::CommandExecution {
                env: flow_env(n + 1, 1),
                call_id: None,
                command: format!("echo {n}"),
                cwd: None,
                parsed_actions: vec![],
                aggregated_output: None,
                exit_code: Some(0),
                duration_ms: None,
                status: calm_types::worker_flow::ExecStatus::Completed,
                source: calm_types::worker_flow::ExecSource::Agent,
            };
            worker_flow_item_insert_tx(
                &mut tx,
                Some(&card_id),
                Some(session_id),
                Some(wave.id.as_str()),
                Some(session_id),
                "command_execution",
                &serde_json::to_string(&item).unwrap(),
                (2 + n) as i64,
            )
            .await
            .unwrap();
        }

        // Final answer LAST (highest id) — well past row 500.
        let final_item = WorkerFlowItem::AgentMessage {
            env: flow_env(599, 1),
            text: FINAL_ANSWER.into(),
            is_final: true,
            phase: None,
        };
        worker_flow_item_insert_tx(
            &mut tx,
            Some(&card_id),
            Some(session_id),
            Some(wave.id.as_str()),
            Some(session_id),
            "assistant_message",
            &serde_json::to_string(&final_item).unwrap(),
            600,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        // Render through the SAME paging path the cat branch uses.
        let rows = worker_flow_rows_all(&repo, &card_id).await.unwrap();
        assert_eq!(rows.len(), 600, "all 600 rows must be paged in");
        let items: Vec<WorkerFlowItem> = rows
            .iter()
            .map(|row| deserialize_flow_row(&row.kind, &row.payload))
            .collect();
        let md = worker_flow_markdown(&card.id, &items);

        assert!(
            md.contains(FIRST_USER),
            "first user message must be rendered"
        );
        assert!(
            md.contains(FINAL_ANSWER),
            "final answer (tail past row 500) must NOT be dropped"
        );
    }
}
