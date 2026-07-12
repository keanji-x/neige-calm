use crate::error::Result;
use crate::event::Event;
use crate::ids::{CardId, WaveId};
use crate::wave_fs_view::{self, RunProjection};
use serde_json::Value;
use sqlx::{Row, Sqlite, Transaction};
use std::collections::{BTreeMap, BTreeSet};

use super::runs::{idempotency_key_from_payload, project_run_by_key_tx, project_runs_tx};
use super::snapshot::{
    card_in_wave_tx, card_meta_json, card_payload_json, card_runtime_json, cards_for_wave_tx,
    cards_index_json, content_json, content_markdown, conversation_markdown,
    hook_events_for_card_tx, hook_events_json, index_markdown, load_wave_optional_tx,
    project_runtime_into_cards_tx, report_markdown, run_json, run_markdown, runs_index_json,
    wave_json,
};
use super::store::{load_blob_bytes_tx, normalize_path, put_rendered_entry};
use super::types::{BlobContent, CardVisibility};
use super::{ManifestEntry, TreeManifest};

#[derive(Default)]
pub(super) struct PathDelta {
    exact: BTreeSet<String>,
    remove_prefixes: BTreeSet<String>,
    run_keys: BTreeSet<String>,
    run_card_ids: BTreeSet<String>,
    /// Safety valve for future schema-wide projection changes. The current
    /// event set is intentionally handled as path-level deltas.
    pub(super) full_snapshot: bool,
}

impl PathDelta {
    fn add(&mut self, path: impl Into<String>) {
        self.exact.insert(path.into());
    }

    fn remove_prefix(&mut self, prefix: impl Into<String>) {
        self.remove_prefixes.insert(prefix.into());
    }

    fn add_run_key(&mut self, key: impl Into<String>) {
        self.run_keys.insert(key.into());
    }

    fn add_run_card_id(&mut self, card_id: impl Into<String>) {
        self.run_card_ids.insert(card_id.into());
    }

    pub(super) fn merge(&mut self, other: PathDelta) {
        self.exact.extend(other.exact);
        self.remove_prefixes.extend(other.remove_prefixes);
        self.run_keys.extend(other.run_keys);
        self.run_card_ids.extend(other.run_card_ids);
        self.full_snapshot |= other.full_snapshot;
    }
}

pub(super) async fn apply_delta_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &WaveId,
    manifest: &mut TreeManifest,
    delta: PathDelta,
    card_visibility: &CardVisibility,
    object_created_at: i64,
) -> Result<()> {
    for prefix in delta.remove_prefixes {
        manifest
            .entries
            .retain(|path, _| !path.starts_with(prefix.as_str()));
    }
    let mut run_keys = delta.run_keys;
    for card_id in delta.run_card_ids {
        if let Some(key) =
            run_key_for_worker_card_tx(tx, wave_id, &card_id, card_visibility).await?
        {
            run_keys.insert(key);
        }
        if let Some(key) =
            run_key_for_worker_card_in_index_tx(tx, &manifest.entries, &card_id).await?
        {
            run_keys.insert(key);
        }
    }
    if !run_keys.is_empty() {
        apply_run_key_delta_tx(
            tx,
            wave_id,
            &mut manifest.entries,
            run_keys,
            card_visibility,
            object_created_at,
        )
        .await?;
    }
    for path in delta.exact {
        match render_path_tx(tx, wave_id, &path, card_visibility).await? {
            Some(content) => {
                put_rendered_entry(tx, &mut manifest.entries, path, content, object_created_at)
                    .await?;
            }
            None => {
                manifest.entries.remove(&path);
            }
        }
    }
    Ok(())
}

