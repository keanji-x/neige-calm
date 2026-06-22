//! SQLite-backed wave VCS snapshots.
//!
//! Two-phase spawn invariant (#310): dispatcher spawn first creates rows in an
//! event-less transaction, then later emits `CardAdded` through
//! `RepoEventWrite::log_pure_event`. Wave VCS commits anchor on persisted
//! events, not on raw rows, so the in-between row is invisible here just as it
//! is invisible to subscribers. Replay re-emits events through the same trait
//! methods, so commits regenerate as a side effect; there is no separate replay
//! path for wave-vcs.
//!
//! Commit hashes include the commit `created_at` timestamp. The tree hash is
//! the deterministic content anchor; replaying the same logical wave state can
//! reproduce the same tree hash without necessarily reproducing the same commit
//! hash. Fixture paths that seed events with `EventScope::System` also do not
//! generate wave-vcs commits because they are outside any wave scope.

use crate::db::WaveEvent;
use crate::db::sqlite::begin_immediate_tx;
use crate::error::{CalmError, Result};
use crate::event::{Event, EventScope};
use crate::ids::{ActorId, CardId, WaveId};
use crate::model::{Card, CardRole, Wave, now_ms};
use crate::session_projection_lookup;
use crate::session_projection_row::{
    projectable_runtimes_for_cards_from_rows, projectable_runtimes_for_cards_query,
};
use crate::wave_fs_dto::WaveFsRunStatus;
use crate::wave_fs_view::{
    self, HookEventProjection, RunEventProjection, RunProjection, RunVerdictProjection,
};
use crate::wave_report::WaveReportPayload;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use similar::TextDiff;
use sqlx::sqlite::SqliteRow;
use sqlx::{QueryBuilder, Row, Sqlite, SqlitePool, Transaction};
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

pub const MANIFEST_SCHEMA_VERSION: i64 = 1;
pub const DEFAULT_PATCH_MAX_LINES: usize = 200;
const LOG_FILTER_SCAN_LIMIT: usize = 1000;
const OBJECT_SWEEP_INTERVAL: Duration = Duration::from_secs(60 * 60);
const OBJECT_SWEEP_GRACE_MS: i64 = 60 * 60 * 1000;
const WAVE_HISTORY_PRUNE_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
// Keep aligned with the existing `neige wave-gc` default.
const DEFAULT_WAVE_HISTORY_PRUNE_KEEP: usize = 50;
const WAVE_HISTORY_PRUNE_INTERVAL_SECS_ENV: &str = "NEIGE_WAVE_PRUNE_INTERVAL_SECS";
const WAVE_HISTORY_PRUNE_KEEP_ENV: &str = "NEIGE_WAVE_PRUNE_KEEP";
const ATTRIBUTION_COMMIT_BOUND: usize = 50;

pub type ObjectHash = String;
pub type CommitHash = String;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreeManifest {
    pub schema_version: i64,
    pub entries: BTreeMap<String, ManifestEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestEntry {
    pub blob_hash: ObjectHash,
    pub byte_len: u64,
    pub content_type: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeSnapshot {
    pub tree_hash: ObjectHash,
    pub manifest: TreeManifest,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitRecord {
    pub hash: CommitHash,
    pub wave_id: WaveId,
    pub parent_hash: Option<CommitHash>,
    pub tree_hash: ObjectHash,
    pub manifest_schema_version: i64,
    pub lifecycle: String,
    pub event_id: Option<i64>,
    pub created_at: i64,
    pub message: Option<String>,
    pub author: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffEntry {
    pub path: String,
    pub status: DiffStatus,
    pub old_hash: Option<ObjectHash>,
    pub new_hash: Option<ObjectHash>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffStatus {
    Added,
    Deleted,
    Modified,
}

impl DiffStatus {
    pub fn wire_label(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Deleted => "deleted",
            Self::Modified => "modified",
        }
    }

    fn observation_label(self) -> &'static str {
        match self {
            Self::Added => "new",
            Self::Deleted => "deleted",
            Self::Modified => "edited",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileDiff {
    pub path: String,
    pub status: DiffStatus,
    pub old_hash: Option<ObjectHash>,
    pub new_hash: Option<ObjectHash>,
    pub old_content_type: Option<String>,
    pub new_content_type: Option<String>,
    pub patch: Option<String>,
    pub patch_truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoricalBlob {
    pub commit: CommitHash,
    pub path: String,
    pub content: String,
    pub content_type: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitLogEntry {
    pub hash: CommitHash,
    pub parent_hash: Option<CommitHash>,
    pub lifecycle: String,
    pub event_id: Option<i64>,
    pub created_at: i64,
    pub message: Option<String>,
    pub changed_paths: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CommitLog {
    pub commits: Vec<CommitLogEntry>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SinceLastTurnBlock {
    pub current_head: Option<CommitHash>,
    pub block: Option<String>,
}

impl SinceLastTurnBlock {
    pub fn empty() -> Self {
        Self::default()
    }
}

#[derive(Clone, Debug)]
struct BlobContent {
    bytes: Vec<u8>,
    content_type: String,
}

#[derive(Clone, Debug)]
struct CardProjection {
    card: Card,
    role: String,
}

enum CardVisibility {
    AnnouncedOrInherited(BTreeSet<String>),
    AllRows,
}

impl CardVisibility {
    fn announced_only() -> Self {
        Self::AnnouncedOrInherited(BTreeSet::new())
    }

    fn from_manifest(manifest: &TreeManifest) -> Self {
        Self::AnnouncedOrInherited(visible_card_ids_from_manifest(manifest))
    }

    fn includes(&self, card_id: &str, announced: bool) -> bool {
        match self {
            Self::AnnouncedOrInherited(inherited) => announced || inherited.contains(card_id),
            Self::AllRows => true,
        }
    }
}

fn visible_card_ids_from_manifest(manifest: &TreeManifest) -> BTreeSet<String> {
    manifest
        .entries
        .keys()
        .filter_map(|path| {
            card_id_from_card_lens_path(path, ".meta.json")
                .or_else(|| card_id_from_card_lens_path(path, "meta.json"))
                .map(ToOwned::to_owned)
        })
        .collect()
}

fn card_id_from_card_lens_path<'a>(path: &'a str, leaf: &str) -> Option<&'a str> {
    path.strip_prefix("cards/")
        .and_then(|path| path.strip_suffix(leaf))
        .and_then(|path| path.strip_suffix('/'))
        .filter(|card_id| !card_id.contains('/'))
}

fn is_legacy_card_lens_path(path: &str) -> bool {
    card_id_from_card_lens_path(path, "meta.json").is_some()
        || card_id_from_card_lens_path(path, "payload.json").is_some()
}

fn has_legacy_card_lens_paths(manifest: &TreeManifest) -> bool {
    manifest
        .entries
        .keys()
        .any(|path| is_legacy_card_lens_path(path))
}

#[derive(Default)]
struct PathDelta {
    exact: BTreeSet<String>,
    remove_prefixes: BTreeSet<String>,
    run_keys: BTreeSet<String>,
    run_card_ids: BTreeSet<String>,
    /// Safety valve for future schema-wide projection changes. The current
    /// event set is intentionally handled as path-level deltas.
    full_snapshot: bool,
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

    fn merge(&mut self, other: PathDelta) {
        self.exact.extend(other.exact);
        self.remove_prefixes.extend(other.remove_prefixes);
        self.run_keys.extend(other.run_keys);
        self.run_card_ids.extend(other.run_card_ids);
        self.full_snapshot |= other.full_snapshot;
    }
}

pub async fn backfill_existing_waves(pool: &SqlitePool) -> Result<usize> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let res = backfill_existing_waves_tx(&mut tx).await;
    match res {
        Ok(count) => {
            tx.commit().await?;
            Ok(count)
        }
        Err(e) => {
            let _ = tx.rollback().await;
            Err(e)
        }
    }
}

async fn backfill_existing_waves_tx(tx: &mut Transaction<'_, Sqlite>) -> Result<usize> {
    let waves: Vec<Wave> = sqlx::query_as::<_, crate::db::rows::WaveRow>(
        r#"SELECT id, cove_id, title, sort, archived_at, pinned_at, lifecycle, cwd,
                  workflow_id, terminal_at, created_at, updated_at
           FROM waves
           WHERE id NOT IN (SELECT wave_id FROM wave_vcs_refs)
           ORDER BY created_at ASC, id ASC"#,
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(Wave::from)
    .collect();

    let mut count = 0usize;
    for wave in waves {
        let latest = latest_wave_event_tx(tx, &wave.id).await?;
        let created_at = latest.map(|(_, at)| at).unwrap_or(wave.updated_at);
        let event_id = latest.map(|(id, _)| id);
        let tree = snapshot_tree_at_tx(
            tx,
            &wave.id,
            MANIFEST_SCHEMA_VERSION,
            created_at,
            &Some(wave.clone()),
            &CardVisibility::AllRows,
        )
        .await?;
        commit_tree_at_tx(
            tx,
            &wave.id,
            &tree,
            CommitTreeMeta {
                parent_hash: None,
                author: None,
                event_id,
                message: "backfill root",
                manifest_schema_version: MANIFEST_SCHEMA_VERSION,
                created_at,
            },
        )
        .await?;
        count += 1;
    }

    Ok(count)
}

pub async fn snapshot_tree(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &WaveId,
    schema_version: i64,
) -> Result<TreeSnapshot> {
    snapshot_tree_at_tx(
        tx,
        wave_id,
        schema_version,
        now_ms(),
        &None,
        &CardVisibility::announced_only(),
    )
    .await
}

async fn snapshot_tree_at_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &WaveId,
    schema_version: i64,
    object_created_at: i64,
    prefetched_wave: &Option<Wave>,
    card_visibility: &CardVisibility,
) -> Result<TreeSnapshot> {
    let wave = match prefetched_wave {
        Some(wave) => wave.clone(),
        None => load_wave_tx(tx, wave_id).await?,
    };
    let cards = cards_for_wave_tx(tx, wave_id, card_visibility).await?;
    let mut runtime_projected_cards = cards.clone();
    project_runtime_into_cards_tx(tx, &mut runtime_projected_cards).await?;
    let runs = project_runs_tx(tx, wave_id, &cards).await?;
    let mut entries = BTreeMap::new();

    put_rendered_entry(
        tx,
        &mut entries,
        "index.md",
        index_markdown(&wave, cards.len()),
        object_created_at,
    )
    .await?;
    put_rendered_entry(
        tx,
        &mut entries,
        "wave.json",
        wave_json(&wave)?,
        object_created_at,
    )
    .await?;
    if let Some(report) = report_markdown(&cards)? {
        put_rendered_entry(tx, &mut entries, "report.md", report, object_created_at).await?;
    }
    put_rendered_entry(
        tx,
        &mut entries,
        "cards/index.json",
        cards_index_json(&cards)?,
        object_created_at,
    )
    .await?;

    for (card, runtime_projected_card) in cards.iter().zip(runtime_projected_cards.iter()) {
        let card_id = card.card.id.as_str();
        put_rendered_entry(
            tx,
            &mut entries,
            format!("cards/{card_id}/.meta.json"),
            card_meta_json(card)?,
            object_created_at,
        )
        .await?;
        put_rendered_entry(
            tx,
            &mut entries,
            format!("cards/{card_id}/.payload.json"),
            card_payload_json(card)?,
            object_created_at,
        )
        .await?;
        put_rendered_entry(
            tx,
            &mut entries,
            format!("cards/{card_id}/runtime.json"),
            card_runtime_json(&runtime_projected_card.card)?,
            object_created_at,
        )
        .await?;
        let hook_events = hook_events_for_card_tx(tx, wave_id, &card.card.id).await?;
        put_rendered_entry(
            tx,
            &mut entries,
            format!("cards/{card_id}/events.json"),
            hook_events_json(&hook_events)?,
            object_created_at,
        )
        .await?;
        put_rendered_entry(
            tx,
            &mut entries,
            format!("cards/{card_id}/conversation.md"),
            content_markdown(conversation_markdown(&card.card.id, &hook_events)),
            object_created_at,
        )
        .await?;
    }

    insert_run_entries(tx, &mut entries, &runs, object_created_at).await?;
    store_tree(tx, schema_version, entries, object_created_at).await
}

pub async fn put_blob(
    tx: &mut Transaction<'_, Sqlite>,
    kind: &str,
    bytes: &[u8],
) -> Result<ObjectHash> {
    put_object_at_tx(tx, kind, bytes, now_ms()).await
}

pub async fn commit_tree(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &WaveId,
    parent_hash: Option<&str>,
    tree: &TreeSnapshot,
    event_id: Option<i64>,
    message: &str,
    manifest_schema_version: i64,
) -> Result<CommitHash> {
    commit_tree_at_tx(
        tx,
        wave_id,
        tree,
        CommitTreeMeta {
            parent_hash,
            author: None,
            event_id,
            message,
            manifest_schema_version,
            created_at: now_ms(),
        },
    )
    .await
}

pub async fn commit_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &WaveId,
    actor: &ActorId,
    event_id: i64,
    event: &Event,
    manifest_schema_version: i64,
) -> Result<Option<CommitHash>> {
    commit_events_in_tx(
        tx,
        wave_id,
        actor,
        event_id,
        std::slice::from_ref(event),
        manifest_schema_version,
    )
    .await
}

pub async fn commit_events_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &WaveId,
    actor: &ActorId,
    event_id: i64,
    events: &[Event],
    manifest_schema_version: i64,
) -> Result<Option<CommitHash>> {
    commit_events_with_author_in_tx(
        tx,
        wave_id,
        Some(actor),
        event_id,
        events,
        manifest_schema_version,
    )
    .await
}

pub async fn commit_events_with_author_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &WaveId,
    author: Option<&ActorId>,
    event_id: i64,
    events: &[Event],
    manifest_schema_version: i64,
) -> Result<Option<CommitHash>> {
    if events
        .iter()
        .any(|event| matches!(event, Event::WaveDeleted { .. }))
    {
        return Ok(None);
    }
    if events.iter().all(|event| {
        matches!(
            event,
            Event::WorkspaceLeased { .. }
                | Event::WorkspaceReleased { .. }
                | Event::ForgePrMerged { .. }
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
                | Event::WorktreeRemoved { .. }
        )
    }) {
        return Ok(None);
    }

    let now = now_ms();
    let mut delta = PathDelta::default();
    for event in events {
        delta.merge(paths_changed_by_event(event, wave_id));
    }
    let author = author.map(ToString::to_string);
    commit_delta_in_tx(
        tx,
        wave_id,
        delta,
        CommitTreeMeta {
            parent_hash: None,
            author: author.as_deref(),
            event_id: Some(event_id),
            message: events.last().map(Event::kind_tag).unwrap_or("event"),
            manifest_schema_version,
            created_at: now,
        },
    )
    .await
    .map(Some)
}

pub async fn snapshot_transcripts_for_cards_in_wave(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &WaveId,
    event_id: Option<i64>,
    manifest_schema_version: i64,
) -> Result<CommitHash> {
    let card_visibility = match head_in_tx(tx, wave_id).await? {
        Some(parent) => {
            let parent_manifest = tree_at_in_tx(tx, &parent).await?.ok_or_else(|| {
                CalmError::Internal(format!("wave-vcs: missing tree for {parent}"))
            })?;
            CardVisibility::from_manifest(&parent_manifest)
        }
        None => CardVisibility::announced_only(),
    };
    let cards = cards_for_wave_tx(tx, wave_id, &card_visibility).await?;
    let mut delta = PathDelta::default();
    for card in cards {
        add_card_event_paths(&mut delta, &card.card.id);
    }

    let author = ActorId::Kernel.to_string();
    commit_delta_in_tx(
        tx,
        wave_id,
        delta,
        CommitTreeMeta {
            parent_hash: None,
            author: Some(author.as_str()),
            event_id,
            message: "transcript refresh",
            manifest_schema_version,
            created_at: now_ms(),
        },
    )
    .await
}

async fn commit_delta_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &WaveId,
    delta: PathDelta,
    meta: CommitTreeMeta<'_>,
) -> Result<CommitHash> {
    let parent_hash = head_in_tx(tx, wave_id).await?;
    let tree = if let Some(parent) = parent_hash.as_deref() {
        let mut parent_manifest = tree_at_in_tx(tx, parent)
            .await?
            .ok_or_else(|| CalmError::Internal(format!("wave-vcs: missing tree for {parent}")))?;
        let card_visibility = CardVisibility::from_manifest(&parent_manifest);
        if delta.full_snapshot || has_legacy_card_lens_paths(&parent_manifest) {
            snapshot_tree_at_tx(
                tx,
                wave_id,
                meta.manifest_schema_version,
                meta.created_at,
                &None,
                &card_visibility,
            )
            .await?
        } else {
            apply_delta_tx(
                tx,
                wave_id,
                &mut parent_manifest,
                delta,
                &card_visibility,
                meta.created_at,
            )
            .await?;
            store_tree(
                tx,
                meta.manifest_schema_version,
                parent_manifest.entries,
                meta.created_at,
            )
            .await?
        }
    } else {
        snapshot_tree_at_tx(
            tx,
            wave_id,
            meta.manifest_schema_version,
            meta.created_at,
            &None,
            &CardVisibility::announced_only(),
        )
        .await?
    };

    commit_tree_at_tx(
        tx,
        wave_id,
        &tree,
        CommitTreeMeta {
            parent_hash: parent_hash.as_deref(),
            ..meta
        },
    )
    .await
}

pub async fn head(pool: &SqlitePool, wave_id: &WaveId) -> Result<Option<CommitHash>> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT head_hash FROM wave_vcs_refs WHERE wave_id = ?1")
            .bind(wave_id.as_str())
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|(hash,)| hash))
}

