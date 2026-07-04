//! Background retention pruner for the `events` table (#854 slice 2).
//!
//! The event log is append-only and grew unbounded in production (214k rows /
//! 1.7GB, 99.1% of rows in four transient kinds). This pruner deletes rows
//! that match ALL of:
//!
//!   * an exact-kind allowlist — `claude.hook`, `harness.phase.changed`,
//!     `harness.item.added`, `overlay.set`. Structural kinds (`card.*`,
//!     `wave.*`, `terminal.*`, …) and `overlay.deleted` are untouchable by
//!     construction; a new transient kind accumulates until explicitly
//!     opted in here (allowlist fails safe, blocklist would not);
//!   * an age horizon on `at` (default 30 days);
//!   * for `overlay.set` only, a keep-latest carve-out: the `MAX(id)` row
//!     per `(plugin_id, entity_kind, entity_id, kind)` quad is always kept,
//!     so the last-writer-wins overlay fold (`derive_layout_positions` /
//!     `fold_layout_positions` server-side, `useOverlayState` client-side)
//!     is invariant under pruning. `overlay.deleted` tombstones are never
//!     pruned out from under a kept older `overlay.set`.
//!
//! Accepted regression — what you lose after the horizon: `claude.hook`
//! rows older than the retention horizon disappear from the two production
//! consumers that replay them from genesis:
//!
//!   1. the wave-fs hook transcript, `hook_events_for_card`
//!      (crates/calm-truth/src/wave_fs_view.rs), which loses hook history
//!      older than the horizon for a card's transcript projection;
//!   2. harness recovery catch-up, `replay_harness_events_since`
//!      (crates/calm-server/src/harness/mod.rs), which replays `claude.hook`
//!      (among others) above a push watermark on boot recovery.
//!
//! Both are diagnostics-grade uses of >30-day-old data; the loss is accepted
//! and documented in `docs/events-retention.md`.
//!
//! Full-write assumption: keep-MAX(id) per quad assumes the latest
//! `overlay.set` for a quad is a FULL write. `fold_layout_positions` ignores
//! a positions-less `overlay.set` (`.or(current)`), so if the latest kept row
//! lacked `positions` while a pruned older row carried them, the fold would
//! change after pruning. Today's kernel writer always sends a full positions
//! map (`spec_harness_layout_payload`, crates/calm-server/src/routes/waves.rs
//! — pinned by a unit test there), and the frontend layout writer PUTs the
//! complete map on every drag. Any future partial-write overlay producer must
//! revisit this carve-out.
//!
//! The pruner never VACUUMs: freed pages are reused by new appends and the
//! file size plateaus. Actual shrink is a manual runbook step (backup, then
//! `neige vacuum --force`); see `docs/events-retention.md`.
//!
//! Cold WS replay stays correct after pruning via the slice-1 protocol:
//! clients whose cursor predates `events_earliest_id` receive
//! `snapshot_required` instead of a gappy replay.

use crate::db::sqlite::begin_immediate_tx;
use crate::error::Result;
use crate::model::now_ms;
use sqlx::SqlitePool;
use std::collections::BTreeMap;
use std::time::Duration;

const EVENTS_PRUNE_INTERVAL_SECS_ENV: &str = "NEIGE_EVENTS_PRUNE_INTERVAL_SECS";
const EVENTS_RETENTION_SECS_ENV: &str = "NEIGE_EVENTS_RETENTION_SECS";
const EVENTS_PRUNE_BATCH_ENV: &str = "NEIGE_EVENTS_PRUNE_BATCH";
const EVENTS_PRUNE_INTERVAL: Duration = Duration::from_secs(60 * 60);
const DEFAULT_EVENTS_RETENTION: Duration = Duration::from_secs(30 * 24 * 60 * 60);
const DEFAULT_EVENTS_PRUNE_BATCH: i64 = 5000;
/// Pause between per-batch write transactions so the pruner never
/// monopolizes SQLite's single writer slot on a bloated first pass.
const BATCH_YIELD: Duration = Duration::from_millis(100);

