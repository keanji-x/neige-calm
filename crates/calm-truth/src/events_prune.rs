//! Background retention pruner for the `events` table: deletes rows matching
//! an exact-kind allowlist AND an age horizon, keeping the `MAX(id)`
//! `overlay.set` row per quad so the last-writer-wins fold is invariant under
//! pruning. Every DELETE advances a durable watermark the WS replay guard
//! uses to force `_snapshot_required`. Never VACUUMs.

use crate::db::sqlite::begin_immediate_tx;
use crate::error::Result;
use crate::model::now_ms;
use sqlx::{Sqlite, SqlitePool, Transaction};
use std::collections::BTreeMap;
use std::time::Duration;

const EVENTS_PRUNE_INTERVAL_SECS_ENV: &str = "NEIGE_EVENTS_PRUNE_INTERVAL_SECS";
const EVENTS_RETENTION_SECS_ENV: &str = "NEIGE_EVENTS_RETENTION_SECS";
const EVENTS_PRUNE_BATCH_ENV: &str = "NEIGE_EVENTS_PRUNE_BATCH";
const EVENTS_PRUNE_INTERVAL: Duration = Duration::from_secs(60 * 60);
const DEFAULT_EVENTS_RETENTION: Duration = Duration::from_secs(30 * 24 * 60 * 60);
/// Floor on the retention horizon: a seconds-vs-days typo must not wipe all
/// allowlisted history.
const MIN_EVENTS_RETENTION: Duration = Duration::from_secs(24 * 60 * 60);
const DEFAULT_EVENTS_PRUNE_BATCH: i64 = 5000;
/// Pause between per-batch write transactions so the pruner never
/// monopolizes SQLite's single writer slot on a bloated first pass.
const BATCH_YIELD: Duration = Duration::from_millis(100);

/// `retention_meta` key holding the highest `events.id` ever pruned.
pub const EVENTS_PRUNE_WATERMARK_KEY: &str = "events_prune_watermark";

/// Exact-kind allowlist; everything else is permanent by construction.
/// `harness.user_message.enqueued` must never be added: it is the only
/// evidence a user message was accepted into a runtime's queue, and pruning it
/// would re-send the Today bootstrap after the horizon.
pub const EVENTS_PRUNE_KINDS: &[&str] = &[
    "claude.hook",
    "codex.hook",
    "harness.phase.changed",
    "harness.item.added",
    "harness.queue.changed",
    "overlay.set",
];

/// One retention rule: prunable kinds plus whether the keep-latest-per-quad
/// carve-out applies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetentionRule {
    pub kinds: Vec<&'static str>,
    pub keep_latest_per_overlay_key: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventsRetentionPolicy {
    pub horizon: Duration,
    pub batch: i64,
    pub rules: Vec<RetentionRule>,
}

impl Default for EventsRetentionPolicy {
    fn default() -> Self {
        Self {
            horizon: DEFAULT_EVENTS_RETENTION,
            batch: DEFAULT_EVENTS_PRUNE_BATCH,
            rules: vec![RetentionRule {
                kinds: EVENTS_PRUNE_KINDS.to_vec(),
                keep_latest_per_overlay_key: true,
            }],
        }
    }
}

/// Spawn the events retention pruner. On by default (hourly interval,
/// 30-day horizon); `NEIGE_EVENTS_PRUNE_INTERVAL_SECS=0` disables it.
pub fn spawn_events_pruner(pool: SqlitePool) {
    let Some((interval, policy)) = events_pruner_config_from_env() else {
        tracing::info!("events_prune: retention pruner disabled");
        return;
    };

    tokio::spawn(async move {
        let mut tick = tokio::time::interval(interval);
        // Skip the immediate boot tick so the server settles before taking writer locks.
        tick.tick().await;
        loop {
            tick.tick().await;
            if let Err(e) = prune_events_once(&pool, &policy).await {
                tracing::warn!(error = %e, "events_prune: pass failed");
            }
        }
    });
}