pub async fn tree_at(pool: &SqlitePool, commit_hash: &str) -> Result<Option<TreeManifest>> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT tree_hash FROM wave_vcs_commits WHERE hash = ?1")
            .bind(commit_hash)
            .fetch_optional(pool)
            .await?;
    let Some((tree_hash,)) = row else {
        return Ok(None);
    };
    load_tree_object_pool(pool, &tree_hash).await
}

pub async fn diff(
    pool: &SqlitePool,
    from: &str,
    to: &str,
    path: Option<&str>,
) -> Result<Vec<DiffEntry>> {
    let from_tree = tree_at(pool, from)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("wave-vcs commit {from}")))?;
    let to_tree = tree_at(pool, to)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("wave-vcs commit {to}")))?;
    Ok(diff_manifests(&from_tree, &to_tree, path))
}

pub async fn diff_with_patches(
    pool: &SqlitePool,
    from: &str,
    to: &str,
    path: Option<&str>,
    max_patch_lines: usize,
) -> Result<Vec<FileDiff>> {
    let from_tree = tree_at(pool, from)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("wave-vcs commit {from}")))?;
    let to_tree = tree_at(pool, to)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("wave-vcs commit {to}")))?;
    let entries = diff_manifests(&from_tree, &to_tree, path);
    file_diffs_from_entries(pool, &from_tree, &to_tree, entries, max_patch_lines).await
}

pub async fn cat_at(pool: &SqlitePool, commit_hash: &str, path: &str) -> Result<HistoricalBlob> {
    let tree = tree_at(pool, commit_hash)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("wave-vcs commit {commit_hash}")))?;
    let path = normalize_path(path);
    let entry = tree
        .entries
        .get(&path)
        .ok_or_else(|| CalmError::NotFound(format!("wave-vcs path {path} at {commit_hash}")))?;
    let bytes = load_blob_bytes_pool(pool, &entry.blob_hash)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("wave-vcs blob {}", entry.blob_hash)))?;
    let content = String::from_utf8(bytes).map_err(|e| {
        CalmError::Internal(format!(
            "wave-vcs: blob {} at {commit_hash}:{path} is not UTF-8: {e}",
            entry.blob_hash
        ))
    })?;
    Ok(HistoricalBlob {
        commit: commit_hash.to_string(),
        path,
        content,
        content_type: entry.content_type.clone(),
    })
}

pub async fn commit_record(pool: &SqlitePool, commit_hash: &str) -> Result<Option<CommitRecord>> {
    load_commit_record_pool(pool, commit_hash).await
}

pub async fn commit_belongs_to_wave(
    pool: &SqlitePool,
    wave_id: &WaveId,
    commit_hash: &str,
) -> Result<bool> {
    let Some(record) = commit_record(pool, commit_hash).await? else {
        return Ok(false);
    };
    Ok(record.wave_id == *wave_id)
}

pub async fn log(
    pool: &SqlitePool,
    wave_id: &WaveId,
    path: Option<&str>,
    limit: usize,
) -> Result<CommitLog> {
    let limit = limit.clamp(1, 200);
    let normalized = path.map(normalize_path).filter(|path| !path.is_empty());
    let scan_limit = if normalized.is_some() {
        LOG_FILTER_SCAN_LIMIT
    } else {
        limit
    };
    let records = commit_records_for_wave_pool(pool, wave_id, scan_limit.saturating_add(1)).await?;
    let fetched = records.len();
    let mut out = Vec::new();
    let mut examined = 0;
    for record in records.into_iter().take(scan_limit) {
        examined += 1;
        let changed_paths = changed_paths_for_commit(pool, &record).await?;
        if let Some(path) = normalized.as_deref()
            && !changed_paths
                .iter()
                .any(|changed| path_matches(changed, path))
        {
            continue;
        }
        out.push(CommitLogEntry {
            hash: record.hash,
            parent_hash: record.parent_hash,
            lifecycle: record.lifecycle,
            event_id: record.event_id,
            created_at: record.created_at,
            message: record.message,
            changed_paths,
        });
        if out.len() >= limit {
            break;
        }
    }
    Ok(CommitLog {
        commits: out,
        truncated: examined < fetched,
    })
}

