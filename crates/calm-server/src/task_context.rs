//! Frozen report-block closures and fail-closed stale-context detection.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use calm_types::event::{Event, EventScope, TaskContextChangedRef, TaskContextRef};
use calm_types::report_blocks::{canonical_json, flat_text, scannable_text_fields};
use calm_types::report_links::{parse_destination, scan_links};
use calm_types::wave_report::ReportBlock;
use dashmap::DashMap;
use sha2::{Digest, Sha256};
use sqlx::{Sqlite, Transaction};

use crate::db::sqlite::mark_context_material_tx;
use crate::db::{Repo, write_in_tx_typed, write_with_actor_events_typed};
use crate::error::{CalmError, Result};
use crate::event::EventBus;
use crate::ids::{ActorId, WaveId};
use crate::model::now_ms;
use crate::state::WriteContext;

pub const MAX_REF_DEPTH: usize = 3;
pub const MAX_REF_NODES: usize = 64;
pub const MAX_RERESOLVE_FANOUT: usize = 64;
pub const MAX_SWEEP_NODES: usize = 4096;
const VERIFY_FAILURE_LIMIT: i64 = 3;
const CONTENT_CHANGED_RATIONALE: &str = "content_changed";
const RESTORED_RATIONALE: &str = "content_restored_to_frozen";
const MATERIAL_VERDICT_OBSOLETE: &str = "task context material verdict became obsolete";
const RESTORE_NOT_ELIGIBLE: &str = "task context restore candidate is no longer eligible";
const RESTORE_EVIDENCE_CHANGED: &str = "task context restore evidence changed in transaction";
const RESTORE_DECLARATION_WITHDRAWN: &str = "task context restore vetoed by declaration withdrawal";

#[derive(Debug, PartialEq, Eq)]
enum RefsMatch {
    Same,
    Mismatch(Vec<TaskContextChangedRef>, &'static str),
    Retryable(String),
}

pub const ROOT_HASH_TASK_FIELDS: &[&str] = &[
    "kind",
    "goal",
    "acceptance",
    "gate",
    "no_gate_reason",
    "depends_on",
    "refs",
    "cwd",
    "context",
];
pub const ROOT_HASH_EXCLUDED_TASK_FIELDS: &[&str] = &[
    "key",
    "priority",
    "declared_by",
    "spawn",
    "tombstone",
    "tombstoned_by",
    "ready",
    "released_by_user",
];

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FrozenClosure {
    pub refs: Vec<TaskContextRef>,
    pub doc_revs: BTreeMap<String, u64>,
    pub closure_truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolveError {
    StorageUnavailable(String),
    MalformedStoredReport(String),
    RootAbsent,
    RootTombstoned,
    DuplicateLiveKey,
    ReferencedWaveAbsent(String),
    ReferencedBlockAbsent(String),
    ReportAbsent(String),
    CrossCove(String),
    InvalidReference(String),
}

impl ResolveError {
    pub const fn variant(&self) -> &'static str {
        match self {
            Self::StorageUnavailable(_) => "storage_unavailable",
            Self::MalformedStoredReport(_) => "malformed_stored_report",
            Self::RootAbsent => "root_absent",
            Self::RootTombstoned => "root_tombstoned",
            Self::DuplicateLiveKey => "duplicate_live_key",
            Self::ReferencedWaveAbsent(_) => "referenced_wave_absent",
            Self::ReferencedBlockAbsent(_) => "referenced_block_absent",
            Self::ReportAbsent(_) => "report_absent",
            Self::CrossCove(_) => "cross_cove",
            Self::InvalidReference(_) => "invalid_reference",
        }
    }
}

#[derive(Default)]
pub struct ContextMetrics {
    detections: AtomicU64,
    hits: AtomicU64,
    closure_total: AtomicU64,
    closure_truncated: AtomicU64,
    fanout_total: AtomicU64,
    fanout_zero: AtomicU64,
    fanout_one_to_eight: AtomicU64,
    fanout_nine_to_sixty_four: AtomicU64,
    fanout_over_limit: AtomicU64,
    sweep_duration_ms: AtomicU64,
    sweep_verified_tuples: AtomicU64,
    sweep_hits: AtomicU64,
    sweep_caps: AtomicU64,
    sweep_restore_verified_tuples: AtomicU64,
    sweep_restore_hits: AtomicU64,
    sweep_restore_caps: AtomicU64,
    last_success_ms: AtomicI64,
    consecutive_failures: AtomicU64,
    claim_fence_race_lost: AtomicU64,
    material_verdict_obsolete: AtomicU64,
    restore_checks: AtomicU64,
    restores: AtomicU64,
    restore_deferred: DashMap<&'static str, u64>,
    context_resolve_failures: DashMap<&'static str, u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContextMetricsSnapshot {
    pub detections: u64,
    pub hits: u64,
    pub closure_total: u64,
    pub closure_truncated: u64,
    pub fanout_total: u64,
    pub fanout_buckets: [u64; 4],
    pub sweep_duration_ms: u64,
    pub sweep_verified_tuples: u64,
    pub sweep_hits: u64,
    pub sweep_caps: u64,
    pub sweep_restore_verified_tuples: u64,
    pub sweep_restore_hits: u64,
    pub sweep_restore_caps: u64,
    pub last_success_age_seconds: u64,
    pub consecutive_failures: u64,
    pub claim_fence_race_lost: u64,
    pub material_verdict_obsolete: u64,
    pub restore_checks: u64,
    pub restores: u64,
    pub restore_deferred: BTreeMap<&'static str, u64>,
    pub context_resolve_failures: BTreeMap<&'static str, u64>,
}

impl ContextMetrics {
    pub fn snapshot(&self) -> ContextMetricsSnapshot {
        let last = self.last_success_ms.load(Ordering::Relaxed);
        ContextMetricsSnapshot {
            detections: self.detections.load(Ordering::Relaxed),
            hits: self.hits.load(Ordering::Relaxed),
            closure_total: self.closure_total.load(Ordering::Relaxed),
            closure_truncated: self.closure_truncated.load(Ordering::Relaxed),
            fanout_total: self.fanout_total.load(Ordering::Relaxed),
            fanout_buckets: [
                self.fanout_zero.load(Ordering::Relaxed),
                self.fanout_one_to_eight.load(Ordering::Relaxed),
                self.fanout_nine_to_sixty_four.load(Ordering::Relaxed),
                self.fanout_over_limit.load(Ordering::Relaxed),
            ],
            sweep_duration_ms: self.sweep_duration_ms.load(Ordering::Relaxed),
            sweep_verified_tuples: self.sweep_verified_tuples.load(Ordering::Relaxed),
            sweep_hits: self.sweep_hits.load(Ordering::Relaxed),
            sweep_caps: self.sweep_caps.load(Ordering::Relaxed),
            sweep_restore_verified_tuples: self
                .sweep_restore_verified_tuples
                .load(Ordering::Relaxed),
            sweep_restore_hits: self.sweep_restore_hits.load(Ordering::Relaxed),
            sweep_restore_caps: self.sweep_restore_caps.load(Ordering::Relaxed),
            last_success_age_seconds: if last <= 0 {
                u64::MAX
            } else {
                now_ms().saturating_sub(last) as u64 / 1000
            },
            consecutive_failures: self.consecutive_failures.load(Ordering::Relaxed),
            claim_fence_race_lost: self.claim_fence_race_lost.load(Ordering::Relaxed),
            material_verdict_obsolete: self.material_verdict_obsolete.load(Ordering::Relaxed),
            restore_checks: self.restore_checks.load(Ordering::Relaxed),
            restores: self.restores.load(Ordering::Relaxed),
            restore_deferred: self
                .restore_deferred
                .iter()
                .map(|entry| (*entry.key(), *entry.value()))
                .collect(),
            context_resolve_failures: self
                .context_resolve_failures
                .iter()
                .map(|entry| (*entry.key(), *entry.value()))
                .collect(),
        }
    }

