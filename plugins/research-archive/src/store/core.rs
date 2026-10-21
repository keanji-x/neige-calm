//! Core completion is independent of the overall run's supplemental work.
use super::{Result, Store};
use crate::catalog::CaptureTier;
use crate::types::{RunId, UnixMs};

impl Store {
    pub async fn core_complete_at(&self, run: RunId) -> Result<Option<UnixMs>> {
        let at: Option<i64> =
            sqlx::query_scalar("SELECT finished_at FROM run_core_complete WHERE run_id=?")
                .bind(run.0)
                .fetch_optional(&self.pool)
                .await?;
        Ok(at.map(UnixMs))
    }

    /// Persist settlement after the runner applies its attempt cap. Store
    /// checks frozen membership, a row for every Core pair and the run clock.
    /// This never prevents later Supplemental observations or resume.
    pub async fn complete_core(&self, run: RunId, at: UnixMs) -> Result<()> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM run_core_complete WHERE run_id=?)")
                .bind(run.0)
                .fetch_one(&mut *tx)
                .await?;
        if !exists {
            let clock = super::write::require_open(&mut tx, run).await?;
            clock.inside_window(at)?;
            clock.not_before_mark(at)?;
            super::write::require_complete(&mut tx, run, Some(CaptureTier::Core)).await?;
            sqlx::query("INSERT INTO run_core_complete(run_id,finished_at) VALUES (?,?)")
                .bind(run.0)
                .bind(at.0)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }
}