async fn render_path_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &WaveId,
    path: &str,
    card_visibility: &CardVisibility,
) -> Result<Option<BlobContent>> {
    let path = normalize_path(path);
    match path.as_str() {
        "index.md" => {
            let wave = match load_wave_optional_tx(tx, wave_id).await? {
                Some(wave) => wave,
                None => return Ok(None),
            };
            let cards = cards_for_wave_tx(tx, wave_id, card_visibility).await?;
            Ok(Some(index_markdown(&wave, cards.len())))
        }
        "wave.json" => {
            let Some(wave) = load_wave_optional_tx(tx, wave_id).await? else {
                return Ok(None);
            };
            Ok(Some(wave_json(&wave)?))
        }
        "report.md" => {
            let cards = cards_for_wave_tx(tx, wave_id, card_visibility).await?;
            report_markdown(&cards)
        }
        "cards/index.json" => {
            let cards = cards_for_wave_tx(tx, wave_id, card_visibility).await?;
            Ok(Some(cards_index_json(&cards)?))
        }
        path if path.starts_with("cards/") => {
            render_card_path_tx(tx, wave_id, path, card_visibility).await
        }
        "runs/index.json" => {
            let cards = cards_for_wave_tx(tx, wave_id, card_visibility).await?;
            let runs = project_runs_tx(tx, wave_id, &cards).await?;
            Ok(Some(runs_index_json(&runs)?))
        }
        path if path.starts_with("runs/") => {
            let cards = cards_for_wave_tx(tx, wave_id, card_visibility).await?;
            let runs = project_runs_tx(tx, wave_id, &cards).await?;
            render_run_path(path, &runs)
        }
        _ => Ok(None),
    }
}

async fn render_card_path_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &WaveId,
    path: &str,
    card_visibility: &CardVisibility,
) -> Result<Option<BlobContent>> {
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() != 3 {
        return Ok(None);
    }
    let Some(card) = card_in_wave_tx(tx, wave_id, parts[1], card_visibility).await? else {
        return Ok(None);
    };
    match parts[2] {
        ".meta.json" => Ok(Some(card_meta_json(&card)?)),
        ".payload.json" => Ok(Some(card_payload_json(&card)?)),
        "runtime.json" => {
            let mut card = card;
            project_runtime_into_cards_tx(tx, std::slice::from_mut(&mut card)).await?;
            Ok(Some(card_runtime_json(&card.card)?))
        }
        "events.json" => {
            let events = hook_events_for_card_tx(tx, wave_id, &card.card.id).await?;
            Ok(Some(hook_events_json(&events)?))
        }
        "conversation.md" => {
            let events = hook_events_for_card_tx(tx, wave_id, &card.card.id).await?;
            Ok(Some(content_markdown(conversation_markdown(
                &card.card.id,
                &events,
            ))))
        }
        _ => Ok(None),
    }
}

fn render_run_path(path: &str, runs: &[RunProjection]) -> Result<Option<BlobContent>> {
    let run_path = path.trim_start_matches("runs/");
    if let Some(key) = run_path.strip_suffix(".json") {
        return runs
            .iter()
            .find(|run| run.idempotency_key == key)
            .map(run_json)
            .transpose();
    }
    if let Some(key) = run_path.strip_suffix(".md") {
        return Ok(runs
            .iter()
            .find(|run| run.idempotency_key == key)
            .map(|run| content_markdown(run_markdown(run))));
    }
    Ok(None)
}

async fn apply_run_key_delta_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &WaveId,
    entries: &mut BTreeMap<String, ManifestEntry>,
    run_keys: BTreeSet<String>,
    card_visibility: &CardVisibility,
    created_at: i64,
) -> Result<()> {
    let mut index = load_runs_index_map_tx(tx, entries).await?;

    for key in run_keys {
        if wave_fs_view::is_reserved_run_key(&key) {
            // See `insert_run_entries`: VCS skips the reserved-key projection
            // to keep unrelated event writes commit-able, so this pathological
            // state deliberately diverges from live `wave_file` byte parity.
            tracing::error!(
                target: "wave_vcs",
                idempotency_key = %key,
                "runs projection: skipping idempotency_key that collides with reserved path"
            );
            continue;
        }
        index.remove(&key);
        match project_run_by_key_tx(tx, wave_id, &key, card_visibility).await? {
            Some(run) => {
                index.insert(
                    key.clone(),
                    serde_json::to_value(wave_fs_view::run_index_entry(&run))?,
                );
                put_rendered_entry(
                    tx,
                    entries,
                    format!("runs/{key}.json"),
                    run_json(&run)?,
                    created_at,
                )
                .await?;
                put_rendered_entry(
                    tx,
                    entries,
                    format!("runs/{key}.md"),
                    content_markdown(run_markdown(&run)),
                    created_at,
                )
                .await?;
            }
            None => {
                entries.remove(&format!("runs/{key}.json"));
                entries.remove(&format!("runs/{key}.md"));
            }
        }
    }

    let values = index.into_values().collect::<Vec<_>>();
    put_rendered_entry(
        tx,
        entries,
        "runs/index.json",
        content_json(&values)?,
        created_at,
    )
    .await
}