    pub fn record_claim_fence_race_lost(&self) {
        self.claim_fence_race_lost.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_context_resolve_failure(&self, variant: &'static str) {
        *self.context_resolve_failures.entry(variant).or_insert(0) += 1;
    }

    fn record_restore_deferred(&self, variant: &'static str) {
        *self.restore_deferred.entry(variant).or_insert(0) += 1;
    }

    fn export(&self) -> ContextMetricsSnapshot {
        let health = self.snapshot();
        tracing::info!(
            context_sweep_last_success_age_seconds = health.last_success_age_seconds,
            context_sweep_consecutive_failures = health.consecutive_failures,
            context_sweep_duration_ms = health.sweep_duration_ms,
            context_sweep_verified_tuples = health.sweep_verified_tuples,
            context_sweep_hits = health.sweep_hits,
            context_sweep_caps = health.sweep_caps,
            context_sweep_restore_verified_tuples = health.sweep_restore_verified_tuples,
            context_sweep_restore_hits = health.sweep_restore_hits,
            context_sweep_restore_caps = health.sweep_restore_caps,
            context_claim_fence_race_lost = health.claim_fence_race_lost,
            context_material_verdict_obsolete = health.material_verdict_obsolete,
            context_restore_checks = health.restore_checks,
            context_restores = health.restores,
            context_restore_deferred = ?health.restore_deferred,
            context_resolve_failures = ?health.context_resolve_failures,
            "task context sweep metrics"
        );
        health
    }
}

pub struct TaskContextMonitor {
    repo: Arc<dyn Repo>,
    events: EventBus,
    write: WriteContext,
    metrics: Arc<ContextMetrics>,
    restore_cursors: DashMap<String, String>,
}

#[derive(Debug, Default)]
struct SweepOutcome {
    verified_tuples: usize,
    hits: usize,
    capped: bool,
    restore_verified_tuples: usize,
    restore_hits: usize,
    restore_capped: bool,
}

impl TaskContextMonitor {
    pub fn new(repo: Arc<dyn Repo>, events: EventBus, write: WriteContext) -> Self {
        Self::new_with_metrics(repo, events, write, Arc::new(ContextMetrics::default()))
    }

    pub fn new_with_metrics(
        repo: Arc<dyn Repo>,
        events: EventBus,
        write: WriteContext,
        metrics: Arc<ContextMetrics>,
    ) -> Self {
        Self {
            repo,
            events,
            write,
            metrics,
            restore_cursors: DashMap::new(),
        }
    }

    pub fn metrics(&self) -> Arc<ContextMetrics> {
        Arc::clone(&self.metrics)
    }

    fn rotate_stale_rows(&self, cursor_scope: &str, rows: &mut [calm_truth::db::TaskContextRow]) {
        let Some(cursor) = self.restore_cursors.get(cursor_scope) else {
            return;
        };
        let split = rows.partition_point(|row| row.task_id.as_str() <= cursor.value().as_str());
        rows.rotate_left(split);
    }

    fn advance_restore_cursor(&self, cursor_scope: &str, task_id: &str) {
        self.restore_cursors
            .insert(cursor_scope.to_string(), task_id.to_string());
    }

    pub async fn resolve_closure(
        &self,
        task_wave_id: &str,
        root_block_id: &str,
    ) -> std::result::Result<FrozenClosure, ResolveError> {
        self.resolve_from_root(task_wave_id, root_block_id).await
    }

    pub async fn resolve_task_closure(
        &self,
        task_wave_id: &str,
        task_key: &str,
    ) -> std::result::Result<FrozenClosure, ResolveError> {
        let mut doc_revs = BTreeMap::new();
        let (_, blocks) = self.report_snapshot(task_wave_id, &mut doc_revs).await?;
        let mut live = Vec::new();
        let mut tombstoned = false;
        for value in blocks {
            if value.get("kind").and_then(serde_json::Value::as_str) == Some("task")
                && value
                    .get("payload")
                    .and_then(|payload| payload.get("key"))
                    .and_then(serde_json::Value::as_str)
                    == Some(task_key)
            {
                let block: ReportBlock = serde_json::from_value(value)
                    .map_err(|_| ResolveError::MalformedStoredReport(task_wave_id.into()))?;
                if block
                    .payload
                    .get("tombstone")
                    .is_some_and(|value| !value.is_null())
                {
                    tombstoned = true;
                } else {
                    live.push(block.id);
                }
            }
        }
        // #1160 — the read path states the same rule in
        // `calm-truth::db::sqlite::task_projection::live_declaration_blocks_by_key`
        // (calm-truth cannot depend on calm-server). Keep the two arms in sync;
        // do not add a third spelling of "which block owns this key".
        let root = match live.as_slice() {
            [root] => root.clone(),
            [] if tombstoned => return Err(ResolveError::RootTombstoned),
            [] => return Err(ResolveError::RootAbsent),
            _ => return Err(ResolveError::DuplicateLiveKey),
        };
        self.resolve_from_root_with_revs(task_wave_id, &root, doc_revs)
            .await
    }

    async fn resolve_from_root(
        &self,
        task_wave_id: &str,
        root_block_id: &str,
    ) -> std::result::Result<FrozenClosure, ResolveError> {
        self.resolve_from_root_with_revs(task_wave_id, root_block_id, BTreeMap::new())
            .await
    }

