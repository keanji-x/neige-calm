//! Object GC: observe immutable trees without a writer, then validate and delete bounded batches.
use crate::db::sqlite::begin_immediate_tx;
use crate::error::{CalmError, Result};
use crate::model::now_ms;
use crate::track_vcs::{ObjectHash, TreeManifest};
use sqlx::{Executor, QueryBuilder, Sqlite, SqlitePool};
use std::collections::BTreeSet;
use std::time::Duration;

const OBJECT_SWEEP_INTERVAL: Duration = Duration::from_secs(60 * 60);
const OBJECT_SWEEP_GRACE_MS: i64 = 60 * 60 * 1000;
const OBJECT_PAGE_SIZE: i64 = 256;
const TREE_READ_BATCH: usize = 32;
const INLINE_TREE_LIMIT: usize = 32;
const INLINE_BYTE_LIMIT: u64 = 1024 * 1024;
const VALIDATION_ATTEMPTS: usize = 3;

// Root on HEADs, not every remaining commit: partial history pruning may leave detached rows.
// Carry the only required state (parent, tree), so each commit is read once and objects are
// joined only after tree deduplication. Equal pairs emit the same tree and have the same next
// parent; UNION can merge them without changing reachability and still terminates on cycles.
// LEFT JOIN makes missing/wrong-kind trees visible instead of treating them as empty manifests.
const REACHABLE_TREES: &str = r#"
    WITH RECURSIVE reachable(parent_hash, tree_hash) AS (
        SELECT c.parent_hash, c.tree_hash
        FROM track_vcs_refs AS refs
        JOIN track_vcs_commits AS c ON c.hash = refs.head_hash
        UNION
        SELECT c.parent_hash, c.tree_hash
        FROM track_vcs_commits AS c
        JOIN reachable AS r ON c.hash = r.parent_hash
    )
    SELECT rooted.tree_hash, o.kind, length(o.bytes)
    FROM (SELECT DISTINCT tree_hash FROM reachable) AS rooted
    LEFT JOIN track_vcs_objects AS o ON o.hash = rooted.tree_hash
"#;

struct TreeMeta {
    hash: ObjectHash,
    byte_len: u64,
}

#[derive(Default)]
struct Observed {
    trees: BTreeSet<ObjectHash>,
    live: BTreeSet<ObjectHash>,
}
impl Observed {
    fn observe(&mut self, hash: String, bytes: &[u8]) -> Result<()> {
        let manifest: TreeManifest = serde_json::from_slice(bytes).map_err(|e| {
            CalmError::Internal(format!("track_vcs: parse tree manifest object {hash}: {e}"))
        })?;
        // The owner hashes schema/entries and INSERT OR IGNOREs immutable objects. Re-reading
        // the same tree hash cannot introduce different references through production writers.
        self.live
            .extend(manifest.entries.into_values().map(|entry| entry.blob_hash));
        self.live.insert(hash.clone());
        self.trees.insert(hash);
        Ok(())
    }
}

/// Content-addressed blobs can be shared across tracks and are swept after a grace window.
pub fn spawn_unreferenced_object_sweeper(pool: SqlitePool) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(OBJECT_SWEEP_INTERVAL);
        tick.tick().await;
        loop {
            tick.tick().await;
            if let Err(e) = sweep_unreferenced_objects_once(&pool).await {
                tracing::warn!(error = %e, "track_vcs: object sweep failed");
            }
        }
    });
}

/// Drain old orphan candidates in separate validated transactions. Expensive manifest reads and
/// parsing run outside the writer; a small newly reachable delta may be observed inside it.
/// Concurrent churn can stop this call after safe partial progress; the return value is actual
/// committed deletions. A later error does not roll back earlier successful batches.
pub async fn sweep_unreferenced_objects_once(pool: &SqlitePool) -> Result<u64> {
    let cutoff = now_ms().saturating_sub(OBJECT_SWEEP_GRACE_MS);
    let mut deleted = 0;
    if let Err(error) = sweep_batches(pool, cutoff, &mut deleted).await {
        tracing::warn!(deleted, error = %error,
            "track_vcs: object sweep stopped; earlier deletion batches remain committed");
        return Err(error);
    }
    if deleted > 0 {
        tracing::info!(deleted, "track_vcs: swept unreferenced objects");
    }
    Ok(deleted)
}

async fn sweep_batches(pool: &SqlitePool, cutoff: i64, deleted: &mut u64) -> Result<()> {
    // One statement is a coherent metadata snapshot. It performs no deferred transaction or
    // read-to-write upgrade; every later delete revalidates the closure in its own IMMEDIATE.
    let initial = reachable_trees(pool).await?;
    let mut observed = Observed::default();
    observe_external(pool, &initial, &mut observed).await?;
    let mut cursor = String::new();
    loop {
        let page: Vec<String> = sqlx::query_scalar(
            "SELECT hash FROM track_vcs_objects WHERE hash > ?1 ORDER BY hash LIMIT ?2",
        )
        .bind(&cursor)
        .bind(OBJECT_PAGE_SIZE)
        .fetch_all(pool)
        .await?;
        let Some(last) = page.last() else {
            return Ok(());
        };
        cursor.clone_from(last);
        // This first page uses only the covering hash index. Do not fetch the large object
        // records' trailing age column for hashes already proven live.
        let unmarked: Vec<_> = page
            .into_iter()
            .filter(|hash| !observed.live.contains(hash))
            .collect();
        if unmarked.is_empty() {
            continue;
        }
        let candidates = old_candidates(pool, cutoff, &unmarked).await?;
        if candidates.is_empty() {
            continue;
        }
        match delete_validated_batch(pool, cutoff, candidates, &mut observed).await? {
            Some(count) => *deleted += count,
            None => {
                tracing::info!(
                    deleted = *deleted,
                    "track_vcs: object sweep deferred remaining candidates after repeated closure changes"
                );
                return Ok(());
            }
        }
    }
}

