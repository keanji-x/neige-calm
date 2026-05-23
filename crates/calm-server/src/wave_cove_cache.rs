//! In-memory `WaveId -> CoveId` cache used by `role_gate::enforce_role`.
//!
//! ## Why a cache
//!
//! Mirrors [`crate::card_role_cache::CardRoleCache`] in shape and rationale.
//! The role gate's Worker arm needs to confirm `scope.cove == cache.cove_of(home_wave)`
//! at every audited write (see #234), and `cove_id` lives on the parent
//! `waves` row — not directly addressable from the `CardRoleCache` entry.
//! Putting cove on the card cache would denormalize the value across every
//! card row (card-count >> wave-count); a separate `WaveId -> CoveId` map
//! keeps storage proportional to waves.
//!
//! Same write-through invariant:
//!
//!   * `wave_create_tx` calls `cache.insert(wave_id, cove_id)` right after
//!     the SQL insert succeeds, *before* the surrounding transaction
//!     commits. A subsequent emit inside the same `write_with_event`
//!     closure therefore sees the freshly-minted wave→cove binding without
//!     waiting for the commit.
//!   * `seed_from_db` repopulates the cache at boot from the `waves`
//!     table. Crash safety: any restart sees the persisted `cove_id` and
//!     reconstitutes the cache before the first write lands.
//!   * `remove` is called from the wave-delete path so the binding doesn't
//!     linger past the row's lifetime.
//!
//! Cove is immutable per wave (`WavePatch` has no `cove_id` field; a wave
//! can't migrate coves), so there's no update path to keep in sync.
//!
//! ## What the cache is *not*
//!
//! It is **not** an authorization decision by itself. The decision lives
//! in `role_gate::enforce_role` — the cache is a read-side optimization
//! and a same-tx propagation mechanism. A cache miss at decision time is
//! treated as **deny** by `enforce_role` for Worker `AiCodex` actors
//! (defense in depth — a race between wave-delete and an in-flight emit
//! means the worker is writing into a wave that no longer exists, and
//! we'd rather drop the write than admit a sketchy one).

use crate::error::Result;
use crate::ids::{CoveId, WaveId};
use dashmap::DashMap;
use sqlx::SqlitePool;
use std::sync::Arc;

/// Concurrent `WaveId -> CoveId` map populated at boot from the `waves`
/// table and maintained write-through by every insert / delete path.
///
/// `Clone` is cheap — the inner `Arc<DashMap<...>>` shares state, so
/// stashing one copy on `AppState::wave_cove_cache` and another inside
/// the FSM / sweeper / dispatcher task closures costs nothing beyond
/// the `Arc` clone.
#[derive(Clone, Default)]
pub struct WaveCoveCache(Arc<DashMap<WaveId, CoveId>>);

impl WaveCoveCache {
    /// Empty cache. Same as `Default::default()` — explicit constructor
    /// because the test scaffolding reads slightly cleaner with `new()`
    /// at the call site.
    pub fn new() -> Self {
        Self::default()
    }

    /// Look up the cove for a wave. `None` means "no entry" — that's
    /// either a wave that hasn't been inserted yet (impossible under
    /// the write-through invariant) or a wave whose row was deleted
    /// (the only legitimate way to see this in production).
    pub fn cove_of(&self, id: &WaveId) -> Option<CoveId> {
        self.0.get(id).map(|c| c.value().clone())
    }

    /// Write-through insert. Called from `wave_create_tx` after the
    /// SQL succeeds but before the surrounding transaction commits;
    /// the same-tx visibility lets a follow-up emit inside the same
    /// `write_with_event` closure see the freshly-minted binding.
    ///
    /// If the txn rolls back the cache will hold a stale entry until
    /// `seed_from_db` (next boot) overwrites it — see `seed_from_db`'s
    /// `clear-then-populate` semantics. Tolerating stale entries on the
    /// failed-write path is the price for commit-then-emit ordering
    /// staying simple; the consequence is at worst an `enforce_role`
    /// that *permits* a write the DB would have rejected on its FK
    /// (the wave row no longer exists), which the transactional layer
    /// surfaces as `NotFound` anyway.
    pub fn insert(&self, wave: WaveId, cove: CoveId) {
        self.0.insert(wave, cove);
    }

    /// Remove a wave's cove entry. Called from the wave-delete path so
    /// the cache shrinks with the table. Safe to call on a missing key.
    pub fn remove(&self, wave: &WaveId) {
        self.0.remove(wave);
    }

