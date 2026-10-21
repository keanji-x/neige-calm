//! Owner-local precommit proof adapter. Acquire/recheck the native/harness park
//! before BEGIN; retain it, MCP and caller lifecycle/dispatch fences through
//! commit and registry publication. No owner await runs inside SQL.
use calm_truth::db::sqlite::conversation::{CarrierReplacementGuard, CurrentInputCarrier};
use sqlx::{Sqlite, Transaction};

pub struct CarrierReplacementOwner<'a> {
    pub idle: &'a super::run_loop::hibernation::IdleCarrierOwner<'a>,
    pub calls: &'a crate::mcp_server::transport::carrier_quiescence::ToolCallGuard,
    pub dispatcher: &'a crate::dispatcher::Dispatcher,
    pub track: crate::ids::TrackId,
    /// Caller checks eligibility before SQL under push admission, retaining all
    /// observed tail facts (including ignored events) for exact comparison.
    pub arrivals: &'a [calm_truth::db::sqlite::event_inputs::EventInputFact],
}

#[async_trait::async_trait]
impl CarrierReplacementGuard for CarrierReplacementOwner<'_> {
    async fn recheck_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        carrier: &CurrentInputCarrier,
    ) -> calm_truth::error::Result<()> {
        use calm_truth::error::CalmError;
        if !self
            .idle
            .matches_current(carrier.card_id(), carrier.worker_session_id())
            || self.calls.worker() != carrier.worker_session_id()
            || self.calls.require_idle().is_err()
        {
            return Err(CalmError::Conflict("protected carrier owner changed"));
        }
        let track: String = sqlx::query_scalar("SELECT track_id FROM cards WHERE id=?1")
            .bind(carrier.card_id())
            .fetch_one(&mut **tx)
            .await?;
        if track != self.track.as_str() {
            return Err(CalmError::Conflict("protected Track changed"));
        }
        let _settled = crate::operation::planner_start_fence::CardStartFence::require_settled_tx(
            tx,
            carrier.card_id(),
        )
        .await
        .map_err(|e| CalmError::Conflict(e.to_string()))?;
        crate::ask::require_no_open_carrier_asks_tx(tx, carrier.card_id())
            .await
            .map_err(|e| CalmError::Conflict(e.to_string()))?;
        let after = carrier.snapshot()["push_watermark"]
            .as_i64()
            .ok_or(CalmError::Conflict("carrier watermark unavailable"))?;
        if self
            .dispatcher
            .carrier_quiescence_prefix_tx(tx, &self.track, after)
            .await
            .map_err(|e| CalmError::Conflict(e.to_string()))?
            .iter()
            .map(|fact| &fact.fingerprint)
            .ne(self.arrivals.iter().map(|fact| &fact.fingerprint))
        {
            return Err(CalmError::Conflict("carrier event arrivals changed"));
        }
        Ok(())
    }
}