/// Exact-kind allowlist. Only these kinds are ever eligible for pruning;
/// everything else in the events table is permanent by construction.
pub const EVENTS_PRUNE_KINDS: &[&str] = &[
    "claude.hook",
    "harness.phase.changed",
    "harness.item.added",
    "overlay.set",
];

/// One retention rule: a set of prunable kinds plus whether the
/// keep-latest-per-overlay-quad carve-out applies. The `Vec<RetentionRule>`
/// on [`EventsRetentionPolicy`] is the seam for #33 per-actor retention;
/// today there is exactly one hardcoded rule.
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
        // Match the wave-history pruner: skip the immediate boot tick and
        // let the server settle before taking SQLite writer locks.
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
            Err(_) => EVENTS_PRUNE_INTERVAL,
        },
        Err(_) => EVENTS_PRUNE_INTERVAL,
    };
    let horizon = match std::env::var(EVENTS_RETENTION_SECS_ENV) {
        Ok(raw) => match raw.trim().parse::<u64>() {
            Ok(secs) if secs > 0 => Duration::from_secs(secs),
            _ => DEFAULT_EVENTS_RETENTION,
        },
        Err(_) => DEFAULT_EVENTS_RETENTION,
    };
    let batch = match std::env::var(EVENTS_PRUNE_BATCH_ENV) {
        Ok(raw) => match raw.trim().parse::<i64>() {
            Ok(n) if n > 0 => n,
            _ => DEFAULT_EVENTS_PRUNE_BATCH,
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

/// One full prune pass. Public so integration tests can drive pruning
/// deterministically without waiting for the scheduled task. Runs one
/// batched DELETE (LIMIT `policy.batch`) per `begin_immediate_tx`, yielding
/// between batches, so writer-lock hold stays bounded to milliseconds.
///
/// The keep-latest `MAX(id)` subquery runs in the same immediate
/// transaction as its DELETE: BEGIN IMMEDIATE holds the single writer lock
/// for the whole statement, so "compute latest" and "delete the rest" are
/// atomic. It is also guarded to `overlay.set` batches only — batches for
/// the other allowlisted kinds never pay for it.
pub async fn prune_events_once(pool: &SqlitePool, policy: &EventsRetentionPolicy) -> Result<u64> {
    let started = std::time::Instant::now();
    let horizon_ms =
        now_ms().saturating_sub(policy.horizon.as_millis().min(i64::MAX as u128) as i64);
    let mut pruned_total: u64 = 0;
    let mut pruned_by_kind: BTreeMap<&'static str, u64> = BTreeMap::new();
    let mut batches: u64 = 0;

    for rule in &policy.rules {
        for kind in &rule.kinds {
            let keep_latest = rule.keep_latest_per_overlay_key && *kind == "overlay.set";
            loop {
                let deleted =
                    match prune_batch(pool, kind, keep_latest, horizon_ms, policy.batch).await {
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
                if deleted < policy.batch as u64 {
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

async fn prune_batch(
    pool: &SqlitePool,
    kind: &str,
    keep_latest_per_overlay_key: bool,
    horizon_ms: i64,
    batch: i64,
) -> Result<u64> {
    let sql = if keep_latest_per_overlay_key {
        r#"DELETE FROM events WHERE id IN (
               SELECT id FROM events
               WHERE kind = ?1 AND at < ?2
                 AND id NOT IN (
                   SELECT MAX(id) FROM events WHERE kind = ?1
                   GROUP BY json_extract(payload, '$.plugin_id'),
                            json_extract(payload, '$.entity_kind'),
                            json_extract(payload, '$.entity_id'),
                            json_extract(payload, '$.kind'))
               LIMIT ?3)"#
    } else {
        r#"DELETE FROM events WHERE id IN (
               SELECT id FROM events
               WHERE kind = ?1 AND at < ?2
               LIMIT ?3)"#
    };
    let mut tx = begin_immediate_tx(pool).await?;
    let result = sqlx::query(sql)
        .bind(kind)
        .bind(horizon_ms)
        .bind(batch)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sqlite::SqlxRepo;

    const DAY_MS: i64 = 24 * 60 * 60 * 1000;

    async fn repo() -> SqlxRepo {
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory repo")
    }

    async fn insert_event(pool: &SqlitePool, kind: &str, payload: &str, at: i64) -> i64 {
        sqlx::query_scalar(
            r#"INSERT INTO events (kind, payload, actor, at, correlation)
               VALUES (?1, ?2, 'user', ?3, NULL)
               RETURNING id"#,
        )
        .bind(kind)
        .bind(payload)
        .bind(at)
        .fetch_one(pool)
        .await
        .expect("insert event")
    }

    fn overlay_payload(plugin_id: &str, entity_kind: &str, entity_id: &str, kind: &str) -> String {
        serde_json::json!({
            "id": format!("{plugin_id}:{entity_kind}:{entity_id}:{kind}"),
            "plugin_id": plugin_id,
            "entity_kind": entity_kind,
            "entity_id": entity_id,
            "kind": kind,
            "payload": {"positions": {"c1": {"x": 0, "y": 0, "w": 6, "h": 12}}},
            "updated_at": 0
        })
        .to_string()
    }

    async fn remaining_ids(pool: &SqlitePool) -> Vec<i64> {
        sqlx::query_scalar("SELECT id FROM events ORDER BY id")
            .fetch_all(pool)
            .await
            .expect("select ids")
    }

    fn old(days: i64) -> i64 {
        now_ms() - days * DAY_MS
    }

    #[tokio::test]
    async fn keeps_exactly_max_id_per_overlay_quad() {
        let repo = repo().await;
        let pool = repo.pool();
        let quad_a = overlay_payload("kernel", "view", "w1", "layout");
        let quad_b = overlay_payload("p1", "card", "c1", "status");
        let _a1 = insert_event(pool, "overlay.set", &quad_a, old(90)).await;
        let _a2 = insert_event(pool, "overlay.set", &quad_a, old(80)).await;
        let _b1 = insert_event(pool, "overlay.set", &quad_b, old(70)).await;
        let a3 = insert_event(pool, "overlay.set", &quad_a, old(60)).await;
        let b2 = insert_event(pool, "overlay.set", &quad_b, old(50)).await;

        let pruned = prune_events_once(pool, &EventsRetentionPolicy::default())
            .await
            .expect("prune");

        assert_eq!(pruned, 3);
        assert_eq!(remaining_ids(pool).await, vec![a3, b2]);
    }

    #[tokio::test]
    async fn keeps_rows_newer_than_horizon_even_when_superseded() {
        let repo = repo().await;
        let pool = repo.pool();
        let quad = overlay_payload("kernel", "view", "w1", "layout");
        let _old_dup = insert_event(pool, "overlay.set", &quad, old(60)).await;
        let new_dup = insert_event(pool, "overlay.set", &quad, old(1)).await;
        let new_latest = insert_event(pool, "overlay.set", &quad, now_ms()).await;
        let new_hook = insert_event(pool, "claude.hook", "{}", old(2)).await;

        let pruned = prune_events_once(pool, &EventsRetentionPolicy::default())
            .await
            .expect("prune");

        assert_eq!(pruned, 1);
        assert_eq!(
            remaining_ids(pool).await,
            vec![new_dup, new_latest, new_hook]
        );
    }

    #[tokio::test]
    async fn deletes_old_transient_kinds_past_horizon() {
        let repo = repo().await;
        let pool = repo.pool();
        insert_event(pool, "claude.hook", "{}", old(31)).await;
        insert_event(pool, "harness.phase.changed", "{}", old(45)).await;
        insert_event(pool, "harness.item.added", "{}", old(45)).await;
        let recent = insert_event(pool, "claude.hook", "{}", now_ms()).await;

        let pruned = prune_events_once(pool, &EventsRetentionPolicy::default())
            .await
            .expect("prune");

        assert_eq!(pruned, 3);
        assert_eq!(remaining_ids(pool).await, vec![recent]);
    }

    #[tokio::test]
    async fn never_touches_non_allowlist_kinds_or_overlay_deleted() {
        let repo = repo().await;
        let pool = repo.pool();
        let structural = insert_event(pool, "cove.updated", "{}", 0).await;
        let card = insert_event(pool, "card.added", "{}", old(400)).await;
        let tombstone = insert_event(
            pool,
            "overlay.deleted",
            r#"{"plugin_id":"kernel","entity_kind":"view","entity_id":"w1","kind":"layout"}"#,
            old(400),
        )
        .await;

        let pruned = prune_events_once(pool, &EventsRetentionPolicy::default())
            .await
            .expect("prune");

        assert_eq!(pruned, 0);
        assert_eq!(remaining_ids(pool).await, vec![structural, card, tombstone]);
    }

    #[tokio::test]
    async fn prunes_across_multiple_batches() {
        let repo = repo().await;
        let pool = repo.pool();
        for _ in 0..7 {
            insert_event(pool, "claude.hook", "{}", old(31)).await;
        }
        let policy = EventsRetentionPolicy {
            batch: 3,
            ..EventsRetentionPolicy::default()
        };

        let pruned = prune_events_once(pool, &policy).await.expect("prune");

        assert_eq!(pruned, 7);
        assert_eq!(remaining_ids(pool).await, Vec::<i64>::new());
    }

    #[test]
    fn events_pruner_config_from_env_respects_disable_and_defaults() {
        let saved_interval = std::env::var(EVENTS_PRUNE_INTERVAL_SECS_ENV).ok();
        let saved_retention = std::env::var(EVENTS_RETENTION_SECS_ENV).ok();
        let saved_batch = std::env::var(EVENTS_PRUNE_BATCH_ENV).ok();
        fn set(key: &str, value: &str) {
            // SAFETY: this test owns the events-pruner env vars it mutates.
            unsafe { std::env::set_var(key, value) };
        }
        fn remove(key: &str) {
            // SAFETY: see `set`.
            unsafe { std::env::remove_var(key) };
        }

        remove(EVENTS_PRUNE_INTERVAL_SECS_ENV);
        remove(EVENTS_RETENTION_SECS_ENV);
        remove(EVENTS_PRUNE_BATCH_ENV);
        assert_eq!(
            events_pruner_config_from_env(),
            Some((EVENTS_PRUNE_INTERVAL, EventsRetentionPolicy::default()))
        );

        set(EVENTS_PRUNE_INTERVAL_SECS_ENV, "0");
        assert_eq!(events_pruner_config_from_env(), None);

        set(EVENTS_PRUNE_INTERVAL_SECS_ENV, "17");
        set(EVENTS_RETENTION_SECS_ENV, "86400");
        set(EVENTS_PRUNE_BATCH_ENV, "23");
        assert_eq!(
            events_pruner_config_from_env(),
            Some((
                Duration::from_secs(17),
                EventsRetentionPolicy {
                    horizon: Duration::from_secs(86400),
                    batch: 23,
                    ..EventsRetentionPolicy::default()
                }
            ))
        );

        set(EVENTS_RETENTION_SECS_ENV, "0");
        set(EVENTS_PRUNE_BATCH_ENV, "0");
        assert_eq!(
            events_pruner_config_from_env(),
            Some((Duration::from_secs(17), EventsRetentionPolicy::default()))
        );

        match saved_interval {
            Some(value) => set(EVENTS_PRUNE_INTERVAL_SECS_ENV, &value),
            None => remove(EVENTS_PRUNE_INTERVAL_SECS_ENV),
        }
        match saved_retention {
            Some(value) => set(EVENTS_RETENTION_SECS_ENV, &value),
            None => remove(EVENTS_RETENTION_SECS_ENV),
        }
        match saved_batch {
            Some(value) => set(EVENTS_PRUNE_BATCH_ENV, &value),
            None => remove(EVENTS_PRUNE_BATCH_ENV),
        }
    }

    #[test]
    fn allowlist_never_contains_structural_or_tombstone_kinds() {
        assert!(!EVENTS_PRUNE_KINDS.contains(&"overlay.deleted"));
        assert!(EVENTS_PRUNE_KINDS.iter().all(|k| !k.starts_with("card.")
            && !k.starts_with("wave.")
            && !k.starts_with("cove.")
            && !k.starts_with("terminal.")));
    }
}