pub async fn since_last_turn_block(
    pool: &SqlitePool,
    wave_id: &WaveId,
    last_seen_head: Option<&str>,
    current_override: Option<&CommitHash>,
    spec_card_id: Option<&CardId>,
) -> Result<SinceLastTurnBlock> {
    let Some(current) = (match current_override {
        Some(current) => Some(current.clone()),
        None => head(pool, wave_id).await?,
    }) else {
        return Ok(SinceLastTurnBlock::empty());
    };
    let Some(previous) = last_seen_head else {
        return Ok(SinceLastTurnBlock {
            current_head: Some(current),
            block: None,
        });
    };
    if previous == current {
        return Ok(SinceLastTurnBlock {
            current_head: Some(current),
            block: None,
        });
    }

    let entries = diff(pool, previous, &current, None)
        .await?
        .into_iter()
        .filter(|entry| !is_internal_observation_diff_path(&entry.path, spec_card_id))
        .collect::<Vec<_>>();
    if entries.is_empty() {
        return Ok(SinceLastTurnBlock {
            current_head: Some(current),
            block: None,
        });
    }
    let report_patch = if entries.iter().any(|entry| {
        entry.path == "report.md"
            && matches!(entry.status, DiffStatus::Added | DiffStatus::Modified)
    }) {
        diff_with_patches(
            pool,
            previous,
            &current,
            Some("report.md"),
            DEFAULT_PATCH_MAX_LINES,
        )
        .await?
        .into_iter()
        .find_map(|entry| entry.patch)
    } else {
        None
    };
    let path_authors = path_authors_since(pool, previous, &current).await?;
    let mut out = String::new();
    out.push_str(&format!(
        "## Wave state changes since your last turn (HEAD {} -> {})\n",
        short_hash(previous),
        short_hash(&current)
    ));
    for entry in entries {
        out.push_str("- ");
        out.push_str(&entry.path);
        out.push(' ');
        out.push_str(entry.status.observation_label());
        if let Some(author) = path_authors.as_ref().and_then(|authors| {
            authors
                .get(&entry.path)
                .and_then(|author| author.as_deref())
        }) {
            out.push_str(" (by ");
            out.push_str(author);
            out.push(')');
        }
        if entry.path == "report.md" && report_patch.is_some() {
            out.push_str(" (unified patch follows)");
        }
        out.push('\n');
        if entry.path == "report.md"
            && let Some(patch) = report_patch.as_deref()
        {
            let fence = markdown_code_fence_for(patch);
            out.push_str(&fence);
            out.push_str("diff\n");
            out.push_str(patch);
            if !patch.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(&fence);
            out.push('\n');
        }
    }
    Ok(SinceLastTurnBlock {
        current_head: Some(current),
        block: Some(out),
    })
}

/// Prune old linear wave history while preserving every commit an active
/// harness may still need for endpoint-only `diff(previous_endpoint, HEAD)`.
///
/// The grace floor is the minimum `created_at` among all protected commits,
/// not a maximum. Keeping every commit at or after the oldest protected
/// endpoint is intentionally conservative: it preserves the contiguous suffix
/// anchored at HEAD and errs toward keeping more history instead of deleting a
/// commit an active session may still reference.
pub async fn prune_wave_history_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &WaveId,
    keep: usize,
) -> Result<u64> {
    let Some(head_hash) = head_in_tx(tx, wave_id).await? else {
        return Ok(0);
    };

    let keep = keep.max(1);
    let mut protected = BTreeMap::<CommitHash, i64>::new();
    let mut cursor = Some(head_hash);
    for _ in 0..keep {
        let Some(hash) = cursor else {
            break;
        };
        let Some(record) = load_commit_record_for_wave_tx(tx, wave_id, &hash).await? else {
            if protected.is_empty() {
                tracing::warn!(
                    target: "wave_vcs",
                    wave_id = %wave_id.as_str(),
                    commit_hash = %hash,
                    "wave-vcs prune: HEAD ref points at a missing commit; skipping prune"
                );
                return Ok(0);
            }
            break;
        };
        cursor = record.parent_hash.clone();
        let inserted = protected
            .insert(record.hash.clone(), record.created_at)
            .is_none();
        if !inserted {
            break;
        }
    }

    let active_endpoints = match active_diff_endpoint_commits_tx(tx, wave_id).await? {
        ActiveLastSeenHeads::Safe(records) => records,
        ActiveLastSeenHeads::SkipPrune => return Ok(0),
    };
    for record in active_endpoints {
        protected.insert(record.hash, record.created_at);
    }

    let Some(floor) = protected.values().min().copied() else {
        return Ok(0);
    };

    sqlx::query(
        r#"CREATE TEMP TABLE IF NOT EXISTS wave_vcs_prune_keep (
               hash TEXT PRIMARY KEY
           )"#,
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query("DELETE FROM wave_vcs_prune_keep")
        .execute(&mut **tx)
        .await?;
    insert_prune_keep_refs_tx(tx, protected.keys()).await?;

    let result = sqlx::query(
        r#"DELETE FROM wave_vcs_commits
           WHERE wave_id = ?1
             AND created_at < ?2
             AND NOT EXISTS (
               SELECT 1
               FROM wave_vcs_prune_keep AS keep
               WHERE keep.hash = wave_vcs_commits.hash
             )"#,
    )
    .bind(wave_id.as_str())
    .bind(floor)
    .execute(&mut **tx)
    .await?;
    Ok(result.rows_affected())
}

/// Spawn the wave-history pruner. It runs the same keep-N prune used by the
/// manual admin GC path, across all waves, without running VACUUM.
pub fn spawn_wave_history_pruner(pool: SqlitePool) {
    let Some((interval, keep)) = wave_history_pruner_config_from_env() else {
        tracing::info!("wave_vcs: history pruner disabled");
        return;
    };

    tokio::spawn(async move {
        let mut tick = tokio::time::interval(interval);
        // Match the object sweeper: skip the immediate boot tick and let the
        // server settle before taking SQLite writer locks for cleanup.
        tick.tick().await;
        loop {
            tick.tick().await;
            if let Err(e) = prune_all_waves_once(&pool, keep).await {
                tracing::warn!(error = %e, "wave_vcs: history prune failed");
            }
        }
    });
}

fn wave_history_pruner_config_from_env() -> Option<(Duration, usize)> {
    let interval = match std::env::var(WAVE_HISTORY_PRUNE_INTERVAL_SECS_ENV) {
        Ok(raw) => match raw.trim().parse::<u64>() {
            Ok(0) => return None,
            Ok(secs) => Duration::from_secs(secs),
            Err(_) => WAVE_HISTORY_PRUNE_INTERVAL,
        },
        Err(_) => WAVE_HISTORY_PRUNE_INTERVAL,
    };
    let keep = match std::env::var(WAVE_HISTORY_PRUNE_KEEP_ENV) {
        Ok(raw) => match raw.trim().parse::<usize>() {
            Ok(n) if n > 0 => n,
            _ => DEFAULT_WAVE_HISTORY_PRUNE_KEEP,
        },
        Err(_) => DEFAULT_WAVE_HISTORY_PRUNE_KEEP,
    };
    Some((interval, keep))
}

/// One all-wave history prune pass. Public so integration tests can drive
/// cleanup deterministically without waiting for the scheduled task.
pub async fn prune_all_waves_once(pool: &SqlitePool, keep: usize) -> Result<u64> {
    let wave_ids: Vec<String> = sqlx::query_scalar("SELECT id FROM waves ORDER BY id")
        .fetch_all(pool)
        .await?;
    let wave_count = wave_ids.len();
    let mut total_pruned = 0;

    for wave_id in wave_ids {
        let wave_id = WaveId::from(wave_id);
        let mut tx = match begin_immediate_tx(pool).await {
            Ok(tx) => tx,
            Err(e) => {
                tracing::warn!(
                    wave_id = %wave_id.as_str(),
                    error = %e,
                    "wave_vcs: prune failed for wave; continuing"
                );
                continue;
            }
        };
        let res = prune_wave_history_tx(&mut tx, &wave_id, keep).await;
        match res {
            Ok(pruned) => {
                if let Err(e) = tx.commit().await {
                    tracing::warn!(
                        wave_id = %wave_id.as_str(),
                        error = %e,
                        "wave_vcs: prune failed for wave; continuing"
                    );
                } else {
                    total_pruned += pruned;
                }
            }
            Err(e) => {
                let _ = tx.rollback().await;
                tracing::warn!(
                    wave_id = %wave_id.as_str(),
                    error = %e,
                    "wave_vcs: prune failed for wave; continuing"
                );
            }
        }
    }

    if total_pruned > 0 {
        tracing::info!(
            pruned = total_pruned,
            waves = wave_count,
            keep,
            "wave_vcs: pruned wave history"
        );
    }
    Ok(total_pruned)
}

/// Spawn the unreferenced-object sweeper. Content-addressed objects are not
/// deleted by `wave_delete_tx` / `cove_delete_tx` because blobs can be shared
/// across waves; this hourly fallback reclaims rows no live commit references.
pub fn spawn_unreferenced_object_sweeper(pool: SqlitePool) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(OBJECT_SWEEP_INTERVAL);
        // Match the terminal sweeper: skip the immediate boot tick and let the
        // server settle before taking the SQLite writer lock for cleanup.
        tick.tick().await;
        loop {
            tick.tick().await;
            if let Err(e) = sweep_unreferenced_objects_once(&pool).await {
                tracing::warn!(error = %e, "wave_vcs: object sweep failed");
            }
        }
    });
}

/// One unreferenced-object sweep pass. Public so integration tests can drive
/// cleanup deterministically without waiting for the hourly task.
///
/// This deliberately performs one `O(commits)` scan inside the writer
/// transaction to seed live tree refs. It is single-writer fallback GC; revisit
/// with streaming/snapshot+reverify if commit counts grow.
pub async fn sweep_unreferenced_objects_once(pool: &SqlitePool) -> Result<u64> {
    let cutoff_ms = now_ms().saturating_sub(OBJECT_SWEEP_GRACE_MS);
    let mut tx = begin_immediate_tx(pool).await?;
    let res = sweep_unreferenced_objects_tx(&mut tx, cutoff_ms).await;
    match res {
        Ok(deleted) => {
            tx.commit().await?;
            if deleted > 0 {
                tracing::info!(deleted, "wave_vcs: swept unreferenced objects");
            }
            Ok(deleted)
        }
        Err(e) => {
            let _ = tx.rollback().await;
            Err(e)
        }
    }
}

async fn sweep_unreferenced_objects_tx(
    tx: &mut Transaction<'_, Sqlite>,
    cutoff_ms: i64,
) -> Result<u64> {
    sqlx::query(
        r#"CREATE TEMP TABLE IF NOT EXISTS wave_vcs_sweep_refs (
               hash TEXT PRIMARY KEY
           )"#,
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query("DELETE FROM wave_vcs_sweep_refs")
        .execute(&mut **tx)
        .await?;
    // Re-rooted on HEAD refs (#722 B.2). Tree-rooted
    // `SELECT DISTINCT tree_hash` would keep trees of pruned-but-not-yet-swept
    // commit rows alive across a partial prune; ref-rooting matches the
    // prune's reachability so orphaned objects are actually reclaimed.
    sqlx::query(
        r#"INSERT OR IGNORE INTO wave_vcs_sweep_refs(hash)
           SELECT DISTINCT c.tree_hash
           FROM wave_vcs_commits AS c
           WHERE c.hash IN (
               WITH RECURSIVE reachable(hash) AS (
                   SELECT head_hash FROM wave_vcs_refs
                   UNION
                   SELECT c2.parent_hash
                   FROM wave_vcs_commits AS c2
                   JOIN reachable AS r ON c2.hash = r.hash
                   WHERE c2.parent_hash IS NOT NULL
               )
               SELECT hash FROM reachable
           )"#,
    )
    .execute(&mut **tx)
    .await?;

    let tree_rows: Vec<(String, Vec<u8>)> = sqlx::query_as(
        r#"SELECT refs.hash, o.bytes
           FROM wave_vcs_sweep_refs AS refs
           JOIN wave_vcs_objects AS o ON o.hash = refs.hash
           WHERE o.kind = 'tree'"#,
    )
    .fetch_all(&mut **tx)
    .await?;

    let mut blob_hashes = BTreeSet::new();
    for (tree_hash, bytes) in tree_rows {
        let manifest: TreeManifest = serde_json::from_slice(&bytes).map_err(|e| {
            CalmError::Internal(format!(
                "wave_vcs: parse tree manifest object {tree_hash}: {e}"
            ))
        })?;
        blob_hashes.extend(manifest.entries.into_values().map(|entry| entry.blob_hash));
    }
    insert_sweep_refs_tx(tx, blob_hashes).await?;

    let result = sqlx::query(
        r#"DELETE FROM wave_vcs_objects
           WHERE created_at < ?1
             AND NOT EXISTS (
               SELECT 1
               FROM wave_vcs_sweep_refs AS refs
               WHERE refs.hash = wave_vcs_objects.hash
             )"#,
    )
    .bind(cutoff_ms)
    .execute(&mut **tx)
    .await?;
    Ok(result.rows_affected())
}