async fn old_candidates(pool: &SqlitePool, cutoff: i64, hashes: &[String]) -> Result<Vec<String>> {
    let mut query =
        QueryBuilder::<Sqlite>::new("SELECT hash FROM track_vcs_objects WHERE created_at < ");
    query.push_bind(cutoff).push(" AND hash IN (");
    let mut separated = query.separated(", ");
    for hash in hashes {
        separated.push_bind(hash);
    }
    separated.push_unseparated(")");
    Ok(query.build_query_scalar().fetch_all(pool).await?)
}

async fn reachable_trees<'e>(
    executor: impl Executor<'e, Database = Sqlite>,
) -> Result<Vec<TreeMeta>> {
    let rows: Vec<(String, Option<String>, Option<i64>)> =
        sqlx::query_as(REACHABLE_TREES).fetch_all(executor).await?;
    rows.into_iter()
        .map(|(hash, kind, length)| {
            if kind.as_deref() != Some("tree") {
                return Err(CalmError::Internal(format!(
                    "track_vcs: reachable tree object {hash} is missing or not a tree"
                )));
            }
            let byte_len = length
                .and_then(|length| u64::try_from(length).ok())
                .ok_or_else(|| {
                    CalmError::Internal(format!("track_vcs: invalid reachable tree length {hash}"))
                })?;
            Ok(TreeMeta { hash, byte_len })
        })
        .collect()
}

async fn observe_external(
    pool: &SqlitePool,
    trees: &[TreeMeta],
    observed: &mut Observed,
) -> Result<()> {
    for batch in trees.chunks(TREE_READ_BATCH) {
        observe_batch(pool, batch, observed).await?;
    }
    Ok(())
}

async fn observe_batch<'e>(
    executor: impl Executor<'e, Database = Sqlite>,
    trees: &[TreeMeta],
    observed: &mut Observed,
) -> Result<()> {
    if trees.is_empty() {
        return Ok(());
    }
    let mut query = QueryBuilder::<Sqlite>::new(
        "SELECT hash, kind, bytes FROM track_vcs_objects WHERE hash IN (",
    );
    let mut separated = query.separated(", ");
    for tree in trees {
        separated.push_bind(&tree.hash);
    }
    separated.push_unseparated(")");
    let rows: Vec<(String, String, Vec<u8>)> = query.build_query_as().fetch_all(executor).await?;
    // fetch_all has completed before parsing. For external observations its pooled connection
    // has been released; no full-payload cursor/transaction is kept across CPU work.
    #[cfg(test)]
    super::gc_writer_tests::pause_before_manifest_parse().await;
    let fetched: BTreeSet<_> = rows.iter().map(|(hash, _, _)| hash.as_str()).collect();
    for tree in trees {
        if !fetched.contains(tree.hash.as_str()) {
            return Err(CalmError::Internal(format!(
                "track_vcs: reachable tree object {} disappeared during observation",
                tree.hash
            )));
        }
    }
    for (hash, kind, bytes) in rows {
        if kind != "tree" {
            return Err(CalmError::Internal(format!(
                "track_vcs: reachable tree object {hash} is not a tree"
            )));
        }
        observed.observe(hash, &bytes)?;
    }
    Ok(())
}

async fn delete_validated_batch(
    pool: &SqlitePool,
    cutoff: i64,
    candidates: Vec<String>,
    observed: &mut Observed,
) -> Result<Option<u64>> {
    for attempt in 0..VALIDATION_ATTEMPTS {
        #[cfg(test)]
        super::gc_writer_tests::at_phase(super::gc_writer_tests::Phase::BeforeDelete).await;
        let mut tx = begin_immediate_tx(pool).await?;
        // Recompute actual ancestry, not just HEAD equality. A restored ancestor can make an
        // old tree live without moving HEAD. Check existence/kind even for cached trees.
        let current = reachable_trees(&mut *tx).await?;
        let delta: Vec<_> = current
            .into_iter()
            .filter(|tree| !observed.trees.contains(&tree.hash))
            .collect();
        let bytes = delta
            .iter()
            .fold(0u64, |sum, tree| sum.saturating_add(tree.byte_len));
        if delta.len() > INLINE_TREE_LIMIT || bytes > INLINE_BYTE_LIMIT {
            tx.rollback().await?;
            if attempt + 1 == VALIDATION_ATTEMPTS {
                return Ok(None);
            }
            observe_external(pool, &delta, observed).await?;
            continue;
        }
        // Both count and byte limits are checked before fetching or parsing any inline payload.
        // This bounded delta prevents ordinary continuous commits starving a full external scan.
        observe_batch(&mut *tx, &delta, observed).await?;
        let candidates: Vec<_> = candidates
            .iter()
            .filter(|hash| !observed.live.contains(*hash))
            .collect();
        if candidates.is_empty() {
            tx.commit().await?;
            return Ok(Some(0));
        }
        let mut query =
            QueryBuilder::<Sqlite>::new("DELETE FROM track_vcs_objects WHERE created_at < ");
        query.push_bind(cutoff).push(" AND hash IN (");
        let mut separated = query.separated(", ");
        for hash in candidates {
            separated.push_bind(hash);
        }
        separated.push_unseparated(")");
        let count = query.build().execute(&mut *tx).await?.rows_affected();
        tx.commit().await?;
        return Ok(Some(count));
    }
    unreachable!("bounded validation either deletes or defers")
}
