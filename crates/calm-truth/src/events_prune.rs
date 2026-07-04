//! Background retention pruner for the `events` table (#854 slice 2).
//!
//! The event log is append-only and grew unbounded in production (214k rows /
//! 1.7GB, 99.1% of rows in a handful of transient kinds). This pruner deletes
//! rows that match ALL of:
//!
//!   * an exact-kind allowlist — `claude.hook`, `codex.hook`,
//!     `harness.phase.changed`, `harness.item.added`, `overlay.set`.
//!     Structural kinds (`card.*`, `wave.*`, `terminal.*`, …) and
//!     `overlay.deleted` are untouchable by construction; a new transient
//!     kind accumulates until explicitly opted in here (allowlist fails
//!     safe, blocklist would not);
//!   * an age horizon on `at` (default 30 days, floored at 1 day);
//!   * for `overlay.set` only, a keep-latest carve-out: the `MAX(id)` row
//!     per `(plugin_id, entity_kind, entity_id, kind)` quad is always kept,
//!     so the last-writer-wins overlay fold (`derive_layout_positions` /
//!     `fold_layout_positions` server-side, `useOverlayState` client-side)
//!     is invariant under pruning. `overlay.deleted` tombstones are never
//!     pruned out from under a kept older `overlay.set`. Additionally, a
//!     quad whose LATEST row would be dropped by the read-side version
//!     guard (`validation::should_skip_overlay` — future `schemaVersion`
//!     written by a newer binary, or an unparseable payload) is skipped
//!     entirely for the pass: replay would hide that latest row, so the
//!     older supported row is the state a client actually folds and must
//!     survive.
//!
//! Accepted regression — what you lose after the horizon: `claude.hook` and
//! `codex.hook` rows older than the retention horizon disappear from the two
//! production consumers that replay them from genesis:
//!
//!   1. the wave-fs hook transcript, `hook_events_for_card`
//!      (crates/calm-truth/src/wave_fs_view.rs), which reads both hook kinds
//!      and loses history older than the horizon for a card's transcript
//!      projection;
//!   2. harness recovery catch-up, `replay_harness_events_since`
//!      (crates/calm-server/src/harness/mod.rs), which replays both hook
//!      kinds (among others) above a push watermark on boot recovery.
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
//! Replay safety: every pruning DELETE advances a durable retention
//! watermark (`retention_meta.events_prune_watermark` = highest id ever
//! pruned, updated in the same transaction as the DELETE). The WS replay
//! guard sends `_snapshot_required` to any client whose `since` cursor is
//! below the watermark, because pruned rows create interior holes that the
//! `MIN(id)` check alone can never detect — structural events are permanent,
//! so `events_earliest_id` never advances past the first structural row.

use crate::db::sqlite::begin_immediate_tx;
use crate::error::Result;
use crate::model::{Overlay, now_ms};
use sqlx::SqlitePool;
use std::collections::BTreeMap;
use std::time::Duration;

const EVENTS_PRUNE_INTERVAL_SECS_ENV: &str = "NEIGE_EVENTS_PRUNE_INTERVAL_SECS";
const EVENTS_RETENTION_SECS_ENV: &str = "NEIGE_EVENTS_RETENTION_SECS";
const EVENTS_PRUNE_BATCH_ENV: &str = "NEIGE_EVENTS_PRUNE_BATCH";
const EVENTS_PRUNE_INTERVAL: Duration = Duration::from_secs(60 * 60);
const DEFAULT_EVENTS_RETENTION: Duration = Duration::from_secs(30 * 24 * 60 * 60);
/// Floor on the retention horizon: a mistyped `NEIGE_EVENTS_RETENTION_SECS`
/// (seconds-vs-days confusion, e.g. `1`) must not wipe all allowlisted
/// history. Values below one day clamp here with a warning.
const MIN_EVENTS_RETENTION: Duration = Duration::from_secs(24 * 60 * 60);
const DEFAULT_EVENTS_PRUNE_BATCH: i64 = 5000;
/// Pause between per-batch write transactions so the pruner never
/// monopolizes SQLite's single writer slot on a bloated first pass.
const BATCH_YIELD: Duration = Duration::from_millis(100);