async fn load_runs_index_map_tx(
    tx: &mut Transaction<'_, Sqlite>,
    entries: &BTreeMap<String, ManifestEntry>,
) -> Result<BTreeMap<String, Value>> {
    let Some(hash) = entries
        .get("runs/index.json")
        .map(|entry| entry.blob_hash.clone())
    else {
        return Ok(BTreeMap::new());
    };
    let Some(bytes) = load_blob_bytes_tx(tx, &hash).await? else {
        return Ok(BTreeMap::new());
    };
    let values: Vec<Value> = serde_json::from_slice(&bytes)?;
    let mut index = BTreeMap::new();
    for value in values {
        if let Some(key) = value.get("idempotency_key").and_then(Value::as_str) {
            index.insert(key.to_string(), value);
        }
    }
    Ok(index)
}

async fn run_key_for_worker_card_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &WaveId,
    card_id: &str,
    visibility: &CardVisibility,
) -> Result<Option<String>> {
    let row = sqlx::query(
        r#"SELECT id,
                  json_extract(payload, '$.idempotency_key') AS idempotency_key,
                  EXISTS (
                    SELECT 1
                    FROM events
                    WHERE events.scope_wave = cards.wave_id
                      AND events.kind = 'card.added'
                      AND json_extract(events.payload, '$.id') = cards.id
                  ) AS vcs_announced
           FROM cards
           WHERE id = ?1
             AND wave_id = ?2
             AND role = 'worker'
             AND json_extract(payload, '$.idempotency_key') IS NOT NULL"#,
    )
    .bind(card_id)
    .bind(wave_id.as_str())
    .fetch_optional(&mut **tx)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let id: String = row.try_get("id")?;
    let announced: i64 = row.try_get("vcs_announced")?;
    if visibility.includes(&id, announced != 0) {
        Ok(Some(row.try_get("idempotency_key")?))
    } else {
        Ok(None)
    }
}

async fn run_key_for_worker_card_in_index_tx(
    tx: &mut Transaction<'_, Sqlite>,
    entries: &BTreeMap<String, ManifestEntry>,
    card_id: &str,
) -> Result<Option<String>> {
    let index = load_runs_index_map_tx(tx, entries).await?;
    Ok(index.into_iter().find_map(|(key, value)| {
        value
            .get("worker_card_id")
            .and_then(Value::as_str)
            .filter(|worker_card_id| *worker_card_id == card_id)
            .map(|_| key)
    }))
}

