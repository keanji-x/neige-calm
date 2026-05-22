//! In-memory `CardId -> CardRole` cache used by `role_gate::enforce_role`.
//!
//! ## Why a cache
//!
//! The role gate runs at every audited write — see
//! `db::sqlite::SqlxRepo::write_with_event` and `log_pure_event`. Looking
//! up `cards.role` from sqlite on the hot path would block every emit on
//! the connection pool *inside* the transaction the gate is meant to
//! protect. A small in-process map keyed by `CardId` is fast (DashMap
//! shards lock per-key), and the source of truth — the `cards` table —
//! is updated in the same transaction that mints / mutates the card, so
//! we keep the cache strictly write-through:
//!
//!   * `card_create_with_id_tx` calls `cache.insert(card_id, role)`
//!     right after the SQL insert succeeds, *before* the transaction
//!     commits. A subsequent emit inside the same `write_with_event`
//!     closure therefore sees the freshly-minted role without waiting
//!     for the commit.
//!   * `seed_from_db` repopulates the cache at boot from the `cards`
//!     table. Crash safety: any restart sees the persisted role and
//!     reconstitutes the cache before the first write lands.
//!   * `remove` is called from the card-delete path so the role doesn't
//!     linger past the row's lifetime.
//!
//! ## What the cache is *not*
//!
//! It is **not** an authorization decision by itself. The decision lives
//! in `role_gate::enforce_role` — the cache is a read-side optimization
//! and a same-tx propagation mechanism. A cache miss at decision time is
//! treated as **deny** by `enforce_role` for `AiCodex` actors (defense
//! in depth — a race between card-delete and an in-flight emit means
//! the writer is referencing a card that no longer exists, and we'd
//! rather drop the write than admit a sketchy one).

use crate::error::Result;
use crate::ids::CardId;
use crate::model::CardRole;
use dashmap::DashMap;
use sqlx::SqlitePool;
use std::sync::Arc;

/// Concurrent `CardId -> CardRole` map populated at boot from the `cards`
/// table and maintained write-through by every insert / delete path.
///
/// `Clone` is cheap — the inner `Arc<DashMap<...>>` shares state, so
/// stashing one copy on `AppState::card_role_cache` and another inside
/// the FSM / sweeper task closures costs nothing beyond the `Arc` clone.
#[derive(Clone, Default)]
pub struct CardRoleCache(Arc<DashMap<CardId, CardRole>>);

impl CardRoleCache {
    /// Empty cache. Same as `Default::default()` — explicit constructor
    /// because the test scaffolding reads slightly cleaner with `new()`
    /// at the call site.
    pub fn new() -> Self {
        Self::default()
    }

    /// Look up the role for a card. `None` means "no entry" — that's
    /// either a card that hasn't been inserted yet (impossible under the
    /// write-through invariant) or a card whose row was deleted (the
    /// only legitimate way to see this in production).
    pub fn get(&self, id: &CardId) -> Option<CardRole> {
        self.0.get(id).map(|r| *r.value())
    }

    /// Write-through insert. Called from `card_create_with_id_tx` after
    /// the SQL succeeds but before the surrounding transaction commits;
    /// the same-tx visibility lets a follow-up emit inside the same
    /// `write_with_event` closure see the freshly-minted role.
    ///
    /// If the txn rolls back the cache will hold a stale entry until
    /// `seed_from_db` (next boot) overwrites it — see
    /// `seed_from_db`'s `clear-then-populate` semantics. Tolerating
    /// stale entries on the failed-write path is the price for
    /// commit-then-emit ordering staying simple; the consequence is at
    /// worst an `enforce_role` that *permits* a write the DB would have
    /// rejected on its FK (the card row no longer exists), which the
    /// transactional layer surfaces as `NotFound` anyway.
    pub fn insert(&self, id: CardId, role: CardRole) {
        self.0.insert(id, role);
    }

    /// Remove a card's role entry. Called from the card-delete path so
    /// the cache shrinks with the table. Safe to call on a missing key.
    pub fn remove(&self, id: &CardId) {
        self.0.remove(id);
    }