    async fn resolve_from_root_with_revs(
        &self,
        task_wave_id: &str,
        root_block_id: &str,
        mut doc_revs: BTreeMap<String, u64>,
    ) -> std::result::Result<FrozenClosure, ResolveError> {
        let task_wave = self
            .repo
            .wave_get(task_wave_id)
            .await
            .map_err(|e| ResolveError::StorageUnavailable(e.to_string()))?
            .ok_or_else(|| ResolveError::ReferencedWaveAbsent(task_wave_id.into()))?;
        let system_cove = self
            .repo
            .cove_get_system()
            .await
            .map_err(|e| ResolveError::StorageUnavailable(e.to_string()))?
            .map(|c| c.id.to_string());
        let mut queue = VecDeque::from([(task_wave_id.to_string(), root_block_id.to_string(), 0)]);
        let mut visited = BTreeSet::new();
        let mut refs = Vec::new();
        let mut truncated = false;
        while let Some((wave_id, block_id, depth)) = queue.pop_front() {
            if !visited.insert((wave_id.clone(), block_id.clone())) {
                continue;
            }
            if refs.len() == MAX_REF_NODES {
                truncated = true;
                break;
            }
            let is_root = depth == 0;
            let (cove_id, block) = self
                .load_block(&wave_id, &block_id, is_root, &mut doc_revs)
                .await?;
            if cove_id != task_wave.cove_id.as_str()
                && system_cove.as_deref() != Some(cove_id.as_str())
            {
                return Err(ResolveError::CrossCove(format!("{wave_id}#{block_id}")));
            }
            refs.push(context_ref(&wave_id, &block, is_root));
            for (dst_wave, dst_block) in block_links(&block)? {
                if depth == MAX_REF_DEPTH {
                    truncated = true;
                } else {
                    queue.push_back((dst_wave, dst_block, depth + 1));
                }
            }
        }
        self.metrics.closure_total.fetch_add(1, Ordering::Relaxed);
        if truncated {
            self.metrics
                .closure_truncated
                .fetch_add(1, Ordering::Relaxed);
        }
        Ok(FrozenClosure {
            refs,
            doc_revs,
            closure_truncated: truncated,
        })
    }

    async fn report_snapshot(
        &self,
        wave_id: &str,
        doc_revs: &mut BTreeMap<String, u64>,
    ) -> std::result::Result<(String, Vec<serde_json::Value>), ResolveError> {
        let wave = self
            .repo
            .wave_get(wave_id)
            .await
            .map_err(|e| ResolveError::StorageUnavailable(e.to_string()))?
            .ok_or_else(|| ResolveError::ReferencedWaveAbsent(wave_id.into()))?;
        let cards = self
            .repo
            .cards_by_wave(wave_id)
            .await
            .map_err(|e| ResolveError::StorageUnavailable(e.to_string()))?;
        let report = cards
            .into_iter()
            .find(|card| card.kind == "wave-report")
            .ok_or_else(|| ResolveError::ReportAbsent(wave_id.into()))?;
        let doc_rev = report
            .payload
            .get("docRev")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| ResolveError::MalformedStoredReport(wave_id.into()))?;
        // Fence baseline is captured before the first block in this wave is decoded.
        doc_revs.entry(wave_id.into()).or_insert(doc_rev);
        let values = report
            .payload
            .get("blocks")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok((wave.cove_id.to_string(), values))
    }

    async fn load_block(
        &self,
        wave_id: &str,
        block_id: &str,
        is_root: bool,
        doc_revs: &mut BTreeMap<String, u64>,
    ) -> std::result::Result<(String, ReportBlock), ResolveError> {
        let (cove, blocks) = self.report_snapshot(wave_id, doc_revs).await?;
        let value = blocks
            .into_iter()
            .find(|value| value.get("id").and_then(serde_json::Value::as_str) == Some(block_id))
            .ok_or_else(|| {
                if is_root {
                    ResolveError::RootAbsent
                } else {
                    ResolveError::ReferencedBlockAbsent(format!("{wave_id}#{block_id}"))
                }
            })?;
        let block = serde_json::from_value(value)
            .map_err(|_| ResolveError::MalformedStoredReport(wave_id.into()))?;
        Ok((cove, block))
    }

    async fn refs_match(&self, task_wave_id: &str, refs: &[TaskContextRef]) -> RefsMatch {
        let task_wave = match self.repo.wave_get(task_wave_id).await {
            Ok(Some(wave)) => wave,
            Ok(None) => return RefsMatch::Mismatch(Vec::new(), "referenced_wave_absent"),
            Err(error) => return RefsMatch::Retryable(error.to_string()),
        };
        let system = match self.repo.cove_get_system().await {
            Ok(system) => system,
            Err(error) => return RefsMatch::Retryable(error.to_string()),
        };
        for frozen in refs {
            let mut ignored_doc_revs = BTreeMap::new();
            let loaded = self
                .load_block(
                    frozen.wave_id.as_str(),
                    &frozen.block_id,
                    frozen.is_root,
                    &mut ignored_doc_revs,
                )
                .await;
            let (cove, block) = match loaded {
                Ok(value) => value,
                Err(ResolveError::StorageUnavailable(error)) => return RefsMatch::Retryable(error),
                Err(ResolveError::MalformedStoredReport(_)) => {
                    return RefsMatch::Mismatch(Vec::new(), "malformed_stored_report");
                }
                Err(error) => {
                    return RefsMatch::Mismatch(
                        vec![TaskContextChangedRef {
                            wave_id: frozen.wave_id.clone(),
                            block_id: frozen.block_id.clone(),
                            from_rev: frozen.rev,
                            from_hash: frozen.hash.clone(),
                            ..Default::default()
                        }],
                        error.variant(),
                    );
                }
            };
            if cove != task_wave.cove_id.as_str()
                && system.as_ref().map(|c| c.id.as_str()) != Some(cove.as_str())
            {
                return RefsMatch::Mismatch(
                    vec![TaskContextChangedRef {
                        wave_id: frozen.wave_id.clone(),
                        block_id: frozen.block_id.clone(),
                        from_rev: frozen.rev,
                        from_hash: frozen.hash.clone(),
                        ..Default::default()
                    }],
                    "cross_cove",
                );
            }
            let current = context_ref(frozen.wave_id.as_str(), &block, frozen.is_root);
            if current.wave_id != frozen.wave_id
                || current.block_id != frozen.block_id
                || current.hash != frozen.hash
            {
                return RefsMatch::Mismatch(
                    vec![TaskContextChangedRef {
                        wave_id: frozen.wave_id.clone(),
                        block_id: frozen.block_id.clone(),
                        from_rev: frozen.rev,
                        to_rev: current.rev,
                        from_hash: frozen.hash.clone(),
                        to_hash: current.hash,
                    }],
                    "content_changed",
                );
            }
        }
        RefsMatch::Same
    }