pub(super) fn paths_changed_by_event(event: &Event, wave_id: &WaveId) -> PathDelta {
    let mut delta = PathDelta::default();
    match event {
        Event::WaveUpdated(_) | Event::WaveLifecycleChanged { .. } => {
            delta.add("index.md");
            delta.add("wave.json");
        }
        Event::CardAdded(card) | Event::CardUpdated(card) => {
            add_card_paths(&mut delta, &card.id);
            delta.add("index.md");
            delta.add("cards/index.json");
            delta.add("report.md");
            if let Some(key) = idempotency_key_from_payload(&card.payload) {
                delta.add_run_key(key);
            }
            delta.add_run_card_id(card.id.as_str());
        }
        Event::CardDeleted { id, .. } => {
            delta.remove_prefix(format!("cards/{}/", id.as_str()));
            delta.add("cards/index.json");
            delta.add("index.md");
            delta.add("report.md");
            delta.add_run_card_id(id.as_str());
        }
        Event::RuntimeStarted { card_id, .. }
        | Event::RuntimeStatusChanged { card_id, .. }
        | Event::RuntimeSuperseded { card_id, .. } => {
            add_card_runtime_paths(&mut delta, card_id);
            delta.add_run_card_id(card_id);
        }
        Event::HarnessItemAdded { card_id, .. }
        | Event::HarnessPhaseChanged { card_id, .. }
        | Event::HarnessTranscriptCleared { card_id, .. }
        | Event::HarnessUserMessageEnqueued { card_id, .. } => {
            add_card_runtime_paths(&mut delta, card_id.as_str());
        }
        Event::WaveReportEdited { card_id, .. } => {
            delta.add("report.md");
            add_card_payload_path(&mut delta, card_id.as_str());
        }
        Event::CodexHook { .. } | Event::ClaudeHook { .. } => {}
        Event::CodexWorkerRequested {
            idempotency_key, ..
        }
        | Event::TerminalWorkerRequested {
            idempotency_key, ..
        }
        | Event::TaskCompleted {
            idempotency_key, ..
        }
        | Event::TaskFailed {
            idempotency_key, ..
        }
        // Issue #644 PR-B — the scheduler's claim record is the
        // requested-record fallback for the runs views (§5.6), so it
        // dirties the same run paths a `*.worker_requested` would.
        | Event::TaskDispatched {
            idempotency_key, ..
        } => {
            delta.add_run_key(idempotency_key);
        }
        Event::OverlaySet(overlay) if overlay.entity_kind == "card" => {
            add_card_payload_path(&mut delta, overlay.entity_id.as_str());
        }
        Event::OverlaySet(overlay)
            if overlay.entity_kind == "wave" && overlay.entity_id == wave_id.as_str() =>
        {
            delta.add("wave.json");
        }
        Event::WaveDeleted { .. }
        | Event::CoveUpdated(_)
        | Event::CoveDeleted { .. }
        | Event::OverlaySet(_)
        | Event::OverlayDeleted { .. }
        | Event::TerminalDeleted { .. }
        | Event::PluginState { .. }
        | Event::PluginToolRegistered { .. }
        | Event::WorkflowRegistered { .. } => {}
        // Issue #644 — the task plan has no wave-fs view yet (a
        // `plan/index.json` projection is a stated follow-up, design
        // §4.3); plan revisions therefore change no tracked path.
        Event::PlanUpdated { .. } => {}
        // Issue #644 PR-C (PR #685 F9) — `runs/<key>` renders from the
        // worker cards + [`*.worker_requested`, `task.dispatched`,
        // `task.completed`, `task.failed`] events only
        // (`wave_fs_view::runs_for_wave`); a gate verdict changes no
        // tracked bytes today. Re-add a run-key dirty arm here when the
        // runs projection starts consuming `task.gate_result`.
        Event::TaskGateResult { .. } => {}
        // Issue #760 slice 1: workspace leases are operational history.
        // They are persisted and replayable, but they do not change the
        // wave filesystem projection in this slice.
        Event::WorkspaceLeased { .. } | Event::WorkspaceReleased { .. } => {}
        // Issue #760 slice ③-a: forge/worktree events are operational
        // history for the git/forge toolset substrate. No wave-fs
        // projection consumes them in this pass.
        Event::ForgePrMerged { .. }
        | Event::ReviewRound { .. }
        | Event::RatifyRequested { .. }
        | Event::RatifyResolved { .. }
        | Event::ForgeScanCompleted { .. }
        | Event::ForgePrOpened { .. }
        | Event::ForgePrDiffRead { .. }
        | Event::ForgePrChecks { .. }
        | Event::ForgeIssueRead { .. }
        | Event::ForgeIssueClosed { .. }
        | Event::WorktreeProvisioned { .. }
        | Event::WorktreeCommitted { .. }
        | Event::WorktreeRemoved { .. } => {}
    }
    delta
}

fn add_card_paths(delta: &mut PathDelta, card_id: &CardId) {
    delta.add(format!("cards/{}/.meta.json", card_id.as_str()));
    delta.add(format!("cards/{}/.payload.json", card_id.as_str()));
    delta.add(format!("cards/{}/runtime.json", card_id.as_str()));
    add_card_event_paths(delta, card_id);
}

fn add_card_payload_path(delta: &mut PathDelta, card_id: &str) {
    delta.add(format!("cards/{card_id}/.payload.json"));
}

fn add_card_runtime_paths(delta: &mut PathDelta, card_id: &str) {
    // Post-#618 payload rendering is runtime-independent. Re-rendering this
    // path on runtime events is byte-stable for current-schema blobs, and it
    // heals pre-#618 manifests whose HEAD payload blobs still contain
    // projected runtime fields: the first runtime event rewrites them to raw,
    // producing a one-time `edited` entry.
    delta.add(format!("cards/{card_id}/.payload.json"));
    delta.add(format!("cards/{card_id}/runtime.json"));
}

pub(super) fn add_card_event_paths(delta: &mut PathDelta, card_id: &CardId) {
    delta.add(format!("cards/{}/events.json", card_id.as_str()));
    delta.add(format!("cards/{}/conversation.md", card_id.as_str()));
}
