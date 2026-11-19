//! Identity of a committed Close obligation; no source or permission to issue work.
use super::*;
use crate::session_projection_repo::WorkerSessionProjection;

#[derive(Clone, sqlx::FromRow)]
pub(crate) struct PlannerCleanup {
    pub worker_session_id: String,
    pub card_id: String,
    pub track_id: String,
    pub requested_at_ms: i64,
}

impl PlannerCleanup {
    pub(crate) async fn pending_runtime_tx(
        &self,
        tx: &mut Tx<'_>,
    ) -> Result<Option<WorkerSessionProjection>> {
        let pending: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM track_close_cleanups WHERE worker_session_id=?1 \
                 AND card_id=?2 AND track_id=?3 AND requested_at_ms=?4)",
        )
        .bind(&self.worker_session_id)
        .bind(&self.card_id)
        .bind(&self.track_id)
        .bind(self.requested_at_ms)
        .fetch_one(&mut **tx)
        .await?;
        if !pending {
            return Ok(None);
        }
        let runtime = crate::db::sqlite::session_projection_by_id_tx(tx, &self.worker_session_id)
            .await?
            .ok_or_else(|| CalmError::Conflict("closed Planner binding unavailable".into()))?;
        let session = crate::db::sqlite::session_get_tx(tx, &self.worker_session_id.clone().into())
            .await?
            .ok_or_else(|| CalmError::Conflict("closed Planner binding unavailable".into()))?;
        if runtime.card_id != self.card_id
            || session.track_id.as_str() != self.track_id
            || session.card_id.as_ref().map(|id| id.as_str()) != Some(self.card_id.as_str())
            || session.contract != calm_types::worker::WorkerContract::Planner
        {
            return Err(CalmError::Conflict("closed Planner binding changed".into()));
        }
        crate::harness::backend::factory::PlannerFactory::require_closed_planner_binding_tx(
            tx, &runtime,
        )
        .await?;
        Ok(Some(runtime))
    }
}

#[async_trait::async_trait]
impl crate::harness::backend::factory::ActiveRecoveryAdmission for PlannerCleanup {
    fn check_current(&self) -> Result<()> {
        Ok(())
    }

    async fn validate_tx(&self, tx: &mut Tx<'_>) -> Result<()> {
        self.pending_runtime_tx(tx)
            .await?
            .ok_or_else(|| CalmError::Conflict("closed Planner request changed".into()))?;
        Ok(())
    }

    async fn accepted_native_end_tx(
        &self,
        tx: &mut Tx<'_>,
    ) -> Result<Option<crate::harness::input_dispatch::end_transition::AcceptedRecovery>> {
        self.validate_tx(tx).await?;
        if !crate::harness::input_dispatch::end_transition::closed_execution_needs_end(
            &mut **tx,
            &self.card_id,
            &self.worker_session_id,
        )
        .await?
        {
            return Ok(None);
        }
        crate::harness::input_dispatch::end_transition::accepted_native_fact_tx(
            tx,
            &self.card_id,
            &self.worker_session_id,
        )
        .await
    }
}
