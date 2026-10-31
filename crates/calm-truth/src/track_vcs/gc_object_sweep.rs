//! Object GC: observe immutable trees without a writer, then validate and delete bounded batches.
use crate::db::sqlite::begin_immediate_tx;
use crate::error::{CalmError, Result};
use crate::model::now_ms;
use crate::track_vcs::{ObjectHash, TreeManifest};
use sqlx::{Executor, QueryBuilder, Row, Sqlite, SqlitePool};
use std::collections::BTreeSet;
use std::time::Duration;

const OBJECT_SWEEP_INTERVAL: Duration = Duration::from_secs(60 * 60);
const OBJECT_SWEEP_GRACE_MS: i64 = 60 * 60 * 1000;
const OBJECT_SCAN_PAGE_SIZE: i64 = 256;
pub(super) const DELETE_BATCH_SIZE: usize = 256;
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
    SELECT rooted.tree_hash, o.kind, o.decoded_size
    FROM (SELECT DISTINCT tree_hash FROM reachable) AS rooted
    LEFT JOIN track_vcs_object_sizes AS o ON o.hash = rooted.tree_hash
"#;

struct TreeMeta {
    hash: ObjectHash,
    byte_len: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Revision(i64);

#[path = "gc/frontier.rs"]
mod frontier;
use frontier::Certificates;

#[derive(Default)]
struct Observed {
    trees: BTreeSet<ObjectHash>,
    live: BTreeSet<ObjectHash>,
    proof: Certificates,
}
impl Observed {
    fn observe(&mut self, hash: String, manifest: TreeManifest) -> Result<()> {
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
pub fn spawn_unreferenced_object_sweeper(pool: crate::db::sqlite::SqliteDb) {
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
pub async fn sweep_unreferenced_objects_once(pool: &crate::db::sqlite::SqliteDb) -> Result<u64> {
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

async fn sweep_batches(
    pool: &crate::db::sqlite::SqliteDb,
    cutoff: i64,
    deleted: &mut u64,
) -> Result<()> {
    let mut observed = Observed::default();
    prepare_full(pool.read_pool(), &mut observed).await?;
    let mut cursor = String::new();
    // Index pages and deletion batches have different costs. Accumulate actual old orphan
    // candidates across sparse live pages before paying for the full ancestry proof.
    let mut pending = Vec::new();
    loop {
        let page: Vec<String> = sqlx::query_scalar(
            "SELECT hash FROM track_vcs_objects WHERE hash > ?1 ORDER BY hash LIMIT ?2",
        )
        .bind(&cursor)
        .bind(OBJECT_SCAN_PAGE_SIZE)
        .fetch_all(pool.read_pool())
        .await?;
        let Some(last) = page.last() else {
            if !pending.is_empty() {
                flush_candidates(pool, cutoff, pending, &mut observed, deleted).await?;
            }
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
        pending.extend(old_candidates(pool.read_pool(), cutoff, &unmarked).await?);
        // Before extending, pending held fewer than DELETE_BATCH_SIZE entries; a scan page
        // adds at most OBJECT_SCAN_PAGE_SIZE. Keep the remainder and consume each batch even
        // if fresh live marks or age changes make its committed deletion count zero.
        while pending.len() >= DELETE_BATCH_SIZE {
            let remainder = pending.split_off(DELETE_BATCH_SIZE);
            let candidates = std::mem::replace(&mut pending, remainder);
            if !flush_candidates(pool, cutoff, candidates, &mut observed, deleted).await? {
                return Ok(());
            }
        }
    }
}

async fn flush_candidates(
    pool: &crate::db::sqlite::SqliteDb,
    cutoff: i64,
    candidates: Vec<String>,
    observed: &mut Observed,
    deleted: &mut u64,
) -> Result<bool> {
    match delete_validated_batch(pool, cutoff, candidates, observed).await? {
        Some(count) => {
            *deleted += count;
            Ok(true)
        }
        None => {
            tracing::info!(
                deleted = *deleted,
                "track_vcs: object sweep deferred remaining candidates after repeated closure changes"
            );
            Ok(false)
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

// Missing, malformed or negative required state is an error, never a cache miss.
async fn revision<'e>(executor: impl Executor<'e, Database = Sqlite>) -> Result<Revision> {
    let row = sqlx::query(
        "SELECT revision, typeof(revision) AS revision_type FROM track_vcs_gc_state WHERE singleton = 1",
    )
    .fetch_one(executor)
    .await?;
    let kind: String = row.try_get("revision_type")?;
    if kind != "integer" {
        return Err(CalmError::Internal(
            "track_vcs: invalid GC revision type".into(),
        ));
    }
    let value: i64 = row.try_get("revision")?;
    if value < 0 {
        return Err(CalmError::Internal(
            "track_vcs: invalid negative GC revision".into(),
        ));
    }
    Ok(Revision(value))
}

// A complete outside observation is the only full-preparation path. Metadata alone is
// not authority: grant certificates only after every selected payload has been parsed.
async fn prepare_full(pool: &SqlitePool, observed: &mut Observed) -> Result<()> {
    let before = frontier::epochs(pool).await?;
    let snapshot = {
        let mut connection = pool.acquire().await?;
        frontier::snapshot(&mut connection).await?
    };
    let trees = reachable_trees(pool).await?;
    #[cfg(test)]
    super::gc_writer_tests::at_phase(super::gc_writer_tests::Phase::AfterMetadata).await;
    observe_external(pool, &trees, observed).await?;
    let after = frontier::epochs(pool).await?;
    observed.proof = Certificates::bracket(before, after, snapshot);
    Ok(())
}

async fn prepare_validation(pool: &SqlitePool, observed: &mut Observed) -> Result<()> {
    let current = frontier::epochs(pool).await?;
    if !observed.proof.covers(current) {
        // One outside attempt only. Unknown churn retains the authoritative locked path.
        prepare_full(pool, observed).await?;
    }
    Ok(())
}

async fn reachable_trees<'e>(
    executor: impl Executor<'e, Database = Sqlite>,
) -> Result<Vec<TreeMeta>> {
    #[cfg(test)]
    super::gc_writer_tests::record_metadata_query();
    let rows: Vec<(String, Option<String>, Option<i64>)> =
        sqlx::query_as(REACHABLE_TREES).fetch_all(executor).await?;
    rows.into_iter()
        .map(|(hash, kind, length)| TreeMeta::checked(hash, kind, length))
        .collect()
}

impl TreeMeta {
    fn checked(hash: String, kind: Option<String>, length: Option<i64>) -> Result<Self> {
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
        Ok(Self { hash, byte_len })
    }
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
        "SELECT hash, kind, bytes, encoding, decoded_size FROM track_vcs_objects WHERE hash IN (",
    );
    let mut separated = query.separated(", ");
    for tree in trees {
        separated.push_bind(&tree.hash);
    }
    separated.push_unseparated(")");
    let rows: Vec<crate::track_vcs::object_codec::StoredObject> =
        query.build_query_as().fetch_all(executor).await?;
    // fetch_all has completed before parsing. For external observations its pooled connection
    // has been released; no full-payload cursor/transaction is kept across CPU work.
    #[cfg(test)]
    super::gc_writer_tests::pause_before_manifest_parse().await;
    let fetched: BTreeSet<_> = rows.iter().map(|row| row.hash.as_str()).collect();
    for tree in trees {
        if !fetched.contains(tree.hash.as_str()) {
            return Err(CalmError::Internal(format!(
                "track_vcs: reachable tree object {} disappeared during observation",
                tree.hash
            )));
        }
    }
    for row in rows {
        let hash = row.hash.clone();
        let kind = &row.kind;
        if kind != "tree" {
            return Err(CalmError::Internal(format!(
                "track_vcs: reachable tree object {hash} is not a tree"
            )));
        }
        observed.observe(hash, row.manifest()?)?;
    }
    Ok(())
}

async fn delete_validated_batch(
    pool: &crate::db::sqlite::SqliteDb,
    cutoff: i64,
    candidates: Vec<String>,
    observed: &mut Observed,
) -> Result<Option<u64>> {
    for attempt in 0..VALIDATION_ATTEMPTS {
        prepare_validation(pool.read_pool(), observed).await?;
        #[cfg(test)]
        super::gc_writer_tests::at_phase(super::gc_writer_tests::Phase::BeforeDelete).await;
        let mut tx = begin_immediate_tx(pool).await?;
        let locked = frontier::locked_epochs(&mut tx).await?;
        let snapshot = frontier::snapshot(&mut tx).await?;
        let (current, reuse_parsed) = if observed.proof.legacy == Some(locked.legacy) {
            // Released171 is independently sufficient, even after conservative U bumps.
            (Vec::new(), true)
        } else if let Some(proof) = observed.proof.frontier.as_ref()
            && proof.unknown == locked.unknown
        {
            match frontier::reconcile(&mut tx, proof, &snapshot).await? {
                Some(trees) => (trees, true),
                None => {
                    tx.rollback().await?;
                    if attempt + 1 == VALIDATION_ATTEMPTS {
                        return Ok(None);
                    }
                    prepare_full(pool.read_pool(), observed).await?;
                    continue;
                }
            }
        } else {
            // Unknown mutations may change cached object bytes. A full query proves metadata,
            // but all its payloads must be re-observed before granting a new certificate.
            (reachable_trees(&mut **tx).await?, false)
        };
        let delta: Vec<_> = current
            .into_iter()
            .filter(|tree| !reuse_parsed || !observed.trees.contains(&tree.hash))
            .collect();
        let bytes = delta
            .iter()
            .fold(0u64, |sum, tree| sum.saturating_add(tree.byte_len));
        if delta.len() > INLINE_TREE_LIMIT || bytes > INLINE_BYTE_LIMIT {
            tx.rollback().await?;
            if attempt + 1 == VALIDATION_ATTEMPTS {
                return Ok(None);
            }
            prepare_full(pool.read_pool(), observed).await?;
            continue;
        }
        // Both count and byte limits are checked before fetching or parsing any inline payload.
        // This bounded delta prevents ordinary continuous commits starving a full external scan.
        observe_batch(&mut **tx, &delta, observed).await?;
        let candidates: Vec<_> = candidates
            .iter()
            .filter(|hash| !observed.live.contains(*hash))
            .collect();
        if candidates.is_empty() {
            tx.commit().await?;
            observed.proof = Certificates::complete(locked, snapshot);
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
        let count = query.build().execute(&mut **tx).await?.rows_affected();
        // Only objects proven unreachable were deleted. Their trigger increments preserve this
        // complete live proof, but do not advance its local certificate until COMMIT acknowledges.
        let after_delete = frontier::locked_epochs(&mut tx).await?;
        tx.commit().await?;
        observed.proof = Certificates::complete(after_delete, snapshot);
        return Ok(Some(count));
    }
    unreachable!("bounded validation either deletes or defers")
}
