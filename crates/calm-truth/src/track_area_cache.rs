//! In-memory `TrackId -> AreaId` cache used by `role_gate::enforce_role`.
//! Write-through: inserted before the surrounding transaction commits so a
//! same-tx emit sees the binding; a miss is treated as deny by the gate.

use crate::error::Result;
use crate::ids::{AreaId, TrackId};
use dashmap::DashMap;
use sqlx::SqlitePool;
use std::sync::Arc;

/// Concurrent `TrackId -> AreaId` map seeded at boot and maintained
/// write-through. `Clone` shares the inner `Arc<DashMap>`.
#[derive(Clone, Default)]
pub struct TrackAreaCache(Arc<DashMap<TrackId, AreaId>>);

impl TrackAreaCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// `None` means a track whose row was deleted (the only legitimate miss in production).
    pub fn area_of(&self, id: &TrackId) -> Option<AreaId> {
        self.0.get(id).map(|c| c.value().clone())
    }

    /// Write-through insert, before the surrounding transaction commits. A
    /// rollback leaves a stale entry until the next `seed_from_db`; at worst the
    /// gate permits a write the DB rejects on its FK anyway.
    pub fn insert(&self, track: TrackId, area: AreaId) {
        self.0.insert(track, area);
    }

    /// Safe to call on a missing key.
    pub fn remove(&self, track: &TrackId) {
        self.0.remove(track);
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Boot-time seed from `tracks`. Clears first so a re-seed in a long-lived
    /// test process doesn't carry stale entries from a previous fixture.
    pub async fn seed_from_db(&self, pool: &SqlitePool) -> Result<()> {
        self.0.clear();
        let rows: Vec<(String, String)> = sqlx::query_as(r#"SELECT id, area_id FROM tracks"#)
            .fetch_all(pool)
            .await?;
        for (track_id, area_id) in rows {
            self.0
                .insert(TrackId::from(track_id), AreaId::from(area_id));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::SqlitePool;

    fn wid(s: &str) -> TrackId {
        TrackId::from(s)
    }

    fn cid(s: &str) -> AreaId {
        AreaId::from(s)
    }

    #[test]
    fn insert_get_remove_round_trip() {
        let c = TrackAreaCache::new();
        assert!(c.is_empty());
        c.insert(wid("w1"), cid("c1"));
        c.insert(wid("w2"), cid("c1"));
        c.insert(wid("w3"), cid("c2"));
        assert_eq!(c.len(), 3);

        assert_eq!(c.area_of(&wid("w1")), Some(cid("c1")));
        assert_eq!(c.area_of(&wid("w2")), Some(cid("c1")));
        assert_eq!(c.area_of(&wid("w3")), Some(cid("c2")));
        assert_eq!(c.area_of(&wid("missing")), None);

        c.remove(&wid("w2"));
        assert_eq!(c.area_of(&wid("w2")), None);
        assert_eq!(c.len(), 2);

        c.remove(&wid("missing"));
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn insert_overwrites_existing() {
        let c = TrackAreaCache::new();
        c.insert(wid("w1"), cid("c1"));
        c.insert(wid("w1"), cid("c2"));
        assert_eq!(c.area_of(&wid("w1")), Some(cid("c2")));
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn clone_shares_inner_state() {
        let a = TrackAreaCache::new();
        let b = a.clone();
        a.insert(wid("x"), cid("y"));
        assert_eq!(b.area_of(&wid("x")), Some(cid("y")));
    }

    #[tokio::test]
    async fn seed_from_db_loads_existing_rows() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::query(
            r#"CREATE TABLE tracks (
                id TEXT PRIMARY KEY,
                area_id TEXT NOT NULL
            )"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO tracks (id, area_id) VALUES \
             ('w1', 'c1'), ('w2', 'c1'), ('w3', 'c2')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let cache = TrackAreaCache::new();
        cache.seed_from_db(&pool).await.unwrap();
        assert_eq!(cache.len(), 3);
        assert_eq!(cache.area_of(&wid("w1")), Some(cid("c1")));
        assert_eq!(cache.area_of(&wid("w2")), Some(cid("c1")));
        assert_eq!(cache.area_of(&wid("w3")), Some(cid("c2")));
    }

    #[tokio::test]
    async fn seed_from_db_clears_before_populate() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::query("CREATE TABLE tracks (id TEXT PRIMARY KEY, area_id TEXT NOT NULL)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO tracks (id, area_id) VALUES ('only', 'area-real')")
            .execute(&pool)
            .await
            .unwrap();

        let cache = TrackAreaCache::new();
        cache.insert(wid("stale"), cid("area-stale"));
        cache.seed_from_db(&pool).await.unwrap();
        assert_eq!(cache.area_of(&wid("stale")), None);
        assert_eq!(cache.area_of(&wid("only")), Some(cid("area-real")));
        assert_eq!(cache.len(), 1);
    }
}