/// `retention_meta` key holding the highest `events.id` ever pruned. Read
/// back by `RepoEventWrite::events_prune_watermark` for the WS replay guard.
pub const EVENTS_PRUNE_WATERMARK_KEY: &str = "events_prune_watermark";

/// Exact-kind allowlist. Only these kinds are ever eligible for pruning;
/// everything else in the events table is permanent by construction.
pub const EVENTS_PRUNE_KINDS: &[&str] = &[
    "claude.hook",
    "codex.hook",
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
            Err(_) => {
                // Unparseable is NOT a disable switch: the failure direction
                // of this knob is data deletion, so say loudly that the
                // pruner stays ON with defaults.
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

/// One full prune pass. Public so integration tests can drive pruning
/// deterministically without waiting for the scheduled task. Runs one
/// batched DELETE (LIMIT `policy.batch`) per `begin_immediate_tx`, yielding
/// between batches, so writer-lock hold stays bounded to milliseconds. Each
/// deleting transaction also advances the durable retention watermark (see
/// module docs) before it commits.
///
/// The keep-latest `MAX(id)` subquery runs in the same immediate
/// transaction as its DELETE: BEGIN IMMEDIATE holds the single writer lock
/// for the whole statement, so "compute latest" and "delete the rest" are
/// atomic. It is also guarded to `overlay.set` batches only — batches for
/// the other allowlisted kinds never pay for it. Quads whose latest row is
/// version-unsupported are computed once per pass and excluded from every
/// `overlay.set` batch (error direction: keep extra rows until the next
/// pass, never delete a row replay still needs).
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
            let frozen_quads = if keep_latest {
                match overlay_quads_with_unsupported_latest(pool).await {
                    Ok(quads) => quads,
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "events_prune: unsupported-latest quad scan failed; \
                             skipping overlay.set this pass"
                        );
                        continue;
                    }
                }
            } else {
                Vec::new()
            };
            loop {
                let deleted =
                    match prune_batch(pool, kind, keep_latest, &frozen_quads, horizon_ms, batch)
                        .await
                    {
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

/// Overlay quad key as SQLite's `json_extract` sees it — `None` when the
/// payload lacks the field (kept as `IS`-comparable NULLs so the exclusion
/// predicate matches exactly the rows the keep-set `GROUP BY` bucketed
/// together).
type OverlayQuad = (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

/// Quads whose LATEST `overlay.set` row would be dropped by the read-side
/// version guard on replay (future `schemaVersion`, or a payload that no
/// longer parses as an `Overlay`). Pruning older rows of such a quad would
/// leave replay with neither the old supported state nor the new one, so
/// the whole quad is frozen for the pass.
async fn overlay_quads_with_unsupported_latest(pool: &SqlitePool) -> Result<Vec<OverlayQuad>> {
    type Row = (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        String,
    );
    let rows: Vec<Row> = sqlx::query_as(
        r#"SELECT json_extract(payload, '$.plugin_id'),
                  json_extract(payload, '$.entity_kind'),
                  json_extract(payload, '$.entity_id'),
                  json_extract(payload, '$.kind'),
                  payload
           FROM events
           WHERE kind = 'overlay.set' AND id IN (
             SELECT MAX(id) FROM events WHERE kind = 'overlay.set'
             GROUP BY json_extract(payload, '$.plugin_id'),
                      json_extract(payload, '$.entity_kind'),
                      json_extract(payload, '$.entity_id'),
                      json_extract(payload, '$.kind'))"#,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|(plugin_id, entity_kind, entity_id, kind, payload)| {
            let unsupported = match serde_json::from_str::<Overlay>(&payload) {
                Ok(overlay) => crate::validation::should_skip_overlay(&overlay),
                // Unparseable latest — replay skips the row entirely, so
                // treat it like an unsupported version and freeze the quad.
                Err(_) => true,
            };
            unsupported.then_some((plugin_id, entity_kind, entity_id, kind))
        })
        .collect())
}

async fn prune_batch(
    pool: &SqlitePool,
    kind: &str,
    keep_latest_per_overlay_key: bool,
    frozen_quads: &[OverlayQuad],
    horizon_ms: i64,
    batch: i64,
) -> Result<u64> {
    let mut sql = String::from(
        r#"DELETE FROM events WHERE id IN (
               SELECT id FROM events
               WHERE kind = ?1 AND at < ?2"#,
    );
    if keep_latest_per_overlay_key {
        sql.push_str(
            r#"
                 AND id NOT IN (
                   SELECT MAX(id) FROM events WHERE kind = ?1
                   GROUP BY json_extract(payload, '$.plugin_id'),
                            json_extract(payload, '$.entity_kind'),
                            json_extract(payload, '$.entity_id'),
                            json_extract(payload, '$.kind'))"#,
        );
        for i in 0..frozen_quads.len() {
            // `IS` (not `=`) so a NULL quad component matches the same
            // rows the keep-set `GROUP BY` grouped together.
            let base = 4 + i * 4;
            sql.push_str(&format!(
                "\n                 AND NOT (json_extract(payload, '$.plugin_id') IS ?{} \
                 AND json_extract(payload, '$.entity_kind') IS ?{} \
                 AND json_extract(payload, '$.entity_id') IS ?{} \
                 AND json_extract(payload, '$.kind') IS ?{})",
                base,
                base + 1,
                base + 2,
                base + 3
            ));
        }
    }
    sql.push_str("\n               LIMIT ?3)\n           RETURNING id");

    let mut query = sqlx::query_scalar::<_, i64>(&sql)
        .bind(kind)
        .bind(horizon_ms)
        .bind(batch);
    if keep_latest_per_overlay_key {
        for (plugin_id, entity_kind, entity_id, overlay_kind) in frozen_quads {
            query = query
                .bind(plugin_id)
                .bind(entity_kind)
                .bind(entity_id)
                .bind(overlay_kind);
        }
    }

    let mut tx = begin_immediate_tx(pool).await?;
    let deleted_ids: Vec<i64> = query.fetch_all(&mut *tx).await?;
    if let Some(max_id) = deleted_ids.iter().max() {
        sqlx::query(
            r#"INSERT INTO retention_meta (key, value) VALUES (?1, ?2)
               ON CONFLICT(key) DO UPDATE SET value = MAX(value, excluded.value)"#,
        )
        .bind(EVENTS_PRUNE_WATERMARK_KEY)
        .bind(max_id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(deleted_ids.len() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::RepoEventWrite;
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
        overlay_payload_with_inner(
            plugin_id,
            entity_kind,
            entity_id,
            kind,
            serde_json::json!({"positions": {"c1": {"x": 0, "y": 0, "w": 6, "h": 12}}}),
        )
    }

    fn overlay_payload_with_inner(
        plugin_id: &str,
        entity_kind: &str,
        entity_id: &str,
        kind: &str,
        inner: serde_json::Value,
    ) -> String {
        serde_json::json!({
            "id": format!("{plugin_id}:{entity_kind}:{entity_id}:{kind}"),
            "plugin_id": plugin_id,
            "entity_kind": entity_kind,
            "entity_id": entity_id,
            "kind": kind,
            "payload": inner,
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
        insert_event(pool, "codex.hook", "{}", old(31)).await;
        insert_event(pool, "harness.phase.changed", "{}", old(45)).await;
        insert_event(pool, "harness.item.added", "{}", old(45)).await;
        let recent_claude = insert_event(pool, "claude.hook", "{}", now_ms()).await;
        let recent_codex = insert_event(pool, "codex.hook", "{}", now_ms()).await;

        let pruned = prune_events_once(pool, &EventsRetentionPolicy::default())
            .await
            .expect("prune");

        assert_eq!(pruned, 4);
        assert_eq!(remaining_ids(pool).await, vec![recent_claude, recent_codex]);
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

    #[tokio::test]
    async fn zero_batch_still_makes_progress() {
        let repo = repo().await;
        let pool = repo.pool();
        insert_event(pool, "claude.hook", "{}", old(31)).await;
        insert_event(pool, "claude.hook", "{}", old(31)).await;
        let policy = EventsRetentionPolicy {
            batch: 0,
            ..EventsRetentionPolicy::default()
        };

        let pruned = prune_events_once(pool, &policy).await.expect("prune");

        assert_eq!(pruned, 2);
    }

    #[tokio::test]
    async fn advances_durable_watermark_to_max_pruned_id() {
        let repo = repo().await;
        let pool = repo.pool();
        assert_eq!(repo.events_prune_watermark().await.expect("watermark"), 0);

        insert_event(pool, "claude.hook", "{}", old(31)).await;
        let hook2 = insert_event(pool, "claude.hook", "{}", old(31)).await;
        let structural = insert_event(pool, "cove.updated", "{}", old(31)).await;

        prune_events_once(pool, &EventsRetentionPolicy::default())
            .await
            .expect("prune");
        assert_eq!(
            repo.events_prune_watermark().await.expect("watermark"),
            hook2,
            "watermark is the highest id ever pruned"
        );

        // A pass that prunes nothing must not move the watermark.
        prune_events_once(pool, &EventsRetentionPolicy::default())
            .await
            .expect("second prune");
        assert_eq!(
            repo.events_prune_watermark().await.expect("watermark"),
            hook2
        );
        assert_eq!(remaining_ids(pool).await, vec![structural]);
    }

    #[tokio::test]
    async fn freezes_quad_whose_latest_row_is_version_unsupported() {
        let repo = repo().await;
        let pool = repo.pool();
        // Kernel-owned `layout` kind: supported v1 write, then a future
        // schemaVersion write as the quad's latest — replay drops the
        // latest on read, so the older supported row must survive too.
        let supported = insert_event(
            pool,
            "overlay.set",
            &overlay_payload_with_inner(
                "kernel",
                "view",
                "w1",
                "layout",
                serde_json::json!({"schemaVersion": 1, "positions": {}}),
            ),
            old(90),
        )
        .await;
        let future = insert_event(
            pool,
            "overlay.set",
            &overlay_payload_with_inner(
                "kernel",
                "view",
                "w1",
                "layout",
                serde_json::json!({"schemaVersion": 99, "positions": {}}),
            ),
            old(60),
        )
        .await;
        // Control quad: normal carve-out still applies in the same pass.
        let quad_b = overlay_payload("p1", "card", "c1", "status");
        let _b1 = insert_event(pool, "overlay.set", &quad_b, old(80)).await;
        let b2 = insert_event(pool, "overlay.set", &quad_b, old(50)).await;

        let pruned = prune_events_once(pool, &EventsRetentionPolicy::default())
            .await
            .expect("prune");

        assert_eq!(pruned, 1, "only the control quad's superseded row goes");
        assert_eq!(remaining_ids(pool).await, vec![supported, future, b2]);
    }

    #[test]
    fn events_pruner_config_from_env_respects_disable_floor_and_defaults() {
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

        // Unparseable interval is NOT a disable: pruner stays ON, default
        // interval (deleting data on a typo'd knob must fail conservative).
        set(EVENTS_PRUNE_INTERVAL_SECS_ENV, "off");
        assert_eq!(
            events_pruner_config_from_env(),
            Some((EVENTS_PRUNE_INTERVAL, EventsRetentionPolicy::default()))
        );

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

        // Sub-floor retention clamps to the 1-day floor (secs-vs-days
        // confusion must not wipe all allowlisted history).
        set(EVENTS_RETENTION_SECS_ENV, "1");
        assert_eq!(
            events_pruner_config_from_env(),
            Some((
                Duration::from_secs(17),
                EventsRetentionPolicy {
                    horizon: MIN_EVENTS_RETENTION,
                    batch: 23,
                    ..EventsRetentionPolicy::default()
                }
            ))
        );

        // Retention 0 / unparseable fall back to the DEFAULT horizon (not
        // the floor); batch 0 / unparseable fall back to the default batch.
        set(EVENTS_RETENTION_SECS_ENV, "0");
        set(EVENTS_PRUNE_BATCH_ENV, "0");
        assert_eq!(
            events_pruner_config_from_env(),
            Some((Duration::from_secs(17), EventsRetentionPolicy::default()))
        );
        set(EVENTS_RETENTION_SECS_ENV, "never");
        set(EVENTS_PRUNE_BATCH_ENV, "lots");
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