    pub async fn detect_wave_edit(&self, dst_wave_id: &str) -> Result<()> {
        let rows = self.repo.task_contexts_by_dst_wave(dst_wave_id).await?;
        let mut stale_rows = self
            .repo
            .stale_task_contexts_by_dst_wave(dst_wave_id)
            .await?;
        let fanout = rows.len().saturating_add(stale_rows.len());
        self.metrics
            .fanout_total
            .fetch_add(fanout as u64, Ordering::Relaxed);
        match fanout {
            0 => &self.metrics.fanout_zero,
            1..=8 => &self.metrics.fanout_one_to_eight,
            9..=MAX_RERESOLVE_FANOUT => &self.metrics.fanout_nine_to_sixty_four,
            _ => &self.metrics.fanout_over_limit,
        }
        .fetch_add(1, Ordering::Relaxed);
        for (index, row) in rows.into_iter().enumerate() {
            self.metrics.detections.fetch_add(1, Ordering::Relaxed);
            let verdict = if index >= MAX_RERESOLVE_FANOUT {
                Some((Vec::new(), "MAX_RERESOLVE_FANOUT budget exceeded"))
            } else if row.closure_truncated {
                Some((Vec::new(), "frozen reference closure was truncated"))
            } else if let Some(refs) = row
                .claim_context_json
                .as_deref()
                .and_then(|json| serde_json::from_str::<Vec<TaskContextRef>>(json).ok())
            {
                match self.refs_match(&row.wave_id, &refs).await {
                    RefsMatch::Mismatch(changed, variant) => {
                        self.metrics.record_context_resolve_failure(variant);
                        Some((changed, variant))
                    }
                    RefsMatch::Same => None,
                    RefsMatch::Retryable(error) => {
                        self.metrics
                            .record_context_resolve_failure("storage_unavailable");
                        tracing::warn!(task_id=%row.task_id, %error, "task context edit detection deferred after retryable resolution failure");
                        None
                    }
                }
            } else {
                Some((Vec::new(), "frozen reference set is missing or malformed"))
            };
            if let Some((changed_refs, rationale)) = verdict {
                self.mark_material(row.task_id, row.wave_id, changed_refs, rationale)
                    .await?;
            }
        }
        let cursor_scope = format!("event:{dst_wave_id}");
        self.rotate_stale_rows(&cursor_scope, &mut stale_rows);
        let stale_count = stale_rows.len();
        for row in stale_rows.into_iter().take(MAX_RERESOLVE_FANOUT) {
            self.metrics.detections.fetch_add(1, Ordering::Relaxed);
            let task_id = row.task_id.clone();
            self.attempt_restore(row).await?;
            self.advance_restore_cursor(&cursor_scope, &task_id);
        }
        for _ in MAX_RERESOLVE_FANOUT..stale_count {
            self.metrics
                .record_restore_deferred("fanout_budget_exceeded");
        }
        Ok(())
    }

    pub async fn sweep(&self) -> Result<()> {
        let started = Instant::now();
        let result = self.sweep_inner().await;
        match &result {
            Ok(outcome) => {
                self.metrics
                    .sweep_duration_ms
                    .store(started.elapsed().as_millis() as u64, Ordering::Relaxed);
                self.metrics
                    .sweep_verified_tuples
                    .store(outcome.verified_tuples as u64, Ordering::Relaxed);
                self.metrics
                    .sweep_hits
                    .store(outcome.hits as u64, Ordering::Relaxed);
                if outcome.capped {
                    self.metrics.sweep_caps.fetch_add(1, Ordering::Relaxed);
                }
                self.metrics
                    .sweep_restore_verified_tuples
                    .store(outcome.restore_verified_tuples as u64, Ordering::Relaxed);
                self.metrics
                    .sweep_restore_hits
                    .store(outcome.restore_hits as u64, Ordering::Relaxed);
                if outcome.restore_capped {
                    self.metrics
                        .sweep_restore_caps
                        .fetch_add(1, Ordering::Relaxed);
                }
                self.metrics
                    .last_success_ms
                    .store(now_ms(), Ordering::Relaxed);
                self.metrics
                    .consecutive_failures
                    .store(0, Ordering::Relaxed);
                tracing::info!(
                    duration_ms = started.elapsed().as_millis(),
                    verified_tuples = outcome.verified_tuples,
                    hits = outcome.hits,
                    capped = outcome.capped,
                    restore_verified_tuples = outcome.restore_verified_tuples,
                    restore_hits = outcome.restore_hits,
                    restore_capped = outcome.restore_capped,
                    "task context sweep completed"
                );
            }
            Err(_) => {
                self.metrics
                    .consecutive_failures
                    .fetch_add(1, Ordering::Relaxed);
            }
        }
        self.metrics.export();
        result.map(|_| ())
    }

    async fn sweep_inner(&self) -> Result<SweepOutcome> {
        let rows = self.repo.task_contexts_inflight_fresh().await?;
        let mut stale_rows = self.repo.task_contexts_inflight_stale().await?;
        let mut verified = 0usize;
        let mut restore_verified = 0usize;
        let mut hits = 0usize;
        let mut restore_hits = 0usize;
        let mut capped = false;
        let mut restore_capped = false;
        for row in rows {
            let refs = row
                .claim_context_json
                .as_deref()
                .and_then(|json| serde_json::from_str::<Vec<TaskContextRef>>(json).ok());
            let verdict = if row.closure_truncated {
                Some((Vec::new(), "frozen reference closure was truncated"))
            } else {
                match refs {
                    None => Some((Vec::new(), "frozen reference set is missing or malformed")),
                    Some(refs) if verified.saturating_add(refs.len()) > MAX_SWEEP_NODES => {
                        capped = true;
                        Some((Vec::new(), "MAX_SWEEP_NODES budget exceeded"))
                    }
                    Some(refs) => {
                        verified += refs.len();
                        match self.refs_match(&row.wave_id, &refs).await {
                            RefsMatch::Same => {
                                self.clear_verify_failures(&row.task_id).await?;
                                None
                            }
                            RefsMatch::Mismatch(changed, variant) => {
                                self.metrics.record_context_resolve_failure(variant);
                                Some((changed, variant))
                            }
                            RefsMatch::Retryable(error) => {
                                self.metrics
                                    .record_context_resolve_failure("storage_unavailable");
                                if self
                                    .record_verify_failure(&row.task_id, &row.wave_id)
                                    .await?
                                {
                                    tracing::warn!(task_id=%row.task_id, %error, "task context verification failed three consecutive sweeps; marking material");
                                    hits += 1;
                                }
                                continue;
                            }
                        }
                    }
                }
            };
            if let Some((changed_refs, rationale)) = verdict {
                hits += 1;
                self.mark_material(row.task_id, row.wave_id, changed_refs, rationale)
                    .await?;
            }
        }
        const SWEEP_CURSOR_SCOPE: &str = "sweep";
        self.rotate_stale_rows(SWEEP_CURSOR_SCOPE, &mut stale_rows);
        for row in stale_rows {
            let task_id = row.task_id.clone();
            let refs = row
                .claim_context_json
                .as_deref()
                .and_then(|json| serde_json::from_str::<Vec<TaskContextRef>>(json).ok());
            let Some(refs) = refs else {
                self.metrics
                    .record_restore_deferred("malformed_frozen_context");
                self.advance_restore_cursor(SWEEP_CURSOR_SCOPE, &task_id);
                continue;
            };
            if row.closure_truncated {
                self.metrics.record_restore_deferred("closure_truncated");
                self.advance_restore_cursor(SWEEP_CURSOR_SCOPE, &task_id);
                continue;
            }
            if restore_verified.saturating_add(refs.len()) > MAX_SWEEP_NODES {
                restore_capped = true;
                self.metrics
                    .record_restore_deferred("sweep_budget_exceeded");
                break;
            }
            restore_verified += refs.len();
            if self.attempt_restore(row).await? {
                restore_hits += 1;
            }
            self.advance_restore_cursor(SWEEP_CURSOR_SCOPE, &task_id);
        }
        self.cleanup_index().await?;
        Ok(SweepOutcome {
            verified_tuples: verified,
            hits,
            capped,
            restore_verified_tuples: restore_verified,
            restore_hits,
            restore_capped,
        })
    }

