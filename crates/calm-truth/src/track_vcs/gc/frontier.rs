//! Call-local coverage certificates and bounded immutable-prefix reconciliation.
//! Released171's coverage epoch remains independently sufficient for older consumers.
use super::{Revision, TreeMeta, revision};
use crate::error::{CalmError, Result};
use sqlx::{QueryBuilder, Row, Sqlite, SqliteConnection, SqlitePool};
use std::collections::{BTreeMap, BTreeSet};

const COMMIT_READ_BATCH: usize = 32;
const COMMIT_NODE_LIMIT: usize = 256;
const COMMIT_ROUND_LIMIT: usize = 32;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct UnknownRevision(i64);
#[derive(Clone, Copy)]
pub(super) struct Epochs {
    pub legacy: Revision,
    pub unknown: UnknownRevision,
}
#[derive(Clone, PartialEq, Eq)]
enum Pending {
    Empty,
    Present { hash: String, track: String },
}
#[derive(Clone)]
pub(super) struct Snapshot {
    roots: BTreeSet<String>,
    pending: Pending,
}
pub(super) struct Certificate {
    pub unknown: UnknownRevision,
    snapshot: Snapshot,
}
// Optional capabilities, not optional schema facts: required epoch reads always fail closed.
#[derive(Default)]
pub(super) struct Certificates {
    pub legacy: Option<Revision>,
    pub frontier: Option<Certificate>,
}
impl Certificates {
    pub fn covers(&self, epochs: Epochs) -> bool {
        self.legacy == Some(epochs.legacy)
            || self
                .frontier
                .as_ref()
                .is_some_and(|p| p.unknown == epochs.unknown)
    }
    pub fn complete(epochs: Epochs, snapshot: Snapshot) -> Self {
        Self {
            legacy: Some(epochs.legacy),
            frontier: Some(Certificate {
                unknown: epochs.unknown,
                snapshot,
            }),
        }
    }
    pub fn bracket(before: Epochs, after: Epochs, snapshot: Snapshot) -> Self {
        Self {
            legacy: (before.legacy == after.legacy).then_some(before.legacy),
            frontier: (before.unknown == after.unknown).then_some(Certificate {
                unknown: before.unknown,
                snapshot,
            }),
        }
    }
}

pub(super) async fn epochs(pool: &SqlitePool) -> Result<Epochs> {
    let mut connection = pool.acquire().await?;
    locked_epochs(&mut connection).await
}
pub(super) async fn locked_epochs(connection: &mut SqliteConnection) -> Result<Epochs> {
    let legacy = revision(&mut *connection).await?;
    let row = sqlx::query(
        "SELECT revision, typeof(revision) AS kind
         FROM track_vcs_gc_frontier_state WHERE singleton=1",
    )
    .fetch_one(&mut *connection)
    .await?;
    let value: i64 = row.try_get("revision")?;
    if row.try_get::<String, _>("kind")? != "integer" || value < 0 {
        return Err(CalmError::Internal(
            "track_vcs: invalid unknown GC revision".into(),
        ));
    }
    Ok(Epochs {
        legacy,
        unknown: UnknownRevision(value),
    })
}

pub(super) async fn snapshot(connection: &mut SqliteConnection) -> Result<Snapshot> {
    // Missing raw HEAD strings were never covered and must not become stopping frontiers.
    let roots = sqlx::query_scalar(
        "SELECT c.hash FROM track_vcs_refs r
         JOIN track_vcs_commits c ON c.hash=r.head_hash",
    )
    .fetch_all(&mut *connection)
    .await?
    .into_iter()
    .collect();
    let row = sqlx::query(
        "SELECT q.hash, q.track_id, c.hash AS stored_hash, c.track_id AS stored_track,
                typeof(q.hash) AS hash_type, typeof(q.track_id) AS track_type,
                typeof(c.hash) AS stored_hash_type, typeof(c.track_id) AS stored_track_type
         FROM track_vcs_gc_pending q LEFT JOIN track_vcs_commits c ON c.hash=q.hash
         WHERE q.singleton=1",
    )
    .fetch_optional(&mut *connection)
    .await?;
    let pending = match row {
        None => Pending::Empty,
        Some(row) => {
            let hash: String = row.try_get("hash")?;
            let track: String = row.try_get("track_id")?;
            let stored_hash: Option<String> = row.try_get("stored_hash")?;
            let stored_track: Option<String> = row.try_get("stored_track")?;
            let types_valid = [
                "hash_type",
                "track_type",
                "stored_hash_type",
                "stored_track_type",
            ]
            .into_iter()
            .all(|key| row.try_get::<String, _>(key).is_ok_and(|v| v == "text"));
            if !types_valid
                || stored_hash.as_ref() != Some(&hash)
                || stored_track.as_ref() != Some(&track)
            {
                return Err(CalmError::Internal(
                    "track_vcs: invalid pending GC publication".into(),
                ));
            }
            // A valid unpublished child need not still have the current HEAD as its parent.
            Pending::Present { hash, track }
        }
    };
    Ok(Snapshot { roots, pending })
}

/// None requires outside rebaselining: pending changed or bounded metadata work overflowed.
/// Hints are never treated as roots. Every admitted new root advances by a fresh prefix;
/// old commit bodies cannot change at an equal unknown epoch, including same-hash restores.
pub(super) async fn reconcile(
    connection: &mut SqliteConnection,
    proof: &Certificate,
    current: &Snapshot,
) -> Result<Option<Vec<TreeMeta>>> {
    if proof.snapshot.pending != current.pending {
        return Ok(None);
    }
    let mut todo: BTreeSet<_> = current
        .roots
        .difference(&proof.snapshot.roots)
        .cloned()
        .collect();
    let mut visited = BTreeSet::new();
    let mut trees = BTreeMap::new();
    let mut rounds = 0;
    while !todo.is_empty() {
        if rounds == COMMIT_ROUND_LIMIT || visited.len() == COMMIT_NODE_LIMIT {
            return Ok(None);
        }
        let count = COMMIT_READ_BATCH.min(COMMIT_NODE_LIMIT - visited.len());
        let batch: Vec<_> = (0..count).filter_map(|_| todo.pop_first()).collect();
        visited.extend(batch.iter().cloned());
        rounds += 1;
        let mut query = QueryBuilder::<Sqlite>::new(
            "SELECT c.hash, c.parent_hash, c.tree_hash, o.kind, length(o.bytes)
             FROM track_vcs_commits c LEFT JOIN track_vcs_objects o ON o.hash=c.tree_hash
             WHERE c.hash IN (",
        );
        let mut separated = query.separated(", ");
        for hash in &batch {
            separated.push_bind(hash);
        }
        separated.push_unseparated(")");
        let rows: Vec<(String, Option<String>, String, Option<String>, Option<i64>)> =
            query.build_query_as().fetch_all(&mut *connection).await?;
        // Missing parent rows terminate exactly as the authoritative recursive JOIN does.
        for (_, parent, hash, kind, length) in rows {
            let tree = TreeMeta::checked(hash, kind, length)?;
            trees.insert(tree.hash.clone(), tree);
            if let Some(parent) = parent
                && !visited.contains(&parent)
                && !proof.snapshot.roots.contains(&parent)
            {
                todo.insert(parent);
            }
        }
    }
    Ok(Some(trees.into_values().collect()))
}