async fn insert_sweep_refs_tx(
    tx: &mut Transaction<'_, Sqlite>,
    hashes: BTreeSet<ObjectHash>,
) -> Result<()> {
    const INSERT_CHUNK_SIZE: usize = 500;
    let hashes = hashes.into_iter().collect::<Vec<_>>();
    for chunk in hashes.chunks(INSERT_CHUNK_SIZE) {
        if chunk.is_empty() {
            continue;
        }
        let mut builder: QueryBuilder<'_, Sqlite> =
            QueryBuilder::new("INSERT OR IGNORE INTO wave_vcs_sweep_refs(hash) ");
        builder.push_values(chunk, |mut row, hash| {
            row.push_bind(hash);
        });
        builder.build().execute(&mut **tx).await?;
    }
    Ok(())
}

async fn insert_prune_keep_refs_tx<'a, I>(tx: &mut Transaction<'_, Sqlite>, hashes: I) -> Result<()>
where
    I: IntoIterator<Item = &'a CommitHash>,
{
    const INSERT_CHUNK_SIZE: usize = 500;
    let hashes = hashes.into_iter().collect::<Vec<_>>();
    for chunk in hashes.chunks(INSERT_CHUNK_SIZE) {
        if chunk.is_empty() {
            continue;
        }
        let mut builder: QueryBuilder<'_, Sqlite> =
            QueryBuilder::new("INSERT OR IGNORE INTO wave_vcs_prune_keep(hash) ");
        builder.push_values(chunk, |mut row, hash| {
            row.push_bind(*hash);
        });
        builder.build().execute(&mut **tx).await?;
    }
    Ok(())
}

enum ActiveLastSeenHeads {
    Safe(Vec<CommitRecord>),
    SkipPrune,
}

async fn active_diff_endpoint_commits_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &WaveId,
) -> Result<ActiveLastSeenHeads> {
    let rows = sqlx::query(
        r#"SELECT id, handle_state_json
           FROM worker_sessions
           WHERE wave_id = ?1
             AND state IN ('starting', 'running', 'idle', 'turn_pending')
           ORDER BY created_at_ms ASC, id ASC"#,
    )
    .bind(wave_id.as_str())
    .fetch_all(&mut **tx)
    .await?;

    let mut records = Vec::new();
    for row in rows {
        let session_id: String = row.try_get("id")?;
        let Some(state_json) = row.try_get::<Option<String>, _>("handle_state_json")? else {
            continue;
        };
        let value: Value = match serde_json::from_str(&state_json) {
            Ok(value) => value,
            Err(e) => {
                tracing::warn!(
                    target: "wave_vcs",
                    wave_id = %wave_id.as_str(),
                    session_id = %session_id,
                    error = %e,
                    "wave-vcs prune: active session snapshot is not parseable; skipping prune"
                );
                return Ok(ActiveLastSeenHeads::SkipPrune);
            }
        };

        let endpoints = match harness_snapshot_diff_endpoints(&value) {
            Ok(endpoints) => endpoints,
            Err(reason) => {
                tracing::warn!(
                    target: "wave_vcs",
                    wave_id = %wave_id.as_str(),
                    session_id = %session_id,
                    reason = %reason,
                    "wave-vcs prune: active session snapshot is ambiguous; skipping prune"
                );
                return Ok(ActiveLastSeenHeads::SkipPrune);
            }
        };
        for (endpoint_name, endpoint_hash) in [
            ("last_seen_head", endpoints.last_seen_head),
            ("issued_turn_head", endpoints.issued_turn_head),
        ] {
            let Some(endpoint_hash) = endpoint_hash else {
                continue;
            };
            let Some(record) = load_commit_record_for_wave_tx(tx, wave_id, endpoint_hash).await?
            else {
                tracing::warn!(
                    target: "wave_vcs",
                    wave_id = %wave_id.as_str(),
                    session_id = %session_id,
                    endpoint = endpoint_name,
                    commit_hash = %endpoint_hash,
                    "wave-vcs prune: active session references an absent commit; skipping prune"
                );
                return Ok(ActiveLastSeenHeads::SkipPrune);
            };
            records.push(record);
        }
    }

    Ok(ActiveLastSeenHeads::Safe(records))
}

struct HarnessSnapshotDiffEndpoints<'a> {
    last_seen_head: Option<&'a str>,
    issued_turn_head: Option<&'a str>,
}

fn harness_snapshot_diff_endpoints(
    value: &Value,
) -> std::result::Result<HarnessSnapshotDiffEndpoints<'_>, &'static str> {
    if value.get("schema_version").and_then(Value::as_i64) != Some(1) {
        return Err("unknown schema_version");
    }
    if value.get("mode").and_then(Value::as_str) != Some("harness") {
        return Err("unknown mode");
    }
    Ok(HarnessSnapshotDiffEndpoints {
        last_seen_head: harness_snapshot_endpoint(value, "last_seen_head")?,
        issued_turn_head: harness_snapshot_endpoint(value, "issued_turn_head")?,
    })
}

fn harness_snapshot_endpoint<'a>(
    value: &'a Value,
    field: &'static str,
) -> std::result::Result<Option<&'a str>, &'static str> {
    match value.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(hash)) => Ok(Some(hash.as_str())),
        Some(_) if field == "last_seen_head" => Err("invalid last_seen_head"),
        Some(_) => Err("invalid issued_turn_head"),
    }
}

async fn path_authors_since(
    pool: &SqlitePool,
    previous: &str,
    current: &str,
) -> Result<Option<BTreeMap<String, Option<String>>>> {
    let mut records = Vec::new();
    let mut cursor = current.to_string();
    let mut reached_previous = false;

    for _ in 0..ATTRIBUTION_COMMIT_BOUND {
        let Some(record) = load_commit_record_pool(pool, &cursor).await? else {
            return Ok(None);
        };
        let Some(parent_hash) = record.parent_hash.clone() else {
            return Ok(None);
        };
        reached_previous = parent_hash == previous;
        cursor = parent_hash;
        records.push(record);
        if reached_previous {
            break;
        }
    }

    if !reached_previous {
        return Ok(None);
    }

    let mut tree_cache = BTreeMap::<String, TreeManifest>::new();
    let mut authors = BTreeMap::<String, Option<String>>::new();
    for record in records {
        let Some(parent_hash) = record.parent_hash.as_deref() else {
            return Ok(None);
        };
        let Some(tree) = load_tree_for_commit_record(pool, &record, &mut tree_cache).await? else {
            return Ok(None);
        };
        let Some(parent) = load_tree_for_commit_hash(pool, parent_hash, &mut tree_cache).await?
        else {
            return Ok(None);
        };
        for entry in diff_manifests(&parent, &tree, None) {
            authors
                .entry(entry.path)
                .or_insert_with(|| record.author.clone());
        }
    }

    Ok(Some(authors))
}

async fn load_tree_for_commit_record(
    pool: &SqlitePool,
    record: &CommitRecord,
    cache: &mut BTreeMap<String, TreeManifest>,
) -> Result<Option<TreeManifest>> {
    if let Some(tree) = cache.get(&record.hash) {
        return Ok(Some(tree.clone()));
    }
    let Some(tree) = load_tree_object_pool(pool, &record.tree_hash).await? else {
        return Ok(None);
    };
    cache.insert(record.hash.clone(), tree.clone());
    Ok(Some(tree))
}

async fn load_tree_for_commit_hash(
    pool: &SqlitePool,
    commit_hash: &str,
    cache: &mut BTreeMap<String, TreeManifest>,
) -> Result<Option<TreeManifest>> {
    if let Some(tree) = cache.get(commit_hash) {
        return Ok(Some(tree.clone()));
    }
    let Some(tree) = tree_at(pool, commit_hash).await? else {
        return Ok(None);
    };
    cache.insert(commit_hash.to_string(), tree.clone());
    Ok(Some(tree))
}

fn diff_manifests(from: &TreeManifest, to: &TreeManifest, path: Option<&str>) -> Vec<DiffEntry> {
    let normalized = path.map(normalize_path).filter(|prefix| !prefix.is_empty());
    let mut paths = BTreeSet::new();
    paths.extend(from.entries.keys().cloned());
    paths.extend(to.entries.keys().cloned());

    let mut out = Vec::new();
    for path in paths {
        if let Some(prefix) = normalized.as_deref()
            && path != prefix
            && !path.starts_with(&format!("{prefix}/"))
        {
            continue;
        }
        let old = from.entries.get(&path);
        let new = to.entries.get(&path);
        match (old, new) {
            (None, Some(new)) => out.push(DiffEntry {
                path,
                status: DiffStatus::Added,
                old_hash: None,
                new_hash: Some(new.blob_hash.clone()),
            }),
            (Some(old), None) => out.push(DiffEntry {
                path,
                status: DiffStatus::Deleted,
                old_hash: Some(old.blob_hash.clone()),
                new_hash: None,
            }),
            (Some(old), Some(new)) if old.blob_hash != new.blob_hash => out.push(DiffEntry {
                path,
                status: DiffStatus::Modified,
                old_hash: Some(old.blob_hash.clone()),
                new_hash: Some(new.blob_hash.clone()),
            }),
            _ => {}
        }
    }
    out
}

async fn file_diffs_from_entries(
    pool: &SqlitePool,
    from_tree: &TreeManifest,
    to_tree: &TreeManifest,
    entries: Vec<DiffEntry>,
    max_patch_lines: usize,
) -> Result<Vec<FileDiff>> {
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        let old_entry = from_tree.entries.get(&entry.path);
        let new_entry = to_tree.entries.get(&entry.path);
        let old_content_type = old_entry.map(|entry| entry.content_type.clone());
        let new_content_type = new_entry.map(|entry| entry.content_type.clone());
        let patch = if should_render_text_patch(old_entry, new_entry) {
            let old = load_optional_text_blob(pool, old_entry).await?;
            let new = load_optional_text_blob(pool, new_entry).await?;
            match (old, new) {
                (Some(old), Some(new)) => {
                    let (patch, truncated) =
                        unified_patch(&entry.path, &old, &new, max_patch_lines);
                    Some((patch, truncated))
                }
                _ => None,
            }
        } else {
            None
        };
        let (patch, patch_truncated) = patch.unwrap_or_else(|| (String::new(), false));
        out.push(FileDiff {
            path: entry.path,
            status: entry.status,
            old_hash: entry.old_hash,
            new_hash: entry.new_hash,
            old_content_type,
            new_content_type,
            patch: if patch.is_empty() { None } else { Some(patch) },
            patch_truncated,
        });
    }
    Ok(out)
}

fn should_render_text_patch(
    old_entry: Option<&ManifestEntry>,
    new_entry: Option<&ManifestEntry>,
) -> bool {
    old_entry
        .or(new_entry)
        .map(|entry| is_text_content_type(&entry.content_type))
        .unwrap_or(false)
        && old_entry
            .map(|entry| is_text_content_type(&entry.content_type))
            .unwrap_or(true)
        && new_entry
            .map(|entry| is_text_content_type(&entry.content_type))
            .unwrap_or(true)
}

fn is_text_content_type(content_type: &str) -> bool {
    content_type.starts_with("text/")
        || matches!(
            content_type,
            "application/json" | "application/x-ndjson" | "application/ld+json"
        )
}

