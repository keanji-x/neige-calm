use crate::db::sqlite::begin_immediate_tx;
use crate::error::Result;
use crate::ids::WaveId;
use crate::wave_vcs::{self, CommitHash, CommitLog, CommitRecord, FileDiff, HistoricalBlob};
use async_trait::async_trait;
use sqlx::SqlitePool;
use std::sync::Arc;

#[async_trait]
pub trait WaveVcsRepo: Send + Sync + 'static {
    async fn head(&self, wave_id: &WaveId) -> Result<Option<CommitHash>>;

    async fn diff_with_patches(
        &self,
        from: &str,
        to: &str,
        path: Option<&str>,
        max_patch_lines: usize,
    ) -> Result<Vec<FileDiff>>;

    async fn cat_at(&self, commit_hash: &str, path: &str) -> Result<HistoricalBlob>;

    async fn log(&self, wave_id: &WaveId, path: Option<&str>, limit: usize) -> Result<CommitLog>;

    async fn commit_record(&self, commit_hash: &str) -> Result<Option<CommitRecord>>;

    async fn prune_wave_history(&self, wave_id: &WaveId, keep: usize, dry_run: bool)
    -> Result<u64>;

    async fn sweep_unreferenced_objects(&self) -> Result<u64>;

    async fn vacuum(&self) -> Result<()>;
}

#[derive(Clone)]
pub struct SqlxWaveVcsRepo {
    pool: SqlitePool,
}

impl SqlxWaveVcsRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub fn shared(pool: SqlitePool) -> Arc<dyn WaveVcsRepo> {
        Arc::new(Self::new(pool))
    }
}

#[async_trait]
impl WaveVcsRepo for SqlxWaveVcsRepo {
    async fn head(&self, wave_id: &WaveId) -> Result<Option<CommitHash>> {
        wave_vcs::head(&self.pool, wave_id).await
    }

    async fn diff_with_patches(
        &self,
        from: &str,
        to: &str,
        path: Option<&str>,
        max_patch_lines: usize,
    ) -> Result<Vec<FileDiff>> {
        wave_vcs::diff_with_patches(&self.pool, from, to, path, max_patch_lines).await
    }

    async fn cat_at(&self, commit_hash: &str, path: &str) -> Result<HistoricalBlob> {
        wave_vcs::cat_at(&self.pool, commit_hash, path).await
    }

    async fn log(&self, wave_id: &WaveId, path: Option<&str>, limit: usize) -> Result<CommitLog> {
        wave_vcs::log(&self.pool, wave_id, path, limit).await
    }

    async fn commit_record(&self, commit_hash: &str) -> Result<Option<CommitRecord>> {
        wave_vcs::commit_record(&self.pool, commit_hash).await
    }

    async fn prune_wave_history(
        &self,
        wave_id: &WaveId,
        keep: usize,
        dry_run: bool,
    ) -> Result<u64> {
        let mut tx = begin_immediate_tx(&self.pool).await?;
        let pruned = wave_vcs::prune_wave_history_tx(&mut tx, wave_id, keep).await?;
        if dry_run {
            tx.rollback().await?;
        } else {
            tx.commit().await?;
        }
        Ok(pruned)
    }

    async fn sweep_unreferenced_objects(&self) -> Result<u64> {
        wave_vcs::sweep_unreferenced_objects_once(&self.pool).await
    }

    async fn vacuum(&self) -> Result<()> {
        sqlx::query("VACUUM").execute(&self.pool).await?;
        Ok(())
    }
}
