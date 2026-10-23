//! In-memory `CardId -> (CardRole, home TrackId)` cache used by `role_gate::enforce_role`, kept strictly
//! write-through with the `cards` table (inserted in the minting transaction before commit, re-seeded at boot).
//! A cache miss at decision time is treated as deny for `AiCodex` actors.

use crate::error::Result;
use crate::ids::{CardId, TrackId};
use crate::model::CardRole;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Per-card cache row: persisted `CardRole` + immutable home `TrackId`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CardCacheEntry {
    pub role: CardRole,
    pub track_id: TrackId,
}

/// Concurrent `CardId -> CardCacheEntry` map populated at boot from `cards` and maintained write-through by every
/// insert / delete path. `Clone` shares the map and its short synchronous publication barrier.
#[derive(Clone, Default)]
pub struct CardRoleCache(Arc<RwLock<HashMap<CardId, CardCacheEntry>>>);

impl CardRoleCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Look up the role for a card. `None` means the card's row was deleted (the only legitimate way to see it in production).
    pub fn get(&self, id: &CardId) -> Option<CardRole> {
        self.0.read().expect("card roles").get(id).map(|e| e.role)
    }

    /// The card's home track id, captured at mint and immutable; `enforce_role` cross-checks `scope.track` against it for Worker cards.
    pub fn track_of(&self, id: &CardId) -> Option<TrackId> {
        self.0
            .read()
            .expect("card roles")
            .get(id)
            .map(|e| e.track_id.clone())
    }

    /// Write-through insert, called after the SQL succeeds but before the transaction commits so a follow-up emit in the
    /// same closure sees the role. `track_id` is required on purpose. If the txn rolls back the stale entry lingers until
    /// the next `seed_from_db`; at worst the gate permits a write the DB rejects on its FK as `NotFound`.
    pub fn insert(&self, id: CardId, role: CardRole, track_id: TrackId) {
        self.0
            .write()
            .expect("card roles")
            .insert(id, CardCacheEntry { role, track_id });
    }

    /// Publish both committed roles under one barrier; readers never see a half pair.
    pub fn publish_pair(&self, old: (CardId, CardCacheEntry), new: (CardId, CardCacheEntry)) {
        let mut entries = self.0.write().expect("card roles");
        entries.insert(old.0, old.1);
        entries.insert(new.0, new.1);
    }

    /// One coherent generation for callers making a decision across multiple cards.
    pub fn roles_snapshot<'a>(
        &self,
        ids: impl IntoIterator<Item = &'a CardId>,
    ) -> HashMap<CardId, CardRole> {
        let entries = self.0.read().expect("card roles");
        ids.into_iter()
            .filter_map(|id| entries.get(id).map(|e| (id.clone(), e.role)))
            .collect()
    }

    /// Remove a card's role entry. Safe to call on a missing key.
    pub fn remove(&self, id: &CardId) {
        self.0.write().expect("card roles").remove(id);
    }

    pub fn len(&self) -> usize {
        self.0.read().expect("card roles").len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.read().expect("card roles").is_empty()
    }

    /// Boot-time seed from `cards`. Replace the complete map under the publication barrier
    /// so readers see no partial seed or stale entries. Production seeds once after migrations.
    pub async fn seed_from_db(&self, pool: &SqlitePool) -> Result<()> {
        // `CardRole` has no `sqlx::Type` derive (zero-IO rule); decode the TEXT column via `TryFrom<String>`.
        let rows: Vec<(String, String, String)> =
            sqlx::query_as(r#"SELECT id, role, track_id FROM cards"#)
                .fetch_all(pool)
                .await?;
        let mut entries = HashMap::new();
        for (id, role, track_id) in rows {
            let role = CardRole::try_from(role).map_err(|e| sqlx::Error::Decode(e.into()))?;
            entries.insert(
                CardId::from(id),
                CardCacheEntry {
                    role,
                    track_id: track_id.into(),
                },
            );
        }
        *self.0.write().expect("card roles") = entries;
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

    fn wid(s: &str) -> TrackId {
        TrackId::from(s)
    }

    #[test]
    fn planner_switch_pair_cache_publication() {
        let cache = CardRoleCache::new();
        let ids = [cid("old"), cid("new")];
        cache.insert(ids[0].clone(), CardRole::Planner, wid("track"));
        cache.insert(ids[1].clone(), CardRole::Assistant, wid("track"));
        let entry = |index: usize, role| {
            (
                ids[index].clone(),
                CardCacheEntry {
                    role,
                    track_id: wid("track"),
                },
            )
        };
        let start = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                start.wait();
                for _ in 0..2000 {
                    let roles = cache.roles_snapshot(&ids);
                    let pair = (roles[&ids[0]], roles[&ids[1]]);
                    assert!(
                        pair == (CardRole::Planner, CardRole::Assistant)
                            || pair == (CardRole::Assistant, CardRole::Planner),
                        "half publication: {pair:?}"
                    );
                }
            });
            start.wait();
            for _ in 0..1000 {
                for (old_role, new_role) in [
                    (CardRole::Assistant, CardRole::Planner),
                    (CardRole::Planner, CardRole::Assistant),
                ] {
                    cache.publish_pair(entry(0, old_role), entry(1, new_role));
                }
            }
        });
        cache.publish_pair(entry(0, CardRole::Assistant), entry(1, CardRole::Planner));
        let roles = cache.roles_snapshot(&ids);
        assert_eq!(
            (roles[&ids[0]], roles[&ids[1]]),
            (CardRole::Assistant, CardRole::Planner)
        );
        assert_eq!(cache.track_of(&ids[1]), Some(wid("track")));
    }

    #[test]
    fn insert_get_remove_round_trip() {
        let c = CardRoleCache::new();
        assert!(c.is_empty());
        c.insert(cid("a"), CardRole::Worker, wid("w1"));
        c.insert(cid("b"), CardRole::Planner, wid("w1"));
        c.insert(cid("c"), CardRole::Worker, wid("w2"));
        assert_eq!(c.len(), 3);

        assert_eq!(c.get(&cid("a")), Some(CardRole::Worker));
        assert_eq!(c.get(&cid("b")), Some(CardRole::Planner));
        assert_eq!(c.get(&cid("c")), Some(CardRole::Worker));
        assert_eq!(c.get(&cid("missing")), None);

        assert_eq!(c.track_of(&cid("a")), Some(wid("w1")));
        assert_eq!(c.track_of(&cid("c")), Some(wid("w2")));
        assert_eq!(c.track_of(&cid("missing")), None);

        c.remove(&cid("b"));
        assert_eq!(c.get(&cid("b")), None);
        assert_eq!(c.track_of(&cid("b")), None);
        assert_eq!(c.len(), 2);

        c.remove(&cid("missing"));
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn insert_overwrites_existing() {
        let c = CardRoleCache::new();
        c.insert(cid("a"), CardRole::Worker, wid("w1"));
        c.insert(cid("a"), CardRole::Planner, wid("w2"));
        assert_eq!(c.get(&cid("a")), Some(CardRole::Planner));
        assert_eq!(c.track_of(&cid("a")), Some(wid("w2")));
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn clone_shares_inner_state() {
        // `Clone` is `Arc::clone` — mutations on one handle are visible through the other.
        let a = CardRoleCache::new();
        let b = a.clone();
        a.insert(cid("x"), CardRole::Worker, wid("w-x"));
        assert_eq!(b.get(&cid("x")), Some(CardRole::Worker));
        assert_eq!(b.track_of(&cid("x")), Some(wid("w-x")));
    }

    #[tokio::test]
    async fn seed_from_db_loads_existing_rows() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        // Mini schema: just enough to satisfy `seed_from_db`'s query.
        sqlx::query(
            r#"CREATE TABLE cards (
                id TEXT PRIMARY KEY,
                track_id TEXT NOT NULL,
                role TEXT NOT NULL DEFAULT 'plain'
            )"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO cards (id, track_id, role) VALUES \
                ('a', 'w1', 'worker'), \
                ('b', 'w1', 'planner'), \
                ('c', 'w2', 'worker')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let cache = CardRoleCache::new();
        cache.seed_from_db(&pool).await.unwrap();
        assert_eq!(cache.len(), 3);
        assert_eq!(cache.get(&cid("a")), Some(CardRole::Worker));
        assert_eq!(cache.get(&cid("b")), Some(CardRole::Planner));
        assert_eq!(cache.get(&cid("c")), Some(CardRole::Worker));
        assert_eq!(cache.track_of(&cid("a")), Some(wid("w1")));
        assert_eq!(cache.track_of(&cid("b")), Some(wid("w1")));
        assert_eq!(cache.track_of(&cid("c")), Some(wid("w2")));
    }

    #[tokio::test]
    async fn seed_from_db_clears_before_populate() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::query(
            "CREATE TABLE cards (id TEXT PRIMARY KEY, track_id TEXT NOT NULL, role TEXT NOT NULL)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO cards (id, track_id, role) VALUES ('only', 'w-only', 'planner')")
            .execute(&pool)
            .await
            .unwrap();

        let cache = CardRoleCache::new();
        cache.insert(cid("stale"), CardRole::Worker, wid("w-stale"));
        cache.seed_from_db(&pool).await.unwrap();
        assert_eq!(cache.get(&cid("stale")), None);
        assert_eq!(cache.get(&cid("only")), Some(CardRole::Planner));
        assert_eq!(cache.track_of(&cid("only")), Some(wid("w-only")));
        assert_eq!(cache.len(), 1);
    }
}