async fn load_optional_text_blob(
    pool: &SqlitePool,
    entry: Option<&ManifestEntry>,
) -> Result<Option<String>> {
    let Some(entry) = entry else {
        return Ok(Some(String::new()));
    };
    let Some(bytes) = load_blob_bytes_pool(pool, &entry.blob_hash).await? else {
        return Ok(None);
    };
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|e| CalmError::Internal(format!("wave-vcs: text blob is not UTF-8: {e}")))
}

fn unified_patch(path: &str, old: &str, new: &str, max_lines: usize) -> (String, bool) {
    let old_header = format!("a/{path}");
    let new_header = format!("b/{path}");
    let patch = TextDiff::from_lines(old, new)
        .unified_diff()
        .header(&old_header, &new_header)
        .to_string();
    truncate_lines(patch, max_lines)
}

fn truncate_lines(text: String, max_lines: usize) -> (String, bool) {
    if max_lines == 0 {
        return (
            "[wave-vcs patch truncated: line budget is 0]\n".to_string(),
            true,
        );
    }
    let mut lines = text.lines().collect::<Vec<_>>();
    if lines.len() <= max_lines {
        return (text, false);
    }
    lines.truncate(max_lines);
    let mut out = lines.join("\n");
    out.push('\n');
    out.push_str(&format!(
        "[wave-vcs patch truncated after {max_lines} lines]\n"
    ));
    (out, true)
}

fn path_matches(changed: &str, requested: &str) -> bool {
    changed == requested || changed.starts_with(&format!("{requested}/"))
}

#[derive(Clone, Copy)]
struct CommitTreeMeta<'a> {
    parent_hash: Option<&'a str>,
    author: Option<&'a str>,
    event_id: Option<i64>,
    message: &'a str,
    manifest_schema_version: i64,
    created_at: i64,
}

fn commit_hash_for_tree(
    wave_id: &WaveId,
    tree_hash: &str,
    lifecycle: &str,
    meta: &CommitTreeMeta<'_>,
) -> Result<CommitHash> {
    let mut commit = BTreeMap::<String, Value>::new();
    commit.insert("created_at".into(), Value::from(meta.created_at));
    commit.insert(
        "event_id".into(),
        meta.event_id.map(Value::from).unwrap_or(Value::Null),
    );
    commit.insert("lifecycle".into(), Value::String(lifecycle.to_string()));
    commit.insert(
        "manifest_schema_version".into(),
        Value::from(meta.manifest_schema_version),
    );
    commit.insert("message".into(), Value::String(meta.message.to_string()));
    commit.insert(
        "parent_hash".into(),
        meta.parent_hash
            .map(|hash| Value::String(hash.to_string()))
            .unwrap_or(Value::Null),
    );
    commit.insert("tree_hash".into(), Value::String(tree_hash.to_string()));
    commit.insert("wave_id".into(), Value::String(wave_id.to_string()));
    let commit_bytes = canonical_json_bytes(&commit)?;
    Ok(hash_bytes("commit", &commit_bytes))
}

async fn commit_tree_at_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &WaveId,
    tree: &TreeSnapshot,
    meta: CommitTreeMeta<'_>,
) -> Result<CommitHash> {
    let lifecycle = wave_lifecycle_tx(tx, wave_id).await?;
    let hash = commit_hash_for_tree(wave_id, &tree.tree_hash, &lifecycle, &meta)?;

    sqlx::query(
        r#"INSERT OR IGNORE INTO wave_vcs_commits (
               hash, wave_id, parent_hash, tree_hash, manifest_schema_version,
               author, message, lifecycle, event_id, created_at
           )
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)"#,
    )
    .bind(&hash)
    .bind(wave_id.as_str())
    .bind(meta.parent_hash)
    .bind(&tree.tree_hash)
    .bind(meta.manifest_schema_version)
    .bind(meta.author)
    .bind(meta.message)
    .bind(&lifecycle)
    .bind(meta.event_id)
    .bind(meta.created_at)
    .execute(&mut **tx)
    .await?;

    sqlx::query(
        r#"INSERT INTO wave_vcs_refs (wave_id, head_hash, updated_event_id)
           VALUES (?1, ?2, ?3)
           ON CONFLICT(wave_id) DO UPDATE SET
             head_hash = excluded.head_hash,
             updated_event_id = excluded.updated_event_id"#,
    )
    .bind(wave_id.as_str())
    .bind(&hash)
    .bind(meta.event_id)
    .execute(&mut **tx)
    .await?;

    Ok(hash)
}

async fn apply_delta_tx(
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

async fn store_tree(
    tx: &mut Transaction<'_, Sqlite>,
    schema_version: i64,
    entries: BTreeMap<String, ManifestEntry>,
    created_at: i64,
) -> Result<TreeSnapshot> {
    let manifest = TreeManifest {
        schema_version,
        entries,
    };
    let tree_hash = hash_tree_manifest(&manifest);
    let bytes = canonical_json_bytes(&manifest)?;
    sqlx::query(
        r#"INSERT OR IGNORE INTO wave_vcs_objects (hash, kind, bytes, created_at)
           VALUES (?1, 'tree', ?2, ?3)"#,
    )
    .bind(&tree_hash)
    .bind(&bytes)
    .bind(created_at)
    .execute(&mut **tx)
    .await?;
    Ok(TreeSnapshot {
        tree_hash,
        manifest,
    })
}

async fn put_rendered_entry(
    tx: &mut Transaction<'_, Sqlite>,
    entries: &mut BTreeMap<String, ManifestEntry>,
    path: impl Into<String>,
    content: BlobContent,
    created_at: i64,
) -> Result<()> {
    let hash = put_object_at_tx(tx, "blob", &content.bytes, created_at).await?;
    entries.insert(
        path.into(),
        ManifestEntry {
            blob_hash: hash,
            byte_len: content.bytes.len() as u64,
            content_type: content.content_type,
        },
    );
    Ok(())
}

async fn insert_run_entries(
    tx: &mut Transaction<'_, Sqlite>,
    entries: &mut BTreeMap<String, ManifestEntry>,
    runs: &[RunProjection],
    created_at: i64,
) -> Result<()> {
    put_rendered_entry(
        tx,
        entries,
        "runs/index.json",
        runs_index_json(runs)?,
        created_at,
    )
    .await?;
    for run in runs {
        if wave_fs_view::is_reserved_run_key(&run.idempotency_key) {
            // `wave_file` errors for reserved run keys because a live read can
            // fail without side effects. Wave VCS is in an event write tx, so
            // it skips the pathological key instead of rolling back unrelated
            // events; byte parity with `wave_file` is intentionally waived for
            // that invariant violation.
            tracing::error!(
                target: "wave_vcs",
                idempotency_key = %run.idempotency_key,
                "runs projection: skipping idempotency_key that collides with reserved path"
            );
            continue;
        }
        put_rendered_entry(
            tx,
            entries,
            format!("runs/{}.json", run.idempotency_key),
            run_json(run)?,
            created_at,
        )
        .await?;
        put_rendered_entry(
            tx,
            entries,
            format!("runs/{}.md", run.idempotency_key),
            content_markdown(run_markdown(run)),
            created_at,
        )
        .await?;
    }
    Ok(())
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

async fn load_blob_bytes_tx(
    tx: &mut Transaction<'_, Sqlite>,
    hash: &str,
) -> Result<Option<Vec<u8>>> {
    let row: Option<(Vec<u8>,)> =
        sqlx::query_as("SELECT bytes FROM wave_vcs_objects WHERE hash = ?1 AND kind = 'blob'")
            .bind(hash)
            .fetch_optional(&mut **tx)
            .await?;
    Ok(row.map(|(bytes,)| bytes))
}

async fn load_blob_bytes_pool(pool: &SqlitePool, hash: &str) -> Result<Option<Vec<u8>>> {
    let row: Option<(Vec<u8>,)> =
        sqlx::query_as("SELECT bytes FROM wave_vcs_objects WHERE hash = ?1 AND kind = 'blob'")
            .bind(hash)
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|(bytes,)| bytes))
}

async fn put_object_at_tx(
    tx: &mut Transaction<'_, Sqlite>,
    kind: &str,
    bytes: &[u8],
    created_at: i64,
) -> Result<ObjectHash> {
    let hash = hash_bytes(kind, bytes);
    sqlx::query(
        r#"INSERT OR IGNORE INTO wave_vcs_objects (hash, kind, bytes, created_at)
           VALUES (?1, ?2, ?3, ?4)"#,
    )
    .bind(&hash)
    .bind(kind)
    .bind(bytes)
    .bind(created_at)
    .execute(&mut **tx)
    .await?;
    Ok(hash)
}

fn paths_changed_by_event(event: &Event, wave_id: &WaveId) -> PathDelta {
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

fn add_card_event_paths(delta: &mut PathDelta, card_id: &CardId) {
    delta.add(format!("cards/{}/events.json", card_id.as_str()));
    delta.add(format!("cards/{}/conversation.md", card_id.as_str()));
}

async fn head_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &WaveId,
) -> Result<Option<CommitHash>> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT head_hash FROM wave_vcs_refs WHERE wave_id = ?1")
            .bind(wave_id.as_str())
            .fetch_optional(&mut **tx)
            .await?;
    Ok(row.map(|(hash,)| hash))
}

async fn tree_at_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    commit_hash: &str,
) -> Result<Option<TreeManifest>> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT tree_hash FROM wave_vcs_commits WHERE hash = ?1")
            .bind(commit_hash)
            .fetch_optional(&mut **tx)
            .await?;
    let Some((tree_hash,)) = row else {
        return Ok(None);
    };
    load_tree_object_tx(tx, &tree_hash).await
}

async fn load_tree_object_tx(
    tx: &mut Transaction<'_, Sqlite>,
    tree_hash: &str,
) -> Result<Option<TreeManifest>> {
    let row: Option<(Vec<u8>,)> =
        sqlx::query_as("SELECT bytes FROM wave_vcs_objects WHERE hash = ?1 AND kind = 'tree'")
            .bind(tree_hash)
            .fetch_optional(&mut **tx)
            .await?;
    row.map(|(bytes,)| serde_json::from_slice(&bytes).map_err(Into::into))
        .transpose()
}

async fn load_tree_object_pool(pool: &SqlitePool, tree_hash: &str) -> Result<Option<TreeManifest>> {
    let row: Option<(Vec<u8>,)> =
        sqlx::query_as("SELECT bytes FROM wave_vcs_objects WHERE hash = ?1 AND kind = 'tree'")
            .bind(tree_hash)
            .fetch_optional(pool)
            .await?;
    row.map(|(bytes,)| serde_json::from_slice(&bytes).map_err(Into::into))
        .transpose()
}

async fn load_commit_record_pool(
    pool: &SqlitePool,
    commit_hash: &str,
) -> Result<Option<CommitRecord>> {
    let row = sqlx::query(
        r#"SELECT hash, wave_id, parent_hash, tree_hash, manifest_schema_version,
                  lifecycle, event_id, created_at, message, author
           FROM wave_vcs_commits
           WHERE hash = ?1"#,
    )
    .bind(commit_hash)
    .fetch_optional(pool)
    .await?;
    row.map(commit_record_from_row).transpose()
}

async fn load_commit_record_for_wave_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &WaveId,
    commit_hash: &str,
) -> Result<Option<CommitRecord>> {
    let row = sqlx::query(
        r#"SELECT hash, wave_id, parent_hash, tree_hash, manifest_schema_version,
                  lifecycle, event_id, created_at, message, author
           FROM wave_vcs_commits
           WHERE hash = ?1
             AND wave_id = ?2"#,
    )
    .bind(commit_hash)
    .bind(wave_id.as_str())
    .fetch_optional(&mut **tx)
    .await?;
    row.map(commit_record_from_row).transpose()
}

