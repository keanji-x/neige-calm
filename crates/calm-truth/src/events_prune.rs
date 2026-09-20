//! Background retention pruner for the `events` table: deletes rows matching
//! an exact-kind allowlist AND an age horizon, keeping the `MAX(id)`
//! `overlay.set` row per quad so the last-writer-wins fold is invariant under
//! pruning. Every DELETE advances a durable watermark the WS replay guard
//! uses to force `_snapshot_required`. Never VACUUMs.

use crate::db::sqlite::begin_immediate_tx;
use crate::error::Result;
use crate::model::{Overlay, now_ms};
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

/// One full prune pass: one batched DELETE per `begin_immediate_tx`, yielding
/// between batches. The keep-latest subquery and the frozen-quad scan run in
/// the same immediate transaction as their DELETE, so a future-schema write
/// can never slip between the freeze decision and the delete.
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
            loop {
                let deleted = match prune_batch(pool, kind, keep_latest, horizon_ms, batch).await {
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

/// Overlay quad key as `json_extract` sees it — `None` for a missing field, so
/// the exclusion predicate matches exactly what the keep-set `GROUP BY` bucketed.
type OverlayQuad = (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

/// Quads whose LATEST `overlay.set` row replay would drop (future
/// `schemaVersion`, or not an `Overlay`): pruning older rows would leave replay
/// with neither state, so the whole quad is frozen for the batch.
async fn overlay_quads_with_unsupported_latest_tx(
    tx: &mut Transaction<'_, Sqlite>,
) -> Result<Vec<OverlayQuad>> {
    type Row = (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        String,
    );
    // `json_valid` gates every `json_extract`: SQLite RAISES on malformed JSON,
    // and one bad historical row would otherwise error every overlay batch.
    // Such rows are never delete candidates — a row we cannot parse is a row we
    // do not prune.
    let rows: Vec<Row> = sqlx::query_as(
        r#"SELECT json_extract(payload, '$.plugin_id'),
                  json_extract(payload, '$.entity_kind'),
                  json_extract(payload, '$.entity_id'),
                  json_extract(payload, '$.kind'),
                  payload
           FROM events
           WHERE kind = 'overlay.set' AND json_valid(payload) AND id IN (
             SELECT MAX(id) FROM events
             WHERE kind = 'overlay.set' AND json_valid(payload)
             GROUP BY json_extract(payload, '$.plugin_id'),
                      json_extract(payload, '$.entity_kind'),
                      json_extract(payload, '$.entity_id'),
                      json_extract(payload, '$.kind'))"#,
    )
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|(plugin_id, entity_kind, entity_id, kind, payload)| {
            let unsupported = match serde_json::from_str::<Overlay>(&payload) {
                Ok(overlay) => crate::validation::should_skip_overlay(&overlay),
                // Valid JSON that is not an `Overlay`: replay skips it, so freeze the quad.
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
    horizon_ms: i64,
    batch: i64,
) -> Result<u64> {
    let mut tx = begin_immediate_tx(pool).await?;
    let frozen_quads = if keep_latest_per_overlay_key {
        overlay_quads_with_unsupported_latest_tx(&mut tx).await?
    } else {
        Vec::new()
    };

    let mut sql = String::from(
        r#"DELETE FROM events WHERE id IN (
               SELECT id FROM events
               WHERE kind = ?1 AND at < ?2"#,
    );
    if keep_latest_per_overlay_key {
        // `json_valid` first, and the keep-set groups only valid rows so a malformed
        // row can never claim a quad's MAX(id) slot from a valid one.
        sql.push_str(
            r#"
                 AND json_valid(payload)
                 AND id NOT IN (
                   SELECT MAX(id) FROM events
                   WHERE kind = ?1 AND json_valid(payload)
                   GROUP BY json_extract(payload, '$.plugin_id'),
                            json_extract(payload, '$.entity_kind'),
                            json_extract(payload, '$.entity_id'),
                            json_extract(payload, '$.kind'))"#,
        );
        for i in 0..frozen_quads.len() {
            // `IS` (not `=`) so a NULL quad component matches what `GROUP BY` grouped.
            // CASE-gated on `json_valid`: SQLite does not guarantee AND-term evaluation
            // order, but CASE evaluation IS guaranteed lazy.
            let base = 4 + i * 4;
            sql.push_str(&format!(
                "\n                 AND NOT (CASE WHEN json_valid(payload) \
                 THEN json_extract(payload, '$.plugin_id') END IS ?{} \
                 AND CASE WHEN json_valid(payload) \
                 THEN json_extract(payload, '$.entity_kind') END IS ?{} \
                 AND CASE WHEN json_valid(payload) \
                 THEN json_extract(payload, '$.entity_id') END IS ?{} \
                 AND CASE WHEN json_valid(payload) \
                 THEN json_extract(payload, '$.kind') END IS ?{})",
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
        for (plugin_id, entity_kind, entity_id, overlay_kind) in &frozen_quads {
            query = query
                .bind(plugin_id)
                .bind(entity_kind)
                .bind(entity_id)
                .bind(overlay_kind);
        }
    }

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
        let structural = insert_event(pool, "area.updated", "{}", 0).await;
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
        let structural = insert_event(pool, "area.updated", "{}", old(31)).await;

        prune_events_once(pool, &EventsRetentionPolicy::default())
            .await
            .expect("prune");
        assert_eq!(
            repo.events_prune_watermark().await.expect("watermark"),
            hook2,
            "watermark is the highest id ever pruned"
        );

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
        let quad_b = overlay_payload("p1", "card", "c1", "status");
        let _b1 = insert_event(pool, "overlay.set", &quad_b, old(80)).await;
        let b2 = insert_event(pool, "overlay.set", &quad_b, old(50)).await;

        let pruned = prune_events_once(pool, &EventsRetentionPolicy::default())
            .await
            .expect("prune");

        assert_eq!(pruned, 1, "only the control quad's superseded row goes");
        assert_eq!(remaining_ids(pool).await, vec![supported, future, b2]);
    }

    #[tokio::test]
    async fn freeze_set_is_recomputed_at_delete_time_not_cached() {
        let repo = repo().await;
        let pool = repo.pool();
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
        assert_eq!(
            prune_events_once(pool, &EventsRetentionPolicy::default())
                .await
                .expect("prune"),
            0
        );

        // The freeze is decided inside the delete tx, never carried over from an
        // earlier scan, so the NEXT batch must keep BOTH rows.
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
        assert_eq!(
            prune_events_once(pool, &EventsRetentionPolicy::default())
                .await
                .expect("prune"),
            0
        );
        assert_eq!(remaining_ids(pool).await, vec![supported, future]);

        let healed = insert_event(
            pool,
            "overlay.set",
            &overlay_payload_with_inner(
                "kernel",
                "view",
                "w1",
                "layout",
                serde_json::json!({"schemaVersion": 1, "positions": {"c": {"x": 0, "y": 0, "w": 1, "h": 1}}}),
            ),
            old(40),
        )
        .await;
        assert_eq!(
            prune_events_once(pool, &EventsRetentionPolicy::default())
                .await
                .expect("prune"),
            2
        );
        assert_eq!(remaining_ids(pool).await, vec![healed]);
    }

    #[tokio::test]
    async fn malformed_overlay_payload_never_poisons_the_batch() {
        let repo = repo().await;
        let pool = repo.pool();
        // Malformed JSON: ungated, `json_extract` would RAISE and disable overlay pruning.
        let malformed = insert_event(pool, "overlay.set", "{not json", old(90)).await;
        let quad = overlay_payload("p1", "card", "c1", "status");
        let _superseded = insert_event(pool, "overlay.set", &quad, old(80)).await;
        let latest = insert_event(pool, "overlay.set", &quad, old(50)).await;
        let _hook = insert_event(pool, "claude.hook", "{}", old(40)).await;

        let pruned = prune_events_once(pool, &EventsRetentionPolicy::default())
            .await
            .expect("prune must not error on a malformed overlay payload");

        assert_eq!(pruned, 2);
        assert_eq!(remaining_ids(pool).await, vec![malformed, latest]);

        let frozen_latest = insert_event(
            pool,
            "overlay.set",
            &overlay_payload_with_inner(
                "kernel",
                "view",
                "w1",
                "layout",
                serde_json::json!({"schemaVersion": 99, "positions": {}}),
            ),
            old(30),
        )
        .await;
        let pruned = prune_events_once(pool, &EventsRetentionPolicy::default())
            .await
            .expect("second prune with frozen quads must not error either");
        assert_eq!(pruned, 0);
        assert_eq!(
            remaining_ids(pool).await,
            vec![malformed, latest, frozen_latest]
        );
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

    /// `harness.user_message.enqueued` is the Today bootstrap's dedup evidence;
    /// pruning it would double-send after the horizon with nothing else going red.
    #[tokio::test]
    async fn first_message_dedup_kind_is_never_prunable() {
        assert!(
            !EVENTS_PRUNE_KINDS.contains(&"harness.user_message.enqueued"),
            "the Today bootstrap predicate reads this kind as permanent evidence; \
             pruning it re-opens a duplicate bootstrap after the horizon"
        );

        let repo = repo().await;
        let pool = repo.pool();
        let dedup_evidence =
            insert_event(pool, "harness.user_message.enqueued", "{}", old(400)).await;
        // A sibling allowlisted kind, so the pass provably did work rather than no-opped.
        insert_event(pool, "harness.item.added", "{}", old(400)).await;

        let pruned = prune_events_once(pool, &EventsRetentionPolicy::default())
            .await
            .expect("prune");

        assert_eq!(pruned, 1, "the allowlisted sibling must be pruned");
        assert_eq!(
            remaining_ids(pool).await,
            vec![dedup_evidence],
            "a 400-day-old harness.user_message.enqueued must survive every prune pass"
        );
    }

    #[test]
    fn allowlist_never_contains_structural_or_tombstone_kinds() {
        assert!(!EVENTS_PRUNE_KINDS.contains(&"overlay.deleted"));
        assert!(EVENTS_PRUNE_KINDS.iter().all(|k| !k.starts_with("card.")
            && !k.starts_with("track.")
            && !k.starts_with("area.")
            && !k.starts_with("terminal.")));
    }
}