    /// Number of entries. Convenience for unit tests + future telemetry;
    /// production code rarely needs the size.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// `true` when no cards have been seeded yet. Mirrors `Vec::is_empty`
    /// — clippy nags if you ship `len()` without it.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Boot-time seed: read every `(id, role)` from `cards` into the
    /// map. Clears the existing contents first so a re-seed during a
    /// long-lived test process (e.g. `AppState::from_parts` with a fresh
    /// pool) doesn't carry stale entries from a previous fixture.
    ///
    /// Production callers run this exactly once from `AppState::new`
    /// after migrations finish — see `state.rs`. Missing cards (e.g.
    /// table doesn't exist yet because migrations haven't run) surface
    /// as a sqlx error and abort boot — the caller already runs
    /// migrations first.
    pub async fn seed_from_db(&self, pool: &SqlitePool) -> Result<()> {
        self.0.clear();
        let rows: Vec<(String, CardRole)> = sqlx::query_as(r#"SELECT id, role FROM cards"#)
            .fetch_all(pool)
            .await?;
        for (id, role) in rows {
            self.0.insert(CardId::from(id), role);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::SqlitePool;

    fn cid(s: &str) -> CardId {
        CardId::from(s)
    }

    #[test]
    fn insert_get_remove_round_trip() {
        let c = CardRoleCache::new();
        assert!(c.is_empty());
        c.insert(cid("a"), CardRole::Plain);
        c.insert(cid("b"), CardRole::Spec);
        c.insert(cid("c"), CardRole::Worker);
        assert_eq!(c.len(), 3);

        assert_eq!(c.get(&cid("a")), Some(CardRole::Plain));
        assert_eq!(c.get(&cid("b")), Some(CardRole::Spec));
        assert_eq!(c.get(&cid("c")), Some(CardRole::Worker));
        assert_eq!(c.get(&cid("missing")), None);

        c.remove(&cid("b"));
        assert_eq!(c.get(&cid("b")), None);
        assert_eq!(c.len(), 2);

        // Removing a missing key is a no-op.
        c.remove(&cid("missing"));
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn insert_overwrites_existing() {
        let c = CardRoleCache::new();
        c.insert(cid("a"), CardRole::Plain);
        c.insert(cid("a"), CardRole::Spec);
        assert_eq!(c.get(&cid("a")), Some(CardRole::Spec));
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn clone_shares_inner_state() {
        // `Clone` is `Arc::clone` — mutations on one handle are visible
        // through the other. Production code relies on this when the
        // cache is stashed on `AppState` and the sweeper / FSM tasks
        // pull a clone for their own closures.
        let a = CardRoleCache::new();
        let b = a.clone();
        a.insert(cid("x"), CardRole::Worker);
        assert_eq!(b.get(&cid("x")), Some(CardRole::Worker));
    }

    #[tokio::test]
    async fn seed_from_db_loads_existing_rows() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        // Mini schema: just enough to satisfy `seed_from_db`'s query.
        sqlx::query(
            r#"CREATE TABLE cards (
                id TEXT PRIMARY KEY,
                role TEXT NOT NULL DEFAULT 'plain'
            )"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO cards (id, role) VALUES ('a', 'plain'), ('b', 'spec'), ('c', 'worker')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let cache = CardRoleCache::new();
        cache.seed_from_db(&pool).await.unwrap();
        assert_eq!(cache.len(), 3);
        assert_eq!(cache.get(&cid("a")), Some(CardRole::Plain));
        assert_eq!(cache.get(&cid("b")), Some(CardRole::Spec));
        assert_eq!(cache.get(&cid("c")), Some(CardRole::Worker));
    }

    #[tokio::test]
    async fn seed_from_db_clears_before_populate() {
        // Re-seeding a cache that already has stale entries should drop
        // them — protects long-lived test processes that swap pools.
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::query("CREATE TABLE cards (id TEXT PRIMARY KEY, role TEXT NOT NULL)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO cards (id, role) VALUES ('only', 'spec')")
            .execute(&pool)
            .await
            .unwrap();

        let cache = CardRoleCache::new();
        cache.insert(cid("stale"), CardRole::Worker);
        cache.seed_from_db(&pool).await.unwrap();
        assert_eq!(cache.get(&cid("stale")), None);
        assert_eq!(cache.get(&cid("only")), Some(CardRole::Spec));
        assert_eq!(cache.len(), 1);
    }
}