async fn commit_records_for_wave_pool(
    pool: &SqlitePool,
    wave_id: &WaveId,
    limit: usize,
) -> Result<Vec<CommitRecord>> {
    let rows = sqlx::query(
        r#"SELECT hash, wave_id, parent_hash, tree_hash, manifest_schema_version,
                  lifecycle, event_id, created_at, message, author
           FROM wave_vcs_commits
           WHERE wave_id = ?1
           ORDER BY created_at DESC, COALESCE(event_id, -1) DESC, hash DESC
           LIMIT ?2"#,
    )
    .bind(wave_id.as_str())
    .bind(limit as i64)
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(commit_record_from_row).collect()
}

fn commit_record_from_row(row: SqliteRow) -> Result<CommitRecord> {
    Ok(CommitRecord {
        hash: row.try_get("hash")?,
        wave_id: WaveId::from(row.try_get::<String, _>("wave_id")?),
        parent_hash: row.try_get("parent_hash")?,
        tree_hash: row.try_get("tree_hash")?,
        manifest_schema_version: row.try_get("manifest_schema_version")?,
        lifecycle: row.try_get("lifecycle")?,
        event_id: row.try_get("event_id")?,
        created_at: row.try_get("created_at")?,
        message: row.try_get("message")?,
        author: row.try_get("author")?,
    })
}

async fn changed_paths_for_commit(pool: &SqlitePool, record: &CommitRecord) -> Result<Vec<String>> {
    let Some(tree) = load_tree_object_pool(pool, &record.tree_hash).await? else {
        return Ok(Vec::new());
    };
    let entries = if let Some(parent_hash) = record.parent_hash.as_deref() {
        let Some(parent) = tree_at(pool, parent_hash).await? else {
            return Ok(Vec::new());
        };
        diff_manifests(&parent, &tree, None)
            .into_iter()
            .map(|entry| entry.path)
            .collect()
    } else {
        tree.entries.keys().cloned().collect()
    };
    Ok(entries)
}

async fn latest_wave_event_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &WaveId,
) -> Result<Option<(i64, i64)>> {
    let row: Option<(i64, i64)> =
        sqlx::query_as("SELECT id, at FROM events WHERE scope_wave = ?1 ORDER BY id DESC LIMIT 1")
            .bind(wave_id.as_str())
            .fetch_optional(&mut **tx)
            .await?;
    Ok(row)
}

async fn wave_lifecycle_tx(tx: &mut Transaction<'_, Sqlite>, wave_id: &WaveId) -> Result<String> {
    let row: Option<(String,)> = sqlx::query_as("SELECT lifecycle FROM waves WHERE id = ?1")
        .bind(wave_id.as_str())
        .fetch_optional(&mut **tx)
        .await?;
    row.map(|(lifecycle,)| lifecycle)
        .ok_or_else(|| CalmError::NotFound(format!("wave {}", wave_id.as_str())))
}

async fn load_wave_tx(tx: &mut Transaction<'_, Sqlite>, wave_id: &WaveId) -> Result<Wave> {
    load_wave_optional_tx(tx, wave_id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("wave {}", wave_id.as_str())))
}

async fn load_wave_optional_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &WaveId,
) -> Result<Option<Wave>> {
    let row = sqlx::query_as::<_, crate::db::rows::WaveRow>(
        r#"SELECT id, cove_id, title, sort, archived_at, pinned_at, lifecycle, cwd,
                  workflow_id, terminal_at, created_at, updated_at
           FROM waves WHERE id = ?1"#,
    )
    .bind(wave_id.as_str())
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.map(Wave::from))
}

async fn cards_for_wave_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &WaveId,
    visibility: &CardVisibility,
) -> Result<Vec<CardProjection>> {
    // Keep this ORDER BY aligned with SqlxRepo::cards_by_wave in db/sqlite.rs;
    // tests pin the sort ASC, id ASC tie-break for duplicate worker run keys.
    let rows = sqlx::query(
        r#"SELECT id, wave_id, kind, sort, payload, role, deletable, created_at, updated_at,
                  EXISTS (
                    SELECT 1
                    FROM events
                    WHERE events.scope_wave = cards.wave_id
                      AND events.kind = 'card.added'
                      AND json_extract(events.payload, '$.id') = cards.id
                  ) AS vcs_announced
           FROM cards
           WHERE wave_id = ?1
           ORDER BY sort ASC, id ASC"#,
    )
    .bind(wave_id.as_str())
    .fetch_all(&mut **tx)
    .await?;
    let mut out = Vec::new();
    for row in rows {
        let id: String = row.try_get("id")?;
        let announced: i64 = row.try_get("vcs_announced")?;
        if visibility.includes(&id, announced != 0) {
            out.push(card_projection_from_row(row)?);
        }
    }
    Ok(out)
}

async fn card_in_wave_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &WaveId,
    card_id: &str,
    visibility: &CardVisibility,
) -> Result<Option<CardProjection>> {
    let row = sqlx::query(
        r#"SELECT id, wave_id, kind, sort, payload, role, deletable, created_at, updated_at,
                  EXISTS (
                    SELECT 1
                    FROM events
                    WHERE events.scope_wave = cards.wave_id
                      AND events.kind = 'card.added'
                      AND json_extract(events.payload, '$.id') = cards.id
                  ) AS vcs_announced
           FROM cards
           WHERE id = ?1 AND wave_id = ?2"#,
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
        card_projection_from_row(row).map(Some)
    } else {
        Ok(None)
    }
}

fn card_projection_from_row(row: SqliteRow) -> Result<CardProjection> {
    let payload_text: String = row.try_get("payload")?;
    let payload = serde_json::from_str(&payload_text)?;
    let deletable: i64 = row.try_get("deletable")?;
    Ok(CardProjection {
        card: Card {
            id: CardId::from(row.try_get::<String, _>("id")?),
            wave_id: WaveId::from(row.try_get::<String, _>("wave_id")?),
            kind: row.try_get("kind")?,
            sort: row.try_get("sort")?,
            payload,
            runtime: None,
            deletable: deletable != 0,
            created_at: row.try_get("created_at")?,
            updated_at: row.try_get("updated_at")?,
        },
        role: row.try_get("role")?,
    })
}

async fn hook_events_for_card_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &WaveId,
    card_id: &CardId,
) -> Result<Vec<HookEventProjection>> {
    let rows: Vec<EventRow> = sqlx::query_as(
        r#"SELECT id, kind, payload, actor, at,
                  scope_kind, scope_cove, scope_wave, scope_card
           FROM (
               SELECT id, kind, payload, actor, at,
                      scope_kind, scope_cove, scope_wave, scope_card
               FROM events
               WHERE scope_wave = ?1
                 AND scope_card = ?2
                 AND kind IN ('codex.hook', 'claude.hook')
               ORDER BY id DESC
               LIMIT ?3
           )
           ORDER BY id ASC"#,
    )
    .bind(wave_id.as_str())
    .bind(card_id.as_str())
    .bind(wave_fs_view::HOOK_EVENT_TRANSCRIPT_CAP as i64)
    .fetch_all(&mut **tx)
    .await?;

    rows.into_iter()
        .map(wave_event_from_row)
        .map(|row| {
            row.and_then(|row| match row.event {
                Event::CodexHook { kind, payload, .. } => Ok(HookEventProjection {
                    event_id: row.id,
                    at: row.at,
                    kind: "codex.hook",
                    hook_kind: kind,
                    payload,
                }),
                Event::ClaudeHook { kind, payload, .. } => Ok(HookEventProjection {
                    event_id: row.id,
                    at: row.at,
                    kind: "claude.hook",
                    hook_kind: kind,
                    payload,
                }),
                _ => Err(CalmError::Internal(
                    "wave-vcs: non-hook event returned from hook query".into(),
                )),
            })
        })
        .collect()
}

type EventRow = (
    i64,
    String,
    String,
    String,
    i64,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

async fn run_events_for_wave_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &WaveId,
) -> Result<Vec<WaveEvent>> {
    let rows: Vec<EventRow> = sqlx::query_as(
        r#"SELECT id, kind, payload, actor, at,
                  scope_kind, scope_cove, scope_wave, scope_card
           FROM events
           WHERE scope_wave = ?1
             AND kind IN (
               'codex.worker_requested',
               'terminal.worker_requested',
               'task.dispatched',
               'task.completed',
               'task.failed'
             )
           ORDER BY id ASC"#,
    )
    .bind(wave_id.as_str())
    .fetch_all(&mut **tx)
    .await?;

    rows.into_iter().map(wave_event_from_row).collect()
}

fn wave_event_from_row(row: EventRow) -> Result<WaveEvent> {
    let (id, kind, payload_text, actor_text, at, sk, sc, sw, scard) = row;
    let payload = serde_json::from_str(&payload_text)?;
    let actor = serde_json::from_str::<ActorId>(&actor_text)?;
    let scope = EventScope::from_row(
        sk.as_deref(),
        sc.as_deref(),
        sw.as_deref(),
        scard.as_deref(),
    );
    let event = Event::from_kind_and_payload(&kind, payload)?;
    Ok(WaveEvent {
        id,
        at,
        actor,
        scope,
        event,
    })
}

