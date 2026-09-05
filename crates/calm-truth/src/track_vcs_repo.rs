use crate::db::sqlite::begin_immediate_tx;
use crate::error::Result;
use crate::ids::TrackId;
use crate::track_vcs::{self, CommitHash, CommitLog, CommitRecord, FileDiff, HistoricalBlob};
use async_trait::async_trait;
use sqlx::SqlitePool;
use std::sync::Arc;

#[async_trait]
pub trait TrackVcsRepo: Send + Sync + 'static {
    async fn head(&self, track_id: &TrackId) -> Result<Option<CommitHash>>;

    async fn diff_with_patches(
        &self,
        from: &str,
        to: &str,
        path: Option<&str>,
        max_patch_lines: usize,
    ) -> Result<Vec<FileDiff>>;

    async fn cat_at(&self, commit_hash: &str, path: &str) -> Result<HistoricalBlob>;

    async fn log(
        &self,
        track_id: &TrackId,
        path: Option<&str>,
        limit: usize,
        include_empty: bool,
    ) -> Result<CommitLog>;

    async fn commit_record(&self, commit_hash: &str) -> Result<Option<CommitRecord>>;

    async fn resolve_commit_prefix(
        &self,
        track_id: &TrackId,
        prefix: &str,
    ) -> Result<Option<CommitRecord>>;

    async fn prune_track_history(
        &self,
        track_id: &TrackId,
        keep: usize,
        dry_run: bool,
    ) -> Result<u64>;

    async fn sweep_unreferenced_objects(&self) -> Result<u64>;

    async fn vacuum(&self) -> Result<()>;
}

#[derive(Clone)]
pub struct SqlxTrackVcsRepo {
    pool: SqlitePool,
}

impl SqlxTrackVcsRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub fn shared(pool: SqlitePool) -> Arc<dyn TrackVcsRepo> {
        Arc::new(Self::new(pool))
    }
}

#[async_trait]
impl TrackVcsRepo for SqlxTrackVcsRepo {
    async fn head(&self, track_id: &TrackId) -> Result<Option<CommitHash>> {
        track_vcs::head(&self.pool, track_id).await
    }

    async fn diff_with_patches(
        &self,
        from: &str,
        to: &str,
        path: Option<&str>,
        max_patch_lines: usize,
    ) -> Result<Vec<FileDiff>> {
        track_vcs::diff_with_patches(&self.pool, from, to, path, max_patch_lines).await
    }

    async fn cat_at(&self, commit_hash: &str, path: &str) -> Result<HistoricalBlob> {
        track_vcs::cat_at(&self.pool, commit_hash, path).await
    }

    async fn log(
        &self,
        track_id: &TrackId,
        path: Option<&str>,
        limit: usize,
        include_empty: bool,
    ) -> Result<CommitLog> {
        track_vcs::log(&self.pool, track_id, path, limit, include_empty).await
    }

    async fn commit_record(&self, commit_hash: &str) -> Result<Option<CommitRecord>> {
        track_vcs::commit_record(&self.pool, commit_hash).await
    }

    async fn resolve_commit_prefix(
        &self,
        track_id: &TrackId,
        prefix: &str,
    ) -> Result<Option<CommitRecord>> {
        track_vcs::resolve_commit_prefix(&self.pool, track_id, prefix).await
    }

    async fn prune_track_history(
        &self,
        track_id: &TrackId,
        keep: usize,
        dry_run: bool,
    ) -> Result<u64> {
        let mut tx = begin_immediate_tx(&self.pool).await?;
        let pruned = track_vcs::prune_track_history_tx(&mut tx, track_id, keep).await?;
        if dry_run {
            tx.rollback().await?;
        } else {
            tx.commit().await?;
        }
        Ok(pruned)
    }

    async fn sweep_unreferenced_objects(&self) -> Result<u64> {
        track_vcs::sweep_unreferenced_objects_once(&self.pool).await
    }

    async fn vacuum(&self) -> Result<()> {
        sqlx::query("VACUUM").execute(&self.pool).await?;
        Ok(())
    }
}
