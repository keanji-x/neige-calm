//! Frozen report-block closures and fail-closed stale-context detection.

use std::collections::{BTreeSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use calm_types::event::TaskContextRef;
use calm_types::report_blocks::{flat_text, scannable_text_fields};
use calm_types::report_links::{parse_destination, scan_links};
use calm_types::wave_report::ReportBlock;
use sha2::{Digest, Sha256};

use crate::db::{Repo, write_in_tx_typed, write_with_actor_events_typed};
use crate::error::{CalmError, Result};
use crate::event::{Event, EventBus, EventScope};
use crate::ids::{ActorId, WaveId};
use crate::model::now_ms;
use crate::state::WriteContext;

pub const MAX_REF_DEPTH: usize = 3;
pub const MAX_REF_NODES: usize = 64;
pub const MAX_RERESOLVE_FANOUT: usize = 64;
pub const MAX_SWEEP_NODES: usize = 4096;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FrozenClosure {
    pub refs: Vec<TaskContextRef>,
    pub closure_truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolveError {
    Missing(String),
    CrossCove(String),
    InvalidReference(String),
    Storage(String),
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
    last_success_ms: AtomicI64,
    consecutive_failures: AtomicU64,
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
    pub last_success_age_seconds: u64,
    pub consecutive_failures: u64,
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
            last_success_age_seconds: if last <= 0 {
                u64::MAX
            } else {
                now_ms().saturating_sub(last) as u64 / 1000
            },
            consecutive_failures: self.consecutive_failures.load(Ordering::Relaxed),
        }
    }
}

pub struct TaskContextMonitor {
    repo: Arc<dyn Repo>,
    events: EventBus,
    write: WriteContext,
    metrics: Arc<ContextMetrics>,
}

impl TaskContextMonitor {
    pub fn new(repo: Arc<dyn Repo>, events: EventBus, write: WriteContext) -> Self {
        Self {
            repo,
            events,
            write,
            metrics: Arc::new(ContextMetrics::default()),
        }
    }

    pub fn metrics(&self) -> Arc<ContextMetrics> {
        Arc::clone(&self.metrics)
    }