    async fn cleanup_index(&self) -> Result<()> {
        write_in_tx_typed(self.repo.as_ref(), |tx| {
            Box::pin(async move {
                sqlx::query(
                    "DELETE FROM task_ref_index WHERE task_id NOT IN \
                     (SELECT id FROM tasks WHERE status IN ('dispatched','running','verifying'))",
                )
                .execute(&mut **tx)
                .await?;
                Ok(())
            })
        })
        .await
    }

    async fn clear_verify_failures(&self, task_id: &str) -> Result<()> {
        let task_id = task_id.to_string();
        write_in_tx_typed(self.repo.as_ref(), move |tx| {
            Box::pin(async move {
                sqlx::query(
                    "UPDATE tasks SET context_verify_failures=0 WHERE id=?1 AND context_verify_failures!=0",
                )
                .bind(task_id)
                .execute(&mut **tx)
                .await?;
                Ok(())
            })
        })
        .await
    }

    /// Returns true when this retryable failure crossed the escalation limit.
    async fn record_verify_failure(&self, task_id: &str, wave_id: &str) -> Result<bool> {
        let task_id = task_id.to_string();
        let wave_id = wave_id.to_string();
        let failures: Option<i64> = write_in_tx_typed(self.repo.as_ref(), {
            let task_id = task_id.clone();
            move |tx| {
                Box::pin(async move {
                    Ok(sqlx::query_scalar(
                        "UPDATE tasks SET context_verify_failures=context_verify_failures+1 WHERE id=?1 AND status IN ('dispatched','running','verifying') AND context_stale_at_ms IS NULL RETURNING context_verify_failures",
                    )
                    .bind(task_id)
                    .fetch_optional(&mut **tx)
                    .await?)
                })
            }
        })
        .await?;
        let escalated = failures.is_some_and(|count| count >= VERIFY_FAILURE_LIMIT);
        if escalated {
            self.mark_material(
                task_id,
                wave_id,
                Vec::new(),
                "three consecutive context verification failures",
            )
            .await?;
        }
        Ok(escalated)
    }