fn events_pruner_config_from_env() -> Option<(Duration, EventsRetentionPolicy)> {
    let interval = match std::env::var(EVENTS_PRUNE_INTERVAL_SECS_ENV) {
        Ok(raw) => match raw.trim().parse::<u64>() {
            Ok(0) => return None,
            Ok(secs) => Duration::from_secs(secs),
            Err(_) => {
                // Unparseable is NOT a disable switch: the failure direction of this knob
                // is data deletion.
                tracing::warn!(
                    raw,
                    "events_prune: unparseable {EVENTS_PRUNE_INTERVAL_SECS_ENV}; \
                     pruner stays ON with the default interval (set it to 0 to disable)"
                );
                EVENTS_PRUNE_INTERVAL
            }
        },
        Err(_) => EVENTS_PRUNE_INTERVAL,
    };
    let horizon = match std::env::var(EVENTS_RETENTION_SECS_ENV) {
        Ok(raw) => match raw.trim().parse::<u64>() {
            Ok(0) => {
                tracing::warn!(
                    "events_prune: {EVENTS_RETENTION_SECS_ENV}=0 is not a disable switch \
                     (use {EVENTS_PRUNE_INTERVAL_SECS_ENV}=0); using the default horizon"
                );
                DEFAULT_EVENTS_RETENTION
            }
            Ok(secs) if Duration::from_secs(secs) < MIN_EVENTS_RETENTION => {
                tracing::warn!(
                    secs,
                    floor_secs = MIN_EVENTS_RETENTION.as_secs(),
                    "events_prune: {EVENTS_RETENTION_SECS_ENV} below the 1-day floor; clamping"
                );
                MIN_EVENTS_RETENTION
            }
            Ok(secs) => Duration::from_secs(secs),
            Err(_) => {
                tracing::warn!(
                    raw,
                    "events_prune: unparseable {EVENTS_RETENTION_SECS_ENV}; using the default horizon"
                );
                DEFAULT_EVENTS_RETENTION
            }
        },
        Err(_) => DEFAULT_EVENTS_RETENTION,
    };
    let batch = match std::env::var(EVENTS_PRUNE_BATCH_ENV) {
        Ok(raw) => match raw.trim().parse::<i64>() {
            Ok(n) if n > 0 => n,
            _ => {
                tracing::warn!(
                    raw,
                    "events_prune: invalid {EVENTS_PRUNE_BATCH_ENV}; using the default batch size"
                );
                DEFAULT_EVENTS_PRUNE_BATCH
            }
        },
        Err(_) => DEFAULT_EVENTS_PRUNE_BATCH,
    };
    Some((
        interval,
        EventsRetentionPolicy {
            horizon,
            batch,
            ..EventsRetentionPolicy::default()
        },
    ))
}

/// One retention pass. Overlay history discovery/interpretation runs outside the writer;
/// each delete revalidates its append frontier and relied-on latest witnesses. Plain kinds
/// keep their bounded DELETE path. Every committed batch advances the same replay watermark.
pub async fn prune_events_once(pool: &SqlitePool, policy: &EventsRetentionPolicy) -> Result<u64> {
    let started = std::time::Instant::now();
    let horizon_ms =
        now_ms().saturating_sub(policy.horizon.as_millis().min(i64::MAX as u128) as i64);
    let batch = policy.batch.max(1);
    let mut pruned_total: u64 = 0;
    let mut pruned_by_kind: BTreeMap<&'static str, u64> = BTreeMap::new();
    let mut batches: u64 = 0;

    for rule in &policy.rules {
        for kind in &rule.kinds {
            let keep_latest = rule.keep_latest_per_overlay_key && *kind == "overlay.set";
            if keep_latest {
                let mut count = 0;
                if let Err(error) =
                    overlay::prune(pool, horizon_ms, batch, &mut count, &mut batches).await
                {
                    tracing::warn!(kind, error = %error, "events_prune: overlay pass failed; prior batches remain committed");
                }
                pruned_total += count;
                if count > 0 {
                    *pruned_by_kind.entry(kind).or_insert(0) += count;
                }
                continue;
            }
            loop {
                let deleted = match prune_batch(pool, kind, horizon_ms, batch).await {
                    Ok(deleted) => deleted,
                    Err(e) => {
                        tracing::warn!(
                            kind,
                            error = %e,
                            "events_prune: batch failed; continuing with next kind"
                        );
                        break;
                    }
                };
                batches += 1;
                if deleted > 0 {
                    pruned_total += deleted;
                    *pruned_by_kind.entry(kind).or_insert(0) += deleted;
                }
                if deleted < batch as u64 {
                    break;
                }
                tokio::time::sleep(BATCH_YIELD).await;
            }
        }
    }

    let events_earliest_id: Option<i64> = sqlx::query_scalar("SELECT MIN(id) FROM events")
        .fetch_one(pool)
        .await?;
    tracing::info!(
        pruned_total,
        pruned_by_kind = ?pruned_by_kind,
        batches,
        duration_ms = started.elapsed().as_millis() as u64,
        horizon_ms,
        events_earliest_id,
        "events_prune: pass complete"
    );
    Ok(pruned_total)
}

async fn prune_batch(pool: &SqlitePool, kind: &str, horizon_ms: i64, batch: i64) -> Result<u64> {
    let mut tx = begin_immediate_tx(pool).await?;
    let query = sqlx::query_scalar::<_, i64>(
        "DELETE FROM events WHERE id IN (SELECT id FROM events WHERE kind=?1 AND at<?2 LIMIT ?3) RETURNING id"
    ).bind(kind).bind(horizon_ms).bind(batch);
    let deleted_ids: Vec<i64> = query.fetch_all(&mut *tx).await?;
    advance_watermark(&mut tx, &deleted_ids).await?;
    tx.commit().await?;
    Ok(deleted_ids.len() as u64)
}

async fn advance_watermark(tx: &mut Transaction<'_, Sqlite>, deleted_ids: &[i64]) -> Result<()> {
    if let Some(max_id) = deleted_ids.iter().max() {
        sqlx::query(
            r#"INSERT INTO retention_meta (key, value) VALUES (?1, ?2)
               ON CONFLICT(key) DO UPDATE SET value = MAX(value, excluded.value)"#,
        )
        .bind(EVENTS_PRUNE_WATERMARK_KEY)
        .bind(max_id)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

mod overlay;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod writer_tests;
