//! SQL authority for Dispatcher Planner inputs and reopen candidates. Cache roles
//! are discovery hints; every durable admission compares the captured tenure.
use super::*;
use crate::error::{CalmError, Result};
use sqlx::{Sqlite, Transaction};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PlannerAdmission {
    pub card: CardId,
    pub track: TrackId,
    pub worker: String,
    revision: i64,
    pub baseline: i64,
}

impl PlannerAdmission {
    /// Input compensation discovers current Planners on open Tracks independently
    /// of native harness recovery. A fresh canonical carrier has no thread yet.
    /// These are hints only: reconcile re-discovers admission under the push lock.
    pub(super) async fn reconcile_candidates(
        repo: &dyn crate::db::Repo,
    ) -> Result<Vec<(TrackId, CardId)>> {
        let db = repo
            .sqlite_db()
            .ok_or_else(|| CalmError::Internal("Planner admission requires SQLite".into()))?;
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT t.id,c.id FROM tracks t JOIN cards c ON c.track_id=t.id
             WHERE t.closed_at IS NULL AND c.role='planner' AND c.session_id IS NOT NULL
             ORDER BY t.id,c.id",
        )
        .fetch_all(db.read_pool())
        .await?;
        Ok(rows
            .into_iter()
            .map(|(track, card)| (track.into(), card.into()))
            .collect())
    }

    pub(crate) async fn discover(
        repo: &dyn crate::db::Repo,
        track: &TrackId,
    ) -> Result<Option<Self>> {
        let db = repo
            .sqlite_db()
            .ok_or_else(|| CalmError::Internal("Planner admission requires SQLite".into()))?;
        let row: Option<(String, String, i64, i64)> = sqlx::query_as(
            "SELECT c.id,c.session_id,t.planner_revision,t.planner_event_baseline
             FROM cards c JOIN tracks t ON t.id=c.track_id
             WHERE c.track_id=?1 AND c.role='planner' AND c.session_id IS NOT NULL",
        )
        .bind(track.as_str())
        .fetch_optional(db.read_pool())
        .await?;
        Ok(row.map(|(card, worker, revision, baseline)| Self {
            card: card.into(),
            track: track.clone(),
            worker,
            revision,
            baseline,
        }))
    }

    pub(super) async fn load_tx(tx: &mut Transaction<'_, Sqlite>, card: &str) -> Result<Self> {
        let row: Option<(String, String, i64, i64)> = sqlx::query_as(
            "SELECT c.track_id,c.session_id,t.planner_revision,t.planner_event_baseline
             FROM cards c JOIN tracks t ON t.id=c.track_id
             WHERE c.id=?1 AND c.role='planner' AND c.session_id IS NOT NULL",
        )
        .bind(card)
        .fetch_optional(&mut **tx)
        .await?;
        let (track, worker, revision, baseline) = row
            .ok_or_else(|| CalmError::Forbidden("Dispatcher target is no longer Planner".into()))?;
        Ok(Self {
            card: card.into(),
            track: track.into(),
            worker,
            revision,
            baseline,
        })
    }

    pub(super) async fn authorize_tx(&self, tx: &mut Transaction<'_, Sqlite>) -> Result<()> {
        if Self::load_tx(tx, self.card.as_str()).await? != *self {
            return Err(CalmError::Forbidden(
                "Dispatcher Planner tenure changed".into(),
            ));
        }
        Ok(())
    }

    pub(crate) async fn is_current(&self, repo: &dyn crate::db::Repo) -> Result<bool> {
        Ok(Self::discover(repo, &self.track).await?.as_ref() == Some(self))
    }
}