    async fn attempt_restore(&self, row: calm_truth::db::TaskContextRow) -> Result<bool> {
        self.metrics.restore_checks.fetch_add(1, Ordering::Relaxed);
        if row.closure_truncated {
            self.metrics.record_restore_deferred("closure_truncated");
            return Ok(false);
        }
        let Some(refs) = row
            .claim_context_json
            .as_deref()
            .and_then(|json| serde_json::from_str::<Vec<TaskContextRef>>(json).ok())
        else {
            self.metrics
                .record_restore_deferred("malformed_frozen_context");
            return Ok(false);
        };
        match self.refs_match(&row.wave_id, &refs).await {
            RefsMatch::Mismatch(_, variant) => {
                self.metrics.record_restore_deferred(variant);
                return Ok(false);
            }
            RefsMatch::Retryable(error) => {
                self.metrics.record_restore_deferred("storage_unavailable");
                tracing::warn!(task_id=%row.task_id, %error, "task context restore deferred after retryable evidence failure");
                return Ok(false);
            }
            RefsMatch::Same => {}
        }

        let task_id = row.task_id;
        let wave_id = row.wave_id;
        let result = write_with_actor_events_typed(
            self.repo.as_ref(),
            None,
            &self.events,
            &self.write,
            move |tx| {
                Box::pin(async move {
                    let events = restore_context_tx(tx, &task_id, &wave_id).await?;
                    Ok(((true,), events))
                })
            },
        )
        .await;
        match result {
            Ok(_) => {
                self.metrics.restores.fetch_add(1, Ordering::Relaxed);
                Ok(true)
            }
            Err(CalmError::Conflict(message)) => {
                let Some(reason) = restore_deferred_reason(&message) else {
                    return Err(CalmError::Conflict(message));
                };
                self.metrics.record_restore_deferred(reason);
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }

    async fn mark_material(
        &self,
        task_id: String,
        wave_id: String,
        changed_refs: Vec<TaskContextChangedRef>,
        rationale: &'static str,
    ) -> Result<()> {
        let result = write_with_actor_events_typed(
            self.repo.as_ref(),
            None,
            &self.events,
            &self.write,
            move |tx| {
                Box::pin(async move {
                    if rationale == CONTENT_CHANGED_RATIONALE
                        && let Some(task) = frozen_task_tx(tx, &task_id, &wave_id).await?
                        && current_context_evidence_tx(tx, &task).await?
                            == CurrentContextEvidence::Equal
                    {
                        return Err(CalmError::Conflict(MATERIAL_VERDICT_OBSOLETE.into()));
                    }
                    let events =
                        mark_context_material_tx(tx, &task_id, &wave_id, changed_refs, rationale)
                            .await?;
                    let changed = !events.is_empty();
                    Ok(((changed,), events))
                })
            },
        )
        .await;
        let ((changed,), _) = match result {
            Ok(value) => value,
            Err(CalmError::Conflict(message)) if message == MATERIAL_VERDICT_OBSOLETE => {
                self.metrics
                    .material_verdict_obsolete
                    .fetch_add(1, Ordering::Relaxed);
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        if changed {
            self.metrics.hits.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    }
}

type FrozenTaskDbRow = (String, String, String, Option<String>, i64, i64, i64);

struct FrozenTaskTx {
    task_id: String,
    wave_id: String,
    task_key: String,
    status: String,
    cove_id: String,
    refs: Option<Vec<TaskContextRef>>,
    closure_truncated: bool,
    decl_ready: bool,
    decl_released_by_user: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CurrentContextEvidence {
    Equal,
    Mismatch,
    DeclarationWithdrawn,
}

async fn frozen_task_tx(
    tx: &mut Transaction<'_, Sqlite>,
    task_id: &str,
    wave_id: &str,
) -> Result<Option<FrozenTaskTx>> {
    let row: Option<FrozenTaskDbRow> = sqlx::query_as(
        "SELECT t.key,t.status,w.cove_id,t.claim_context_json,\
         t.context_closure_truncated,t.decl_ready,t.decl_released_by_user \
         FROM tasks t JOIN waves w ON w.id=t.wave_id WHERE t.id=?1 AND t.wave_id=?2",
    )
    .bind(task_id)
    .bind(wave_id)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.map(
        |(
            task_key,
            status,
            cove_id,
            claim_context_json,
            closure_truncated,
            decl_ready,
            decl_released_by_user,
        )| FrozenTaskTx {
            task_id: task_id.into(),
            wave_id: wave_id.into(),
            task_key,
            status,
            cove_id,
            refs: claim_context_json
                .as_deref()
                .and_then(|json| serde_json::from_str(json).ok()),
            closure_truncated: closure_truncated != 0,
            decl_ready: decl_ready != 0,
            decl_released_by_user: decl_released_by_user != 0,
        },
    ))
}

async fn current_context_evidence_tx(
    tx: &mut Transaction<'_, Sqlite>,
    task: &FrozenTaskTx,
) -> Result<CurrentContextEvidence> {
    if task.closure_truncated {
        return Ok(CurrentContextEvidence::Mismatch);
    }
    let Some(refs) = task.refs.as_deref() else {
        return Ok(CurrentContextEvidence::Mismatch);
    };
    let system_cove: Option<String> =
        sqlx::query_scalar("SELECT id FROM coves WHERE kind='system' LIMIT 1")
            .fetch_optional(&mut **tx)
            .await?;
    let mut saw_root = false;
    for frozen in refs {
        let report: Option<(String, String)> = sqlx::query_as(
            "SELECT w.cove_id,c.payload FROM waves w \
             JOIN cards c ON c.wave_id=w.id AND c.kind='wave-report' WHERE w.id=?1",
        )
        .bind(frozen.wave_id.as_str())
        .fetch_optional(&mut **tx)
        .await?;
        let Some((current_cove, payload)) = report else {
            return Ok(CurrentContextEvidence::Mismatch);
        };
        if current_cove != task.cove_id && system_cove.as_deref() != Some(current_cove.as_str()) {
            return Ok(CurrentContextEvidence::Mismatch);
        }
        let Ok(report) =
            serde_json::from_str::<calm_types::wave_report::WaveReportPayload>(&payload)
        else {
            return Ok(CurrentContextEvidence::Mismatch);
        };
        let Some(block) = report
            .blocks
            .unwrap_or_default()
            .into_iter()
            .find(|block| block.id == frozen.block_id)
        else {
            return Ok(CurrentContextEvidence::Mismatch);
        };
        let current = context_ref(frozen.wave_id.as_str(), &block, frozen.is_root);
        if current.wave_id != frozen.wave_id
            || current.block_id != frozen.block_id
            || current.hash != frozen.hash
        {
            return Ok(CurrentContextEvidence::Mismatch);
        }
        if frozen.is_root {
            saw_root = true;
            let payload = &block.payload;
            if payload.get("key").and_then(serde_json::Value::as_str)
                != Some(task.task_key.as_str())
                || payload
                    .get("tombstone")
                    .is_some_and(serde_json::Value::is_object)
                || (task.decl_ready
                    && !payload
                        .get("ready")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false))
                || (task.decl_released_by_user
                    && !payload
                        .get("released_by_user")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false))
            {
                return Ok(CurrentContextEvidence::DeclarationWithdrawn);
            }
        }
    }
    if !saw_root {
        return Ok(CurrentContextEvidence::Mismatch);
    }
    Ok(CurrentContextEvidence::Equal)
}

async fn restore_context_tx(
    tx: &mut Transaction<'_, Sqlite>,
    task_id: &str,
    wave_id: &str,
) -> Result<Vec<(ActorId, EventScope, Event)>> {
    let Some(task) = frozen_task_tx(tx, task_id, wave_id).await? else {
        return Err(CalmError::Conflict(RESTORE_NOT_ELIGIBLE.into()));
    };
    if !matches!(task.status.as_str(), "dispatched" | "running" | "verifying") {
        return Err(CalmError::Conflict(RESTORE_NOT_ELIGIBLE.into()));
    }
    match current_context_evidence_tx(tx, &task).await? {
        CurrentContextEvidence::Equal => {}
        CurrentContextEvidence::Mismatch => {
            return Err(CalmError::Conflict(RESTORE_EVIDENCE_CHANGED.into()));
        }
        CurrentContextEvidence::DeclarationWithdrawn => {
            return Err(CalmError::Conflict(RESTORE_DECLARATION_WITHDRAWN.into()));
        }
    }
    let changed = sqlx::query(
        "UPDATE tasks SET context_stale_at_ms=NULL WHERE id=?1 AND wave_id=?2 \
         AND context_stale_at_ms IS NOT NULL",
    )
    .bind(task_id)
    .bind(wave_id)
    .execute(&mut **tx)
    .await?
    .rows_affected();
    if changed != 1 {
        return Err(CalmError::Conflict(RESTORE_NOT_ELIGIBLE.into()));
    }
    Ok(vec![(
        ActorId::Kernel,
        EventScope::Wave {
            wave: WaveId::from(task.wave_id.as_str()),
            cove: task.cove_id.into(),
        },
        Event::TaskContextAdvanced {
            wave_id: WaveId::from(task.wave_id.as_str()),
            task_key: task.task_key,
            task_id: task.task_id,
            changed_refs: Vec::new(),
            verdict: "restored".into(),
            rationale: RESTORED_RATIONALE.into(),
        },
    )])
}

fn restore_deferred_reason(message: &str) -> Option<&'static str> {
    match message {
        RESTORE_NOT_ELIGIBLE => Some("not_eligible"),
        RESTORE_EVIDENCE_CHANGED => Some("transaction_evidence_changed"),
        RESTORE_DECLARATION_WITHDRAWN => Some("declaration_withdrawn"),
        _ => None,
    }
}

pub(crate) fn context_ref(wave_id: &str, block: &ReportBlock, is_root: bool) -> TaskContextRef {
    let content = if is_root {
        task_root_projection(&block.payload)
    } else {
        flat_text(block)
    };
    TaskContextRef {
        wave_id: WaveId::from(wave_id),
        block_id: block.id.clone(),
        rev: i64::from(block.rev),
        hash: format!("{:x}", Sha256::digest(content.as_bytes())),
        is_root,
    }
}

fn task_root_projection(payload: &serde_json::Value) -> String {
    let mut projected = serde_json::Map::new();
    if let Some(object) = payload.as_object() {
        for key in ROOT_HASH_TASK_FIELDS {
            if let Some(value) = object.get(*key).filter(|value| !value.is_null()) {
                projected.insert((*key).into(), value.clone());
            }
        }
    }
    canonical_json(&serde_json::Value::Object(projected))
}

fn block_links(block: &ReportBlock) -> std::result::Result<Vec<(String, String)>, ResolveError> {
    let mut links = Vec::new();
    if let Some(explicit) = block
        .payload
        .get("refs")
        .and_then(serde_json::Value::as_array)
    {
        for value in explicit {
            let raw = value
                .as_str()
                .ok_or_else(|| ResolveError::InvalidReference(value.to_string()))?;
            let (wave, block) = parse_destination(raw)
                .filter(|(_, block)| block.is_some())
                .ok_or_else(|| ResolveError::InvalidReference(raw.into()))?;
            links.push((wave, block.expect("filtered Some")));
        }
    }
    for text in scannable_text_fields(&block.kind, &block.payload) {
        for link in scan_links(text).links {
            if let Some(block) = link.dst_block_id {
                links.push((link.dst_wave_id, block));
            }
        }
    }
    Ok(links)
}

pub async fn sweep_with_timeout(monitor: &TaskContextMonitor, timeout: Duration) -> Result<()> {
    tokio::time::timeout(timeout, monitor.sweep())
        .await
        .map_err(|_| CalmError::Internal("task context sweep timed out".into()))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use calm_types::report_blocks::TASK_FIELDS;

    use crate::db::sqlite::{SqlxRepo, begin_immediate_tx};
    use crate::db::{Repo, ServerRepoSyncDomainRawExt};
    use crate::model::{NewCard, NewCove, NewWave, RequestTheme};
    use calm_types::wave_report::WaveReportPayload;

    async fn seed_restore_transaction_fixture(
        status: &str,
        stale_at_ms: Option<i64>,
    ) -> (SqlxRepo, String, String) {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let cove = repo
            .cove_create(NewCove {
                name: format!("restore-guard-{status}"),
                color: "#000".into(),
                sort: None,
            })
            .await
            .unwrap();
        let wave = repo
            .wave_create(NewWave {
                workflow_input: None,
                cove_id: cove.id,
                title: format!("restore-guard-{status}"),
                sort: None,
                cwd: String::new(),
                workflow_id: None,
                plugin_scope: None,
                attach_folder: false,
                theme: RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        let root = ReportBlock {
            id: "b_restore_guard".into(),
            kind: "task".into(),
            rev: 1,
            payload: serde_json::json!({
                "key": "terminal",
                "kind": "terminal",
                "goal": "true",
                "ready": true,
                "declared_by": "spec",
            }),
        };
        let frozen = context_ref(wave.id.as_str(), &root, true);
        let report_card = repo
            .card_create(NewCard {
                wave_id: wave.id.clone(),
                kind: "wave-report".into(),
                sort: None,
                payload: serde_json::to_value(WaveReportPayload::initial()).unwrap(),
                title: None,
            })
            .await
            .unwrap();
        let task_id = format!("{}:terminal", wave.id);
        let pool = repo.sqlite_pool().unwrap();
        sqlx::query("UPDATE cards SET payload=?1 WHERE id=?2")
            .bind(
                serde_json::to_string(&WaveReportPayload {
                    schema_version: 3,
                    doc_rev: 1,
                    summary: String::new(),
                    body: String::new(),
                    blocks: Some(vec![root]),
                })
                .unwrap(),
            )
            .bind(report_card.id.as_str())
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO tasks \
             (id,wave_id,key,kind,goal,context_json,depends_on_json,priority,status,\
              declared_by,claim_context_json,context_stale_at_ms,\
              context_closure_truncated,decl_ready,decl_released_by_user,\
              context_verify_failures,spawn,created_at_ms,updated_at_ms) \
             VALUES (?1,?2,'terminal','terminal','true','null','[]',0,?3,\
                     'spec',?4,?5,0,1,0,0,'in-wave',1,1)",
        )
        .bind(&task_id)
        .bind(wave.id.as_str())
        .bind(status)
        .bind(serde_json::to_string(&vec![frozen]).unwrap())
        .bind(stale_at_ms)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO events(kind,payload,actor,at,event_version,scope_kind,scope_wave) \
             VALUES('task.context_advanced',?1,'kernel',1,12,'wave',?2)",
        )
        .bind(
            serde_json::json!({
                "task_id": task_id,
                "verdict": "material",
                "rationale": "content_changed",
            })
            .to_string(),
        )
        .bind(wave.id.as_str())
        .execute(&pool)
        .await
        .unwrap();
        (repo, wave.id.to_string(), task_id)
    }

    #[test]
    fn health_export_contains_positive_sweep_signals() {
        let metrics = ContextMetrics::default();
        metrics.last_success_ms.store(now_ms(), Ordering::Relaxed);
        metrics.consecutive_failures.store(3, Ordering::Relaxed);
        metrics.record_context_resolve_failure("malformed_stored_report");

        let exported = metrics.export();

        assert_ne!(exported.last_success_age_seconds, u64::MAX);
        assert_eq!(exported.consecutive_failures, 3);
        assert_eq!(
            exported.context_resolve_failures["malformed_stored_report"], 1,
            "the irreversible malformed-report verdict must have a named health bucket"
        );
    }

    #[tokio::test]
    async fn restore_transaction_rejects_terminal_candidate_even_if_called_directly() {
        let (repo, wave_id, task_id) = seed_restore_transaction_fixture("failed", Some(1)).await;
        let pool = repo.sqlite_pool().unwrap();

        let mut tx = begin_immediate_tx(&pool).await.unwrap();
        let error = restore_context_tx(&mut tx, &task_id, &wave_id)
            .await
            .expect_err("the transaction guard must reject a terminal row from any caller");
        assert!(
            matches!(error, CalmError::Conflict(ref message) if message == RESTORE_NOT_ELIGIBLE),
            "the direct caller must be rejected by the terminal/stale eligibility guard: {error}"
        );
        tx.rollback().await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, Option<i64>>(
                "SELECT context_stale_at_ms FROM tasks WHERE id=?1"
            )
            .bind(&task_id)
            .fetch_one(&pool)
            .await
            .unwrap(),
            Some(1)
        );
    }

    #[tokio::test]
    async fn restore_update_rejects_a_fresh_candidate_even_after_equal_evidence() {
        let (repo, wave_id, task_id) = seed_restore_transaction_fixture("running", None).await;
        let pool = repo.sqlite_pool().unwrap();
        let mut tx = begin_immediate_tx(&pool).await.unwrap();
        let error = restore_context_tx(&mut tx, &task_id, &wave_id)
            .await
            .expect_err("the conditional update must reject a row that is no longer stale");
        assert!(
            matches!(error, CalmError::Conflict(ref message) if message == RESTORE_NOT_ELIGIBLE),
            "the conditional update must own the stale-level guard: {error}"
        );
        tx.rollback().await.unwrap();
    }

    #[test]
    fn task_root_hash_field_partition_covers_task_fields() {
        let classified: BTreeSet<_> = ROOT_HASH_TASK_FIELDS
            .iter()
            .chain(ROOT_HASH_EXCLUDED_TASK_FIELDS)
            .copied()
            .collect();
        let all: BTreeSet<_> = TASK_FIELDS.iter().copied().collect();
        assert_eq!(classified, all);
        assert_eq!(
            classified.len(),
            ROOT_HASH_TASK_FIELDS.len() + ROOT_HASH_EXCLUDED_TASK_FIELDS.len()
        );
    }

    #[test]
    fn projection_drift_fields_equal_hashed_stored_fields() {
        let hashed: BTreeSet<_> = ROOT_HASH_TASK_FIELDS.iter().copied().collect();
        let drift: BTreeSet<_> = crate::db::sqlite::PROJECTION_DRIFT_TASK_FIELDS
            .iter()
            .copied()
            .collect();
        let expected = hashed
            .difference(&BTreeSet::from(["refs", "no_gate_reason"]))
            .copied()
            .collect();
        assert_eq!(drift, expected);
    }

    #[test]
    fn withdrawal_diagnostic_paths_match_the_exhaustive_task_set() {
        let actual: BTreeSet<_> = calm_types::report_blocks::tasks::TASK_BLOCKING_DIAGNOSTIC_PATHS
            .iter()
            .copied()
            .collect();
        let expected = BTreeSet::from(["depends_on", "gate", "key", "payload", "refs"]);
        assert_eq!(actual, expected);
    }

    #[test]
    fn root_hash_only_tracks_included_fields_while_child_hashes_whole_block() {
        let block = ReportBlock {
            id: "b_root".into(),
            kind: "task".into(),
            rev: 1,
            payload: serde_json::json!({
                "key": "build", "kind": "codex", "goal": "old", "priority": 0,
                "declared_by": "spec", "spawn": null, "released_by_user": false
            }),
        };
        let root = context_ref("w", &block, true);
        for (field, value) in [
            ("goal", serde_json::json!("new")),
            ("kind", serde_json::json!("terminal")),
            ("gate", serde_json::json!({"cmd": "cargo test"})),
            ("refs", serde_json::json!(["neige://wave/w#b_child"])),
            ("no_gate_reason", serde_json::json!("not required")),
        ] {
            let mut changed = block.clone();
            changed.payload[field] = value;
            assert_ne!(context_ref("w", &changed, true).hash, root.hash, "{field}");
        }
        for (field, value) in [
            ("priority", serde_json::json!(1)),
            ("declared_by", serde_json::json!("user")),
            ("spawn", serde_json::json!({"future": true})),
            ("released_by_user", serde_json::json!(true)),
        ] {
            let mut changed = block.clone();
            changed.payload[field] = value;
            assert_eq!(context_ref("w", &changed, true).hash, root.hash, "{field}");
            assert_ne!(
                context_ref("w", &changed, false).hash,
                context_ref("w", &block, false).hash,
                "child {field} must retain whole-block hashing"
            );
        }
    }

    #[test]
    fn root_projection_treats_missing_and_null_as_equal() {
        let mut absent = ReportBlock {
            id: "b".into(),
            kind: "task".into(),
            rev: 1,
            payload: serde_json::json!({"key": "k", "kind": "codex", "goal": "g"}),
        };
        let baseline = context_ref("w", &absent, true).hash;
        absent.payload["context"] = serde_json::Value::Null;
        assert_eq!(context_ref("w", &absent, true).hash, baseline);
    }

    #[test]
    fn frozen_root_projection_bytes_and_hash_are_version_stable() {
        let block = ReportBlock {
            id: "b_golden".into(),
            kind: "task".into(),
            rev: 7,
            payload: serde_json::json!({
                "key": "excluded-key",
                "kind": "codex",
                "goal": "ship",
                "acceptance": "green",
                "depends_on": ["alpha"],
                "refs": ["neige://wave/w#b_child"],
                "cwd": "/repo",
                "context": {"z": 1, "a": 2},
                "priority": 9,
                "declared_by": "spec",
                "ready": true,
                "released_by_user": true,
            }),
        };
        let projection = task_root_projection(&block.payload);
        assert_eq!(
            projection,
            r#"{
  "acceptance": "green",
  "context": {
    "a": 2,
    "z": 1
  },
  "cwd": "/repo",
  "depends_on": ["alpha"],
  "goal": "ship",
  "kind": "codex",
  "refs": ["neige://wave/w#b_child"]
}"#
        );
        assert_eq!(
            context_ref("w", &block, true).hash,
            "d914beed029c5ce2775bb930f45f2fccf1f011cd2bc3b878ce7b0c9f588305ff",
            "persisted claim hashes are a cross-version compatibility boundary"
        );
    }