    pub async fn resolve_closure(
        &self,
        task_wave_id: &str,
        root_block_id: &str,
    ) -> std::result::Result<FrozenClosure, ResolveError> {
        let task_wave = self
            .repo
            .wave_get(task_wave_id)
            .await
            .map_err(|e| ResolveError::Storage(e.to_string()))?
            .ok_or_else(|| ResolveError::Missing(task_wave_id.into()))?;
        let system_cove = self
            .repo
            .cove_get_system()
            .await
            .map_err(|e| ResolveError::Storage(e.to_string()))?
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
            let (cove_id, block) = self.load_block(&wave_id, &block_id).await?;
            if cove_id != task_wave.cove_id.as_str()
                && system_cove.as_deref() != Some(cove_id.as_str())
            {
                return Err(ResolveError::CrossCove(format!("{wave_id}#{block_id}")));
            }
            refs.push(context_ref(&wave_id, &block));
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
            closure_truncated: truncated,
        })
    }

    async fn load_block(
        &self,
        wave_id: &str,
        block_id: &str,
    ) -> std::result::Result<(String, ReportBlock), ResolveError> {
        let wave = self
            .repo
            .wave_get(wave_id)
            .await
            .map_err(|e| ResolveError::Storage(e.to_string()))?
            .ok_or_else(|| ResolveError::Missing(wave_id.into()))?;
        let cards = self
            .repo
            .cards_by_wave(wave_id)
            .await
            .map_err(|e| ResolveError::Storage(e.to_string()))?;
        let report = cards
            .into_iter()
            .find(|card| card.kind == "wave-report")
            .ok_or_else(|| ResolveError::Missing(format!("report:{wave_id}")))?;
        let blocks = report
            .payload
            .get("blocks")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| ResolveError::Missing(format!("blocks:{wave_id}")))?;
        let block = blocks
            .iter()
            .find_map(|value| serde_json::from_value::<ReportBlock>(value.clone()).ok())
            .filter(|block| block.id == block_id)
            .or_else(|| {
                blocks.iter().find_map(|value| {
                    let block = serde_json::from_value::<ReportBlock>(value.clone()).ok()?;
                    (block.id == block_id).then_some(block)
                })
            })
            .ok_or_else(|| ResolveError::Missing(format!("{wave_id}#{block_id}")))?;
        Ok((wave.cove_id.to_string(), block))
    }

    async fn refs_match(&self, task_wave_id: &str, refs: &[TaskContextRef]) -> bool {
        let task_wave = match self.repo.wave_get(task_wave_id).await {
            Ok(Some(wave)) => wave,
            _ => return false,
        };
        let system = self.repo.cove_get_system().await.ok().flatten();
        for frozen in refs {
            let Ok((cove, block)) = self
                .load_block(frozen.wave_id.as_str(), &frozen.block_id)
                .await
            else {
                return false;
            };
            if cove != task_wave.cove_id.as_str()
                && system.as_ref().map(|c| c.id.as_str()) != Some(cove.as_str())
            {
                return false;
            }
            let current = context_ref(frozen.wave_id.as_str(), &block);
            if &current != frozen {
                return false;
            }
        }
        true
    }

    pub async fn detect_wave_edit(&self, dst_wave_id: &str) -> Result<()> {
        let rows = self.repo.task_contexts_by_dst_wave(dst_wave_id).await?;
        self.metrics
            .fanout_total
            .fetch_add(rows.len() as u64, Ordering::Relaxed);
        match rows.len() {
            0 => &self.metrics.fanout_zero,
            1..=8 => &self.metrics.fanout_one_to_eight,
            9..=MAX_RERESOLVE_FANOUT => &self.metrics.fanout_nine_to_sixty_four,
            _ => &self.metrics.fanout_over_limit,
        }
        .fetch_add(1, Ordering::Relaxed);
        for (index, row) in rows.into_iter().enumerate() {
            self.metrics.detections.fetch_add(1, Ordering::Relaxed);
            let material = if index >= MAX_RERESOLVE_FANOUT || row.closure_truncated {
                true
            } else if let Some(refs) = row
                .claim_context_json
                .as_deref()
                .and_then(|json| serde_json::from_str::<Vec<TaskContextRef>>(json).ok())
            {
                !self.refs_match(&row.wave_id, &refs).await
            } else {
                true
            };
            if material {
                self.mark_material(row.task_id, row.wave_id).await?;
            }
        }
        Ok(())
    }

    pub async fn sweep(&self) -> Result<()> {
        let started = Instant::now();
        let result = self.sweep_inner().await;
        match &result {
            Ok((verified, hits, capped)) => {
                self.metrics
                    .sweep_duration_ms
                    .store(started.elapsed().as_millis() as u64, Ordering::Relaxed);
                self.metrics
                    .sweep_verified_tuples
                    .store(*verified as u64, Ordering::Relaxed);
                self.metrics
                    .sweep_hits
                    .store(*hits as u64, Ordering::Relaxed);
                if *capped {
                    self.metrics.sweep_caps.fetch_add(1, Ordering::Relaxed);
                }
                self.metrics
                    .last_success_ms
                    .store(now_ms(), Ordering::Relaxed);
                self.metrics
                    .consecutive_failures
                    .store(0, Ordering::Relaxed);
                tracing::info!(
                    duration_ms = started.elapsed().as_millis(),
                    verified_tuples = *verified,
                    hits = *hits,
                    capped = *capped,
                    "task context sweep completed"
                );
            }
            Err(_) => {
                self.metrics
                    .consecutive_failures
                    .fetch_add(1, Ordering::Relaxed);
            }
        }
        result.map(|_| ())
    }

    async fn sweep_inner(&self) -> Result<(usize, usize, bool)> {
        let rows = self.repo.task_contexts_inflight_fresh().await?;
        let mut verified = 0usize;
        let mut hits = 0usize;
        let mut capped = false;
        for row in rows {
            let refs = row
                .claim_context_json
                .as_deref()
                .and_then(|json| serde_json::from_str::<Vec<TaskContextRef>>(json).ok());
            let material = match refs {
                None => true,
                Some(_) if row.closure_truncated => true,
                Some(refs) if verified.saturating_add(refs.len()) > MAX_SWEEP_NODES => {
                    capped = true;
                    true
                }
                Some(refs) => {
                    verified += refs.len();
                    !self.refs_match(&row.wave_id, &refs).await
                }
            };
            if material {
                hits += 1;
                self.mark_material(row.task_id, row.wave_id).await?;
            }
        }
        self.cleanup_index().await?;
        Ok((verified, hits, capped))
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

    async fn mark_material(&self, task_id: String, wave_id: String) -> Result<()> {
        let wave = self.repo.wave_get(&wave_id).await?;
        let Some(wave) = wave else { return Ok(()) };
        let scope = EventScope::Wave {
            wave: WaveId::from(wave_id),
            cove: wave.cove_id,
        };
        let event_task_id = task_id.clone();
        let result = write_with_actor_events_typed(
            self.repo.as_ref(),
            None,
            &self.events,
            &self.write,
            move |tx| {
                Box::pin(async move {
                    let changed = sqlx::query(
                        "UPDATE tasks SET context_stale_at_ms = ?1 \
                         WHERE id = ?2 AND status IN ('dispatched','running','verifying') \
                         AND context_stale_at_ms IS NULL",
                    )
                    .bind(now_ms())
                    .bind(&task_id)
                    .execute(&mut **tx)
                    .await?
                    .rows_affected()
                        != 0;
                    if !changed {
                        return Err(CalmError::Conflict("context-already-stale".into()));
                    }
                    Ok((
                        (),
                        vec![(
                            ActorId::Kernel,
                            scope,
                            Event::TaskContextAdvanced {
                                task_id: event_task_id,
                                verdict: "material".into(),
                            },
                        )],
                    ))
                })
            },
        )
        .await;
        match result {
            Ok(_) => {
                self.metrics.hits.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            Err(CalmError::Conflict(message)) if message == "context-already-stale" => Ok(()),
            Err(error) => Err(error),
        }
    }
}

fn context_ref(wave_id: &str, block: &ReportBlock) -> TaskContextRef {
    TaskContextRef {
        wave_id: WaveId::from(wave_id),
        block_id: block.id.clone(),
        rev: i64::from(block.rev),
        hash: format!("{:x}", Sha256::digest(flat_text(block).as_bytes())),
    }
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