async fn project_runs_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &WaveId,
    cards: &[CardProjection],
) -> Result<Vec<RunProjection>> {
    let events = run_events_for_wave_tx(tx, wave_id).await?;

    let mut keys = BTreeSet::new();
    let mut worker_cards = BTreeMap::new();
    for card in cards.iter().cloned() {
        if card.role != "worker" {
            continue;
        }
        if let Some(key) = idempotency_key_from_payload(&card.card.payload) {
            keys.insert(key.to_string());
            worker_cards.entry(key.to_string()).or_insert(card.card);
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
            // Issue #644 PR-B — scheduler claim record; merged below as
            // the §5.6 requested-record fallback.
            Event::TaskDispatched {
                idempotency_key,
                kind,
                ..
            } => {
                keys.insert(idempotency_key.clone());
                dispatched_kind
                    .insert(idempotency_key.clone(), wave_fs_view::run_kind_static(kind));
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

    // §5.6 fallback: keys with a dispatch record but no
    // `*.worker_requested` event use it as their requested-record.
    for (key, event) in dispatched {
        requested.entry(key).or_insert(event);
    }
    for (key, kind) in dispatched_kind {
        requested_kind.entry(key).or_insert(kind);
    }

    Ok(keys
        .into_iter()
        .filter(|key| run_key_is_visible(key))
        .map(|key| {
            let worker_card = worker_cards.remove(&key);
            let requested_event = requested.remove(&key);
            let completed_event = completed.remove(&key);
            let failed_event = failed.remove(&key);
            let verdict_event = verdict.remove(&key);
            let verdict = verdict_event.as_ref().and_then(verdict_from_event);
            let final_event = latest_final_event(completed_event.as_ref(), failed_event.as_ref());
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
        .collect())
}

async fn project_run_by_key_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &WaveId,
    key: &str,
    card_visibility: &CardVisibility,
) -> Result<Option<RunProjection>> {
    if !run_key_is_visible(key) {
        return Ok(None);
    }
    let worker_projection = worker_card_for_run_key_tx(tx, wave_id, key, card_visibility).await?;
    let worker_card = worker_projection.map(|projection| projection.card);
    let events = run_events_for_key_tx(tx, wave_id, key).await?;
    if worker_card.is_none() && events.is_empty() {
        return Ok(None);
    }

    let mut requested_event = None;
    let mut requested_kind = None;
    let mut dispatched_event: Option<RunEventProjection> = None;
    let mut dispatched_kind = None;
    let mut completed_event = None;
    let mut failed_event = None;
    let mut verdict_event = None;

    for row in events {
        match &row.event {
            // Issue #644 PR-B — scheduler claim record; §5.6 fallback
            // applied after the loop when no `*.worker_requested` landed.
            Event::TaskDispatched { kind, .. } => {
                dispatched_kind = Some(wave_fs_view::run_kind_static(kind));
                let event = run_event(row.id, row.at, "task.dispatched", row.event.payload_value());
                if dispatched_event
                    .as_ref()
                    .is_none_or(|existing: &RunEventProjection| existing.event_id > event.event_id)
                {
                    dispatched_event = Some(event);
                }
            }
            Event::CodexWorkerRequested { .. } => {
                requested_kind = Some("codex");
                let event = run_event(
                    row.id,
                    row.at,
                    "codex.worker_requested",
                    row.event.payload_value(),
                );
                if requested_event
                    .as_ref()
                    .is_none_or(|existing: &RunEventProjection| existing.event_id > event.event_id)
                {
                    requested_event = Some(event);
                }
            }
            Event::TerminalWorkerRequested { .. } => {
                requested_kind = Some("terminal");
                let event = run_event(
                    row.id,
                    row.at,
                    "terminal.worker_requested",
                    row.event.payload_value(),
                );
                if requested_event
                    .as_ref()
                    .is_none_or(|existing: &RunEventProjection| existing.event_id > event.event_id)
                {
                    requested_event = Some(event);
                }
            }
            Event::TaskCompleted { .. } => {
                let event = run_event(row.id, row.at, "task.completed", row.event.payload_value());
                if is_spec_verdict_event(&row.scope, &row.actor) {
                    if verdict_event
                        .as_ref()
                        .is_none_or(|existing: &RunEventProjection| {
                            existing.event_id < event.event_id
                        })
                    {
                        verdict_event = Some(event);
                    }
                } else if completed_event
                    .as_ref()
                    .is_none_or(|existing: &RunEventProjection| existing.event_id < event.event_id)
                {
                    completed_event = Some(event);
                }
            }
            Event::TaskFailed { .. } => {
                let event = run_event(row.id, row.at, "task.failed", row.event.payload_value());
                if is_spec_verdict_event(&row.scope, &row.actor) {
                    if verdict_event
                        .as_ref()
                        .is_none_or(|existing: &RunEventProjection| {
                            existing.event_id < event.event_id
                        })
                    {
                        verdict_event = Some(event);
                    }
                } else if failed_event
                    .as_ref()
                    .is_none_or(|existing: &RunEventProjection| existing.event_id < event.event_id)
                {
                    failed_event = Some(event);
                }
            }
            _ => {}
        }
    }

    // §5.6 fallback: the dispatch record stands in for a missing
    // `*.worker_requested` event.
    if requested_event.is_none() {
        requested_event = dispatched_event;
    }
    if requested_kind.is_none() {
        requested_kind = dispatched_kind;
    }

    let verdict = verdict_event.as_ref().and_then(verdict_from_event);
    let final_event = latest_final_event(completed_event.as_ref(), failed_event.as_ref());
    let (status, finished_at) = match (requested_event.as_ref(), final_event) {
        (Some(_), Some(("completed", event))) => (WaveFsRunStatus::Completed, Some(event.at)),
        (Some(_), Some(("failed", event))) => (WaveFsRunStatus::Failed, Some(event.at)),
        (Some(_), Some((_, event))) => (WaveFsRunStatus::Unknown, Some(event.at)),
        (Some(_), None) if worker_card.is_some() => (WaveFsRunStatus::Running, None),
        (Some(_), None) => (WaveFsRunStatus::Requested, None),
        (None, _) => (WaveFsRunStatus::Unknown, None),
    };
    let kind = worker_card
        .as_ref()
        .and_then(run_kind_from_card)
        .or(requested_kind)
        .unwrap_or("unknown")
        .to_string();

    Ok(Some(RunProjection {
        idempotency_key: key.to_string(),
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
    }))
}

async fn worker_card_for_run_key_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &WaveId,
    key: &str,
    visibility: &CardVisibility,
) -> Result<Option<CardProjection>> {
    let rows = sqlx::query(
        r#"SELECT id, wave_id, kind, sort, payload, role, deletable, created_at, updated_at,
                  EXISTS (
                    SELECT 1
                    FROM events
                    WHERE events.scope_wave = cards.wave_id
                      AND events.kind = 'card.added'
                      AND json_extract(events.payload, '$.id') = cards.id
                  ) AS vcs_announced
           FROM cards
           WHERE wave_id = ?1
             AND role = 'worker'
             AND json_extract(payload, '$.idempotency_key') = ?2
           ORDER BY sort ASC, id ASC
           "#,
    )
    .bind(wave_id.as_str())
    .bind(key)
    .fetch_all(&mut **tx)
    .await?;
    for row in rows {
        let id: String = row.try_get("id")?;
        let announced: i64 = row.try_get("vcs_announced")?;
        if visibility.includes(&id, announced != 0) {
            return card_projection_from_row(row).map(Some);
        }
    }
    Ok(None)
}

async fn run_events_for_key_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &WaveId,
    key: &str,
) -> Result<Vec<WaveEvent>> {
    type EventRow = (
        i64,
        String,
        String,
        String,
        i64,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    );
    let rows: Vec<EventRow> = sqlx::query_as(
        r#"SELECT id, kind, payload, actor, at,
                  scope_kind, scope_cove, scope_wave, scope_card
           FROM events
           WHERE scope_wave = ?1
             AND kind IN (
               'codex.worker_requested',
               'terminal.worker_requested',
               'task.dispatched',
               'task.completed',
               'task.failed'
             )
             AND json_extract(payload, '$.idempotency_key') = ?2
           ORDER BY id ASC"#,
    )
    .bind(wave_id.as_str())
    .bind(key)
    .fetch_all(&mut **tx)
    .await?;

    let mut out = Vec::with_capacity(rows.len());
    for (id, kind, payload_text, actor_text, at, sk, sc, sw, scard) in rows {
        let payload = serde_json::from_str(&payload_text)?;
        let actor = serde_json::from_str::<ActorId>(&actor_text)?;
        let scope = EventScope::from_row(
            sk.as_deref(),
            sc.as_deref(),
            sw.as_deref(),
            scard.as_deref(),
        );
        let event = Event::from_kind_and_payload(&kind, payload)?;
        out.push(WaveEvent {
            id,
            at,
            actor,
            scope,
            event,
        });
    }
    Ok(out)
}

async fn project_runtime_into_cards_tx(
    tx: &mut Transaction<'_, Sqlite>,
    cards: &mut [CardProjection],
) -> Result<()> {
    if cards.is_empty() {
        return Ok(());
    }
    let card_ids = cards
        .iter()
        .map(|card| card.card.id.to_string())
        .collect::<Vec<_>>();
    let mut query = projectable_runtimes_for_cards_query(&card_ids);
    let rows = query.build().fetch_all(&mut **tx).await?;
    let runtimes = projectable_runtimes_for_cards_from_rows(rows)?;
    for card in cards {
        if let Some(runtime) = runtimes.get(card.card.id.as_str()) {
            session_projection_lookup::project_runtime_fields(&mut card.card, runtime);
        }
    }
    Ok(())
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
) -> Option<(&'static str, &'a RunEventProjection)> {
    match (completed, failed) {
        (Some(done), Some(fail)) if done.event_id > fail.event_id => Some(("completed", done)),
        (Some(_), Some(fail)) => Some(("failed", fail)),
        (Some(done), None) => Some(("completed", done)),
        (None, Some(fail)) => Some(("failed", fail)),
        (None, None) => None,
    }
}

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

fn run_key_is_visible(key: &str) -> bool {
    if wave_fs_view::is_reserved_run_key(key) {
        // Deliberate VCS/live-view divergence; see `insert_run_entries`.
        tracing::error!(
            target: "wave_vcs",
            idempotency_key = %key,
            "runs projection: skipping idempotency_key that collides with reserved path"
        );
        false
    } else {
        true
    }
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

fn index_markdown(wave: &Wave, card_count: usize) -> BlobContent {
    content_markdown(wave_fs_view::index_markdown(wave, card_count))
}

fn wave_json(wave: &Wave) -> Result<BlobContent> {
    content_json(wave)
}

fn report_markdown(cards: &[CardProjection]) -> Result<Option<BlobContent>> {
    let Some(report_card) = cards.iter().find(|card| card.card.kind == "wave-report") else {
        return Ok(None);
    };
    let payload = serde_json::from_value::<WaveReportPayload>(report_card.card.payload.clone())
        .map_err(|e| {
            CalmError::Internal(format!(
                "wave_report: malformed payload on card {}: {e}",
                report_card.card.id.as_str()
            ))
        })?;
    Ok(Some(content_markdown(payload.body)))
}

fn cards_index_json(cards: &[CardProjection]) -> Result<BlobContent> {
    let mut values = Vec::with_capacity(cards.len());
    for card in cards {
        values.push(card_meta_value(card)?);
    }
    content_json(&values)
}

fn card_meta_json(card: &CardProjection) -> Result<BlobContent> {
    content_json(&card_meta_value(card)?)
}

fn card_meta_value(card: &CardProjection) -> Result<crate::wave_fs_dto::WaveFsCardMeta> {
    // Hard-erroring on an unknown role is intentional and unreachable in practice:
    // migration 0037_drop_plain_role.sql backfilled 'plain'→'worker' and added
    // insert/update triggers restricting cards.role to worker|spec|reportcard, so
    // any parse failure here is DB corruption worth failing loudly on.
    let role = serde_json::from_value::<CardRole>(Value::String(card.role.clone()))?;
    Ok(wave_fs_view::card_meta_value(&card.card, role))
}

fn card_payload_json(card: &CardProjection) -> Result<BlobContent> {
    content_json(&card.card.payload)
}

fn is_internal_observation_diff_path(path: &str, spec_card_id: Option<&CardId>) -> bool {
    if path.starts_with("cards/") && path.ends_with("/runtime.json") {
        return true;
    }
    let Some(spec_card_id) = spec_card_id else {
        return false;
    };
    let spec_card_id = spec_card_id.as_str();
    // Legacy spellings appear once per wave in the post-rename healing commit.
    path == format!("cards/{spec_card_id}/.payload.json")
        || path == format!("cards/{spec_card_id}/payload.json")
}

fn card_runtime_json(card: &Card) -> Result<BlobContent> {
    match &card.runtime {
        Some(runtime) => content_json(runtime),
        None => content_json(&Value::Null),
    }
}

fn hook_events_json(events: &[HookEventProjection]) -> Result<BlobContent> {
    content_json(&wave_fs_view::hook_events_json(events))
}

fn conversation_markdown(card_id: &CardId, events: &[HookEventProjection]) -> String {
    wave_fs_view::conversation_markdown(card_id, events)
}

fn runs_index_json(runs: &[RunProjection]) -> Result<BlobContent> {
    let values = runs
        .iter()
        .map(wave_fs_view::run_index_entry)
        .collect::<Vec<_>>();
    content_json(&values)
}

fn run_json(run: &RunProjection) -> Result<BlobContent> {
    content_json(&wave_fs_view::run_json(run))
}

fn run_markdown(run: &RunProjection) -> String {
    wave_fs_view::run_markdown(run)
}

fn content_markdown(content: String) -> BlobContent {
    blob_from_fs_content(wave_fs_view::content_markdown(content))
}

fn content_json<T: Serialize>(value: &T) -> Result<BlobContent> {
    Ok(blob_from_fs_content(wave_fs_view::content_json(value)?))
}

fn blob_from_fs_content(content: wave_fs_view::WaveFsContent) -> BlobContent {
    BlobContent {
        bytes: content.content.into_bytes(),
        content_type: content.content_type,
    }
}

pub fn canonical_json_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    let value = serde_json::to_value(value)?;
    let mut out = Vec::new();
    write_canonical_json(&mut out, &value)?;
    Ok(out)
}

fn write_canonical_json(out: &mut Vec<u8>, value: &Value) -> Result<()> {
    match value {
        Value::Null => out.extend_from_slice(b"null"),
        Value::Bool(v) => out.extend_from_slice(if *v { b"true" } else { b"false" }),
        Value::Number(number) => out.extend_from_slice(number.to_string().as_bytes()),
        Value::String(s) => serde_json::to_writer(out, s)?,
        Value::Array(values) => {
            out.push(b'[');
            for (idx, value) in values.iter().enumerate() {
                if idx > 0 {
                    out.push(b',');
                }
                write_canonical_json(out, value)?;
            }
            out.push(b']');
        }
        Value::Object(map) => {
            out.push(b'{');
            let mut first = true;
            for (key, value) in map.iter().collect::<BTreeMap<_, _>>() {
                if !first {
                    out.push(b',');
                }
                first = false;
                serde_json::to_writer(&mut *out, key)?;
                out.push(b':');
                write_canonical_json(out, value)?;
            }
            out.push(b'}');
        }
    }
    Ok(())
}

fn hash_tree_manifest(manifest: &TreeManifest) -> ObjectHash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"calm-wave-vcs-v1\0tree\0");
    hasher.update(manifest.schema_version.to_string().as_bytes());
    hasher.update(b"\0");
    for (path, entry) in &manifest.entries {
        hasher.update(path.as_bytes());
        hasher.update(b"\0");
        hasher.update(entry.blob_hash.as_bytes());
        hasher.update(b"\0");
        hasher.update(entry.byte_len.to_string().as_bytes());
        hasher.update(b"\0");
        hasher.update(entry.content_type.as_bytes());
        hasher.update(b"\0");
    }
    hasher.finalize().to_hex().to_string()
}