    #[test]
    fn frozen_root_gate_branches_and_child_bytes_and_hashes_are_version_stable() {
        let base = serde_json::json!({
            "key": "excluded-key",
            "kind": "codex",
            "goal": "ship",
            "acceptance": "green",
            "depends_on": ["alpha"],
            "refs": ["neige://wave/w#b_child"],
            "cwd": "/repo",
            "context": {"z": 1, "a": 2},
        });
        for (field, value, expected_projection, expected_hash) in [
            (
                "gate",
                serde_json::json!({"cmd": "cargo test"}),
                "{\n  \"acceptance\": \"green\",\n  \"context\": {\n    \"a\": 2,\n    \"z\": 1\n  },\n  \"cwd\": \"/repo\",\n  \"depends_on\": [\"alpha\"],\n  \"gate\": {\n    \"cmd\": \"cargo test\"\n  },\n  \"goal\": \"ship\",\n  \"kind\": \"codex\",\n  \"refs\": [\"neige://wave/w#b_child\"]\n}",
                "7706e6f5c0a597613e4a765b5240d04d1095394929eb4f6c420029a3787864ed",
            ),
            (
                "no_gate_reason",
                serde_json::json!("not required"),
                "{\n  \"acceptance\": \"green\",\n  \"context\": {\n    \"a\": 2,\n    \"z\": 1\n  },\n  \"cwd\": \"/repo\",\n  \"depends_on\": [\"alpha\"],\n  \"goal\": \"ship\",\n  \"kind\": \"codex\",\n  \"no_gate_reason\": \"not required\",\n  \"refs\": [\"neige://wave/w#b_child\"]\n}",
                "1713b5457e2b56c76197371fac747a298d94567c2a9e20c3f668692d98288f2f",
            ),
        ] {
            let mut payload = base.clone();
            payload[field] = value;
            let root = ReportBlock {
                id: "b_root_golden".into(),
                kind: "task".into(),
                rev: 7,
                payload,
            };
            assert_eq!(
                task_root_projection(&root.payload),
                expected_projection,
                "{field}"
            );
            assert_eq!(context_ref("w", &root, true).hash, expected_hash, "{field}");
        }

        let child = ReportBlock {
            id: "b_child_golden".into(),
            kind: "prose".into(),
            rev: 11,
            payload: serde_json::json!({"markdown": "referenced original\n\n"}),
        };
        assert_eq!(flat_text(&child), "referenced original\n\n");
        assert_eq!(
            context_ref("w", &child, false).hash,
            "7d28538cbed4b590a7776ecfc7023f8b1e5897753d03f4dae24779563066e292",
            "persisted non-root flat_text hashes are the same compatibility boundary as roots"
        );
    }
}
