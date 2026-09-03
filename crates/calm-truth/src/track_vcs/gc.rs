use crate::db::sqlite::begin_immediate_tx;
use crate::error::{CalmError, Result};
use crate::ids::TrackId;
use crate::model::now_ms;
use serde_json::Value;
use sqlx::{QueryBuilder, Row, Sqlite, SqlitePool, Transaction};
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use super::store::{head_in_tx, load_commit_record_for_track_tx};
use super::{CommitHash, CommitRecord, ObjectHash, TreeManifest};

const OBJECT_SWEEP_INTERVAL: Duration = Duration::from_secs(60 * 60);
const OBJECT_SWEEP_GRACE_MS: i64 = 60 * 60 * 1000;
pub(super) const TRACK_HISTORY_PRUNE_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
// Keep aligned with the existing `neige track-gc` default.
pub(super) const DEFAULT_TRACK_HISTORY_PRUNE_KEEP: usize = 50;
pub(super) const TRACK_HISTORY_PRUNE_INTERVAL_SECS_ENV: &str = "NEIGE_TRACK_PRUNE_INTERVAL_SECS";
pub(super) const TRACK_HISTORY_PRUNE_KEEP_ENV: &str = "NEIGE_TRACK_PRUNE_KEEP";

/// Prune old linear track history while preserving every commit an active
/// harness may still need for endpoint-only `diff(previous_endpoint, HEAD)`.
///
/// The grace floor is the minimum `created_at` among all protected commits,
/// not a maximum. Keeping every commit at or after the oldest protected
/// endpoint is intentionally conservative: it preserves the contiguous suffix
/// anchored at HEAD and errs toward keeping more history instead of deleting a
/// commit an active session may still reference.
pub async fn prune_track_history_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track_id: &TrackId,
    keep: usize,
) -> Result<u64> {
    let Some(head_hash) = head_in_tx(tx, track_id).await? else {
        return Ok(0);
    };

    let keep = keep.max(1);
    let mut protected = BTreeMap::<CommitHash, i64>::new();
    let mut cursor = Some(head_hash);
    for _ in 0..keep {
        let Some(hash) = cursor else {
            break;
        };
        let Some(record) = load_commit_record_for_track_tx(tx, track_id, &hash).await? else {
            if protected.is_empty() {
                tracing::warn!(
                    target: "track_vcs",
                    track_id = %track_id.as_str(),
                    commit_hash = %hash,
                    "track-vcs prune: HEAD ref points at a missing commit; skipping prune"
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

    let active_endpoints = match active_diff_endpoint_commits_tx(tx, track_id).await? {
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
        r#"CREATE TEMP TABLE IF NOT EXISTS track_vcs_prune_keep (
               hash TEXT PRIMARY KEY
           )"#,
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query("DELETE FROM track_vcs_prune_keep")
        .execute(&mut **tx)
        .await?;
    insert_prune_keep_refs_tx(tx, protected.keys()).await?;

    let result = sqlx::query(
        r#"DELETE FROM track_vcs_commits
           WHERE track_id = ?1
             AND created_at < ?2
             AND NOT EXISTS (
               SELECT 1
               FROM track_vcs_prune_keep AS keep
               WHERE keep.hash = track_vcs_commits.hash
             )"#,
    )
    .bind(track_id.as_str())
    .bind(floor)
    .execute(&mut **tx)
    .await?;
    Ok(result.rows_affected())
}

/// Spawn the track-history pruner. It runs the same keep-N prune used by the
/// manual admin GC path, across all tracks, without running VACUUM.
pub fn spawn_track_history_pruner(pool: SqlitePool) {
    let Some((interval, keep)) = track_history_pruner_config_from_env() else {
        tracing::info!("track_vcs: history pruner disabled");
        return;
    };

    tokio::spawn(async move {
        let mut tick = tokio::time::interval(interval);
        // Match the object sweeper: skip the immediate boot tick and let the
        // server settle before taking SQLite writer locks for cleanup.
        tick.tick().await;
        loop {
            tick.tick().await;
            if let Err(e) = prune_all_tracks_once(&pool, keep).await {
                tracing::warn!(error = %e, "track_vcs: history prune failed");
            }
        }
    });
}

pub(super) fn track_history_pruner_config_from_env() -> Option<(Duration, usize)> {
    let interval = match std::env::var(TRACK_HISTORY_PRUNE_INTERVAL_SECS_ENV) {
        Ok(raw) => match raw.trim().parse::<u64>() {
            Ok(0) => return None,
            Ok(secs) => Duration::from_secs(secs),
            Err(_) => TRACK_HISTORY_PRUNE_INTERVAL,
        },
        Err(_) => TRACK_HISTORY_PRUNE_INTERVAL,
    };
    let keep = match std::env::var(TRACK_HISTORY_PRUNE_KEEP_ENV) {
        Ok(raw) => match raw.trim().parse::<usize>() {
            Ok(n) if n > 0 => n,
            _ => DEFAULT_TRACK_HISTORY_PRUNE_KEEP,
        },
        Err(_) => DEFAULT_TRACK_HISTORY_PRUNE_KEEP,
    };
    Some((interval, keep))
}

/// One all-track history prune pass. Public so integration tests can drive
/// cleanup deterministically without waiting for the scheduled task.
pub async fn prune_all_tracks_once(pool: &SqlitePool, keep: usize) -> Result<u64> {
    let track_ids: Vec<String> = sqlx::query_scalar("SELECT id FROM tracks ORDER BY id")
        .fetch_all(pool)
        .await?;
    let track_count = track_ids.len();
    let mut total_pruned = 0;

    for track_id in track_ids {
        let track_id = TrackId::from(track_id);
        let mut tx = match begin_immediate_tx(pool).await {
            Ok(tx) => tx,
            Err(e) => {
                tracing::warn!(
                    track_id = %track_id.as_str(),
                    error = %e,
                    "track_vcs: prune failed for track; continuing"
                );
                continue;
            }
        };
        let res = prune_track_history_tx(&mut tx, &track_id, keep).await;
        match res {
            Ok(pruned) => {
                if let Err(e) = tx.commit().await {
                    tracing::warn!(
                        track_id = %track_id.as_str(),
                        error = %e,
                        "track_vcs: prune failed for track; continuing"
                    );
                } else {
                    total_pruned += pruned;
                }
            }
            Err(e) => {
                let _ = tx.rollback().await;
                tracing::warn!(
                    track_id = %track_id.as_str(),
                    error = %e,
                    "track_vcs: prune failed for track; continuing"
                );
            }
        }
    }

    if total_pruned > 0 {
        tracing::info!(
            pruned = total_pruned,
            tracks = track_count,
            keep,
            "track_vcs: pruned track history"
        );
    }
    Ok(total_pruned)
}

/// Spawn the unreferenced-object sweeper. Content-addressed objects are not
/// deleted by `track_delete_tx` / `area_delete_tx` because blobs can be shared
/// across tracks; this hourly fallback reclaims rows no live commit references.
pub fn spawn_unreferenced_object_sweeper(pool: SqlitePool) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(OBJECT_SWEEP_INTERVAL);
        // Match the terminal sweeper: skip the immediate boot tick and let the
        // server settle before taking the SQLite writer lock for cleanup.
        tick.tick().await;
        loop {
            tick.tick().await;
            if let Err(e) = sweep_unreferenced_objects_once(&pool).await {
                tracing::warn!(error = %e, "track_vcs: object sweep failed");
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
                tracing::info!(deleted, "track_vcs: swept unreferenced objects");
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
        r#"CREATE TEMP TABLE IF NOT EXISTS track_vcs_sweep_refs (
               hash TEXT PRIMARY KEY
           )"#,
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query("DELETE FROM track_vcs_sweep_refs")
        .execute(&mut **tx)
        .await?;
    // Re-rooted on HEAD refs (#722 B.2). Tree-rooted
    // `SELECT DISTINCT tree_hash` would keep trees of pruned-but-not-yet-swept
    // commit rows alive across a partial prune; ref-rooting matches the
    // prune's reachability so orphaned objects are actually reclaimed.
    sqlx::query(
        r#"INSERT OR IGNORE INTO track_vcs_sweep_refs(hash)
           SELECT DISTINCT c.tree_hash
           FROM track_vcs_commits AS c
           WHERE c.hash IN (
               WITH RECURSIVE reachable(hash) AS (
                   SELECT head_hash FROM track_vcs_refs
                   UNION
                   SELECT c2.parent_hash
                   FROM track_vcs_commits AS c2
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
           FROM track_vcs_sweep_refs AS refs
           JOIN track_vcs_objects AS o ON o.hash = refs.hash
           WHERE o.kind = 'tree'"#,
    )
    .fetch_all(&mut **tx)
    .await?;

    let mut blob_hashes = BTreeSet::new();
    for (tree_hash, bytes) in tree_rows {
        let manifest: TreeManifest = serde_json::from_slice(&bytes).map_err(|e| {
            CalmError::Internal(format!(
                "track_vcs: parse tree manifest object {tree_hash}: {e}"
            ))
        })?;
        blob_hashes.extend(manifest.entries.into_values().map(|entry| entry.blob_hash));
    }
    insert_sweep_refs_tx(tx, blob_hashes).await?;

    let result = sqlx::query(
        r#"DELETE FROM track_vcs_objects
           WHERE created_at < ?1
             AND NOT EXISTS (
               SELECT 1
               FROM track_vcs_sweep_refs AS refs
               WHERE refs.hash = track_vcs_objects.hash
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
            QueryBuilder::new("INSERT OR IGNORE INTO track_vcs_sweep_refs(hash) ");
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
            QueryBuilder::new("INSERT OR IGNORE INTO track_vcs_prune_keep(hash) ");
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
    track_id: &TrackId,
) -> Result<ActiveLastSeenHeads> {
    let rows = sqlx::query(
        r#"SELECT id, handle_state_json
           FROM worker_sessions
           WHERE track_id = ?1
             AND state IN ('starting', 'running', 'idle', 'turn_pending')
           ORDER BY created_at_ms ASC, id ASC"#,
    )
    .bind(track_id.as_str())
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
                    target: "track_vcs",
                    track_id = %track_id.as_str(),
                    session_id = %session_id,
                    error = %e,
                    "track-vcs prune: active session snapshot is not parseable; skipping prune"
                );
                return Ok(ActiveLastSeenHeads::SkipPrune);
            }
        };

        let endpoints = match harness_snapshot_diff_endpoints(&value) {
            Ok(endpoints) => endpoints,
            Err(reason) => {
                tracing::warn!(
                    target: "track_vcs",
                    track_id = %track_id.as_str(),
                    session_id = %session_id,
                    reason = %reason,
                    "track-vcs prune: active session snapshot is ambiguous; skipping prune"
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
            let Some(record) = load_commit_record_for_track_tx(tx, track_id, endpoint_hash).await?
            else {
                tracing::warn!(
                    target: "track_vcs",
                    track_id = %track_id.as_str(),
                    session_id = %session_id,
                    endpoint = endpoint_name,
                    commit_hash = %endpoint_hash,
                    "track-vcs prune: active session references an absent commit; skipping prune"
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