fn hash_bytes(kind: &str, bytes: &[u8]) -> ObjectHash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"calm-wave-vcs-v1\0");
    hasher.update(kind.as_bytes());
    hasher.update(b"\0");
    hasher.update(bytes);
    hasher.finalize().to_hex().to_string()
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

fn short_hash(hash: &str) -> &str {
    hash.get(..8).unwrap_or(hash)
}

fn markdown_code_fence_for(text: &str) -> String {
    let mut longest = 0;
    let mut current = 0;
    for ch in text.chars() {
        if ch == '`' {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    "`".repeat(3.max(longest + 1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::prelude::*;
    use crate::db::sqlite::{SqlxRepo, begin_immediate_tx};
    use crate::event::ForgeMergeSubject;
    use crate::model::{NewCove, NewWave, RequestTheme};
    use calm_types::event::{ChannelVerdict, RatifyDecision, ReviewSubject};

    #[test]
    fn commit_hash_ignores_author_metadata() {
        let wave_id = WaveId::from("wave-1");
        let base = CommitTreeMeta {
            parent_hash: Some("parent-1"),
            author: Some("user"),
            event_id: Some(7),
            message: "wave.updated",
            manifest_schema_version: MANIFEST_SCHEMA_VERSION,
            created_at: 1234,
        };
        let other_author = CommitTreeMeta {
            author: Some("kernel"),
            ..base
        };

        assert_eq!(
            commit_hash_for_tree(&wave_id, "tree-1", "draft", &base).unwrap(),
            commit_hash_for_tree(&wave_id, "tree-1", "draft", &other_author).unwrap()
        );
    }

    #[tokio::test]
    async fn forge_pr_merged_only_batch_does_not_advance_head() {
        let repo = SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open sqlite repo");
        let cove = repo
            .cove_create(NewCove {
                name: "cove".into(),
                color: "#336699".into(),
                sort: None,
            })
            .await
            .expect("create cove");
        let wave = repo
            .wave_create(NewWave {
                cove_id: cove.id,
                title: "wave".into(),
                sort: None,
                cwd: "/tmp".into(),
                workflow_id: None,
                attach_folder: false,
                theme: RequestTheme::default_dark(),
            })
            .await
            .expect("create wave");
        let before = head(repo.pool(), &wave.id).await.expect("head before");

        let event = Event::ForgePrMerged {
            wave_id: wave.id.clone(),
            subject: ForgeMergeSubject {
                phase: "impl".into(),
                slice_id: "6".into(),
                pr_number: 760,
            },
            head_sha: "head-sha".into(),
            merge_sha: "merge-sha".into(),
        };
        let mut tx = begin_immediate_tx(repo.pool())
            .await
            .expect("begin transaction");
        let committed = commit_events_with_author_in_tx(
            &mut tx,
            &wave.id,
            Some(&ActorId::KernelDispatcher),
            42,
            &[event],
            MANIFEST_SCHEMA_VERSION,
        )
        .await
        .expect("commit forge.pr.merged batch");
        tx.commit().await.expect("commit transaction");

        let after = head(repo.pool(), &wave.id).await.expect("head after");
        let commit_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM wave_vcs_commits WHERE wave_id = ?1")
                .bind(wave.id.as_str())
                .fetch_one(repo.pool())
                .await
                .expect("commit count");
        assert_eq!(committed, None);
        assert_eq!(after, before);
        assert_eq!(commit_count, 0);
    }

    #[tokio::test]
    async fn worktree_committed_only_batch_does_not_advance_head() {
        let repo = SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open sqlite repo");
        let cove = repo
            .cove_create(NewCove {
                name: "cove".into(),
                color: "#336699".into(),
                sort: None,
            })
            .await
            .expect("create cove");
        let wave = repo
            .wave_create(NewWave {
                cove_id: cove.id,
                title: "wave".into(),
                sort: None,
                cwd: "/tmp".into(),
                workflow_id: None,
                attach_folder: false,
                theme: RequestTheme::default_dark(),
            })
            .await
            .expect("create wave");
        let before = head(repo.pool(), &wave.id).await.expect("head before");

        let event = Event::WorktreeCommitted {
            wave_id: wave.id.clone(),
            card_id: CardId::from("card-1"),
            commit_sha: "1111111111111111111111111111111111111111".into(),
            branch: "neige/wave/card-1".into(),
        };
        let mut tx = begin_immediate_tx(repo.pool())
            .await
            .expect("begin transaction");
        let committed = commit_events_with_author_in_tx(
            &mut tx,
            &wave.id,
            Some(&ActorId::KernelDispatcher),
            42,
            &[event],
            MANIFEST_SCHEMA_VERSION,
        )
        .await
        .expect("commit worktree.committed batch");
        tx.commit().await.expect("commit transaction");

        let after = head(repo.pool(), &wave.id).await.expect("head after");
        let commit_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM wave_vcs_commits WHERE wave_id = ?1")
                .bind(wave.id.as_str())
                .fetch_one(repo.pool())
                .await
                .expect("commit count");
        assert_eq!(committed, None);
        assert_eq!(after, before);
        assert_eq!(commit_count, 0);
    }

    #[tokio::test]
    async fn review_ratify_only_batch_does_not_advance_head() {
        let repo = SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open sqlite repo");
        let cove = repo
            .cove_create(NewCove {
                name: "cove".into(),
                color: "#336699".into(),
                sort: None,
            })
            .await
            .expect("create cove");
        let wave = repo
            .wave_create(NewWave {
                cove_id: cove.id,
                title: "wave".into(),
                sort: None,
                cwd: "/tmp".into(),
                workflow_id: None,
                attach_folder: false,
                theme: RequestTheme::default_dark(),
            })
            .await
            .expect("create wave");
        let before = head(repo.pool(), &wave.id).await.expect("head before");

        let events = vec![
            Event::ReviewRound {
                wave_id: wave.id.clone(),
                subject: ReviewSubject {
                    phase: "impl".into(),
                    slice_id: "5b".into(),
                    pr_number: Some(760),
                },
                head_sha: Some("head-sha".into()),
                n: 1,
                cap: 8,
                converged: false,
                channels: vec![
                    ChannelVerdict {
                        role: "design-correctness".into(),
                        verdict: "changes_requested".into(),
                    },
                    ChannelVerdict {
                        role: "failure-path".into(),
                        verdict: "approved".into(),
                    },
                ],
                root_cause: Some("tests failing".into()),
                idempotency_key: format!("review.round:{}:impl:5b:760:1", wave.id),
            },
            Event::RatifyRequested {
                wave_id: wave.id.clone(),
                reason: "cap_exhausted".into(),
            },
            Event::RatifyResolved {
                wave_id: wave.id.clone(),
                decision: RatifyDecision::Grant,
            },
        ];
        let mut tx = begin_immediate_tx(repo.pool())
            .await
            .expect("begin transaction");
        let committed = commit_events_with_author_in_tx(
            &mut tx,
            &wave.id,
            Some(&ActorId::AiSpec(CardId::from("spec-card"))),
            42,
            &events,
            MANIFEST_SCHEMA_VERSION,
        )
        .await
        .expect("commit review/ratify batch");
        tx.commit().await.expect("commit transaction");

        let after = head(repo.pool(), &wave.id).await.expect("head after");
        let commit_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM wave_vcs_commits WHERE wave_id = ?1")
                .bind(wave.id.as_str())
                .fetch_one(repo.pool())
                .await
                .expect("commit count");
        assert_eq!(committed, None);
        assert_eq!(after, before);
        assert_eq!(commit_count, 0);
    }

    #[test]
    fn wave_history_pruner_config_from_env_respects_disable_and_defaults() {
        let saved_interval = std::env::var(WAVE_HISTORY_PRUNE_INTERVAL_SECS_ENV).ok();
        let saved_keep = std::env::var(WAVE_HISTORY_PRUNE_KEEP_ENV).ok();
        fn set(key: &str, value: &str) {
            // SAFETY: this test owns the wave-pruner env vars it mutates.
            unsafe { std::env::set_var(key, value) };
        }
        fn remove(key: &str) {
            // SAFETY: see `set`.
            unsafe { std::env::remove_var(key) };
        }

        remove(WAVE_HISTORY_PRUNE_INTERVAL_SECS_ENV);
        remove(WAVE_HISTORY_PRUNE_KEEP_ENV);
        assert_eq!(
            wave_history_pruner_config_from_env(),
            Some((WAVE_HISTORY_PRUNE_INTERVAL, DEFAULT_WAVE_HISTORY_PRUNE_KEEP))
        );

        set(WAVE_HISTORY_PRUNE_INTERVAL_SECS_ENV, "0");
        assert_eq!(wave_history_pruner_config_from_env(), None);

        set(WAVE_HISTORY_PRUNE_INTERVAL_SECS_ENV, "17");
        set(WAVE_HISTORY_PRUNE_KEEP_ENV, "23");
        assert_eq!(
            wave_history_pruner_config_from_env(),
            Some((Duration::from_secs(17), 23))
        );

        set(WAVE_HISTORY_PRUNE_KEEP_ENV, "0");
        assert_eq!(
            wave_history_pruner_config_from_env(),
            Some((Duration::from_secs(17), DEFAULT_WAVE_HISTORY_PRUNE_KEEP))
        );

        match saved_interval {
            Some(value) => set(WAVE_HISTORY_PRUNE_INTERVAL_SECS_ENV, &value),
            None => remove(WAVE_HISTORY_PRUNE_INTERVAL_SECS_ENV),
        }
        match saved_keep {
            Some(value) => set(WAVE_HISTORY_PRUNE_KEEP_ENV, &value),
            None => remove(WAVE_HISTORY_PRUNE_KEEP_ENV),
        }
    }
}