    /// Number of entries. Convenience for unit tests + future telemetry;
    /// production code rarely needs the size.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// `true` when no waves have been seeded yet. Mirrors `Vec::is_empty`
    /// — clippy nags if you ship `len()` without it.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Boot-time seed: read every `(id, cove_id)` from `waves` into the
    /// map. Clears the existing contents first so a re-seed during a
    /// long-lived test process (e.g. `AppState::from_parts` with a fresh
    /// pool) doesn't carry stale entries from a previous fixture.
    ///
    /// Production callers run this exactly once from `AppState::new`
    /// after migrations finish — see `state.rs`. Missing tables (e.g.
    /// schema isn't migrated yet) surface as a sqlx error and abort
    /// boot — the caller already runs migrations first.
    pub async fn seed_from_db(&self, pool: &SqlitePool) -> Result<()> {
        self.0.clear();
        let rows: Vec<(String, String)> = sqlx::query_as(r#"SELECT id, cove_id FROM waves"#)
            .fetch_all(pool)
            .await?;
        for (wave_id, cove_id) in rows {
            self.0.insert(WaveId::from(wave_id), CoveId::from(cove_id));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::SqlitePool;

    fn wid(s: &str) -> WaveId {
        WaveId::from(s)
    }

    fn cid(s: &str) -> CoveId {
        CoveId::from(s)
    }

    #[test]
    fn insert_get_remove_round_trip() {
        let c = WaveCoveCache::new();
        assert!(c.is_empty());
        c.insert(wid("w1"), cid("c1"));
        c.insert(wid("w2"), cid("c1"));
        c.insert(wid("w3"), cid("c2"));
        assert_eq!(c.len(), 3);

        assert_eq!(c.cove_of(&wid("w1")), Some(cid("c1")));
        assert_eq!(c.cove_of(&wid("w2")), Some(cid("c1")));
        assert_eq!(c.cove_of(&wid("w3")), Some(cid("c2")));
        assert_eq!(c.cove_of(&wid("missing")), None);

        c.remove(&wid("w2"));
        assert_eq!(c.cove_of(&wid("w2")), None);
        assert_eq!(c.len(), 2);

        // Removing a missing key is a no-op.
        c.remove(&wid("missing"));
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn insert_overwrites_existing() {
        let c = WaveCoveCache::new();
        c.insert(wid("w1"), cid("c1"));
        c.insert(wid("w1"), cid("c2"));
        assert_eq!(c.cove_of(&wid("w1")), Some(cid("c2")));
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn clone_shares_inner_state() {
        // `Clone` is `Arc::clone` — mutations on one handle are visible
        // through the other. Production code relies on this when the
        // cache is stashed on `AppState` and the role gate / dispatcher
        // tasks pull a clone for their own closures.
        let a = WaveCoveCache::new();
        let b = a.clone();
        a.insert(wid("x"), cid("y"));
        assert_eq!(b.cove_of(&wid("x")), Some(cid("y")));
    }

    #[tokio::test]
    async fn seed_from_db_loads_existing_rows() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::query(
            r#"CREATE TABLE waves (
                id TEXT PRIMARY KEY,
                cove_id TEXT NOT NULL
            )"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO waves (id, cove_id) VALUES \
             ('w1', 'c1'), ('w2', 'c1'), ('w3', 'c2')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let cache = WaveCoveCache::new();
        cache.seed_from_db(&pool).await.unwrap();
        assert_eq!(cache.len(), 3);
        assert_eq!(cache.cove_of(&wid("w1")), Some(cid("c1")));
        assert_eq!(cache.cove_of(&wid("w2")), Some(cid("c1")));
        assert_eq!(cache.cove_of(&wid("w3")), Some(cid("c2")));
    }

    #[tokio::test]
    async fn seed_from_db_clears_before_populate() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::query("CREATE TABLE waves (id TEXT PRIMARY KEY, cove_id TEXT NOT NULL)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO waves (id, cove_id) VALUES ('only', 'cove-real')")
            .execute(&pool)
            .await
            .unwrap();

        let cache = WaveCoveCache::new();
        cache.insert(wid("stale"), cid("cove-stale"));
        cache.seed_from_db(&pool).await.unwrap();
        assert_eq!(cache.cove_of(&wid("stale")), None);
        assert_eq!(cache.cove_of(&wid("only")), Some(cid("cove-real")));
        assert_eq!(cache.len(), 1);
    }
}
