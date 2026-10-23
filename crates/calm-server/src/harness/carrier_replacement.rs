//! Owner-local precommit proof adapter. Acquire/recheck the native/harness park
//! before BEGIN; retain it, MCP and caller lifecycle/dispatch fences through
//! commit and registry publication. No owner await runs inside SQL.
pub use super::run_loop::hibernation::{OwnerProofError, OwnerRefusal};
use calm_truth::db::sqlite::conversation::{CarrierReplacementGuard, CurrentInputCarrier};
use calm_truth::error::CalmError as TruthError;
use sqlx::{Sqlite, Transaction};

#[derive(Debug)]
pub enum CarrierOwnerError {
    Refused {
        reason: OwnerRefusal,
        error: TruthError,
    },
    Failed(TruthError),
    PredicateFailed(crate::error::CalmError),
}
impl CarrierOwnerError {
    fn refused(reason: OwnerRefusal, message: &str) -> Self {
        Self::Refused {
            reason,
            error: TruthError::Conflict(message),
        }
    }
    fn predicate(reason: OwnerRefusal, error: crate::error::CalmError) -> Self {
        if matches!(error, crate::error::CalmError::Conflict(_)) {
            Self::refused(reason, &error.to_string())
        } else {
            Self::PredicateFailed(error)
        }
    }
    fn into_legacy(self) -> TruthError {
        match self {
            Self::Refused { error, .. } | Self::Failed(error) => error,
            Self::PredicateFailed(error) => TruthError::Conflict(error.to_string()),
        }
    }
}
impl From<sqlx::Error> for CarrierOwnerError {
    fn from(error: sqlx::Error) -> Self {
        Self::Failed(error.into())
    }
}

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
        self.recheck_typed_tx(tx, carrier)
            .await
            .map_err(CarrierOwnerError::into_legacy)
    }
}
impl CarrierReplacementOwner<'_> {
    pub async fn recheck_typed_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        carrier: &CurrentInputCarrier,
    ) -> std::result::Result<(), CarrierOwnerError> {
        if let Err(reason) = self
            .idle
            .check_current(carrier.card_id(), carrier.worker_session_id())
        {
            return Err(CarrierOwnerError::refused(
                reason,
                "protected carrier owner changed",
            ));
        }
        if self.calls.worker() != carrier.worker_session_id() {
            return Err(CarrierOwnerError::refused(
                OwnerRefusal::Changed,
                "protected carrier owner changed",
            ));
        }
        if self.calls.require_idle().is_err() {
            return Err(CarrierOwnerError::refused(
                OwnerRefusal::PendingSubmission,
                "protected carrier owner changed",
            ));
        }
        let track: String = sqlx::query_scalar("SELECT track_id FROM cards WHERE id=?1")
            .bind(carrier.card_id())
            .fetch_one(&mut **tx)
            .await?;
        if track != self.track.as_str() {
            return Err(CarrierOwnerError::refused(
                OwnerRefusal::Changed,
                "protected Track changed",
            ));
        }
        let _settled = crate::operation::planner_start_fence::CardStartFence::require_settled_tx(
            tx,
            carrier.card_id(),
        )
        .await
        .map_err(|e| CarrierOwnerError::predicate(OwnerRefusal::TurnInFlight, e))?;
        crate::ask::require_no_open_carrier_asks_tx(tx, carrier.card_id())
            .await
            .map_err(|e| CarrierOwnerError::predicate(OwnerRefusal::OpenAsk, e))?;
        let after = carrier.snapshot()["push_watermark"]
            .as_i64()
            .ok_or_else(|| {
                CarrierOwnerError::refused(
                    OwnerRefusal::SessionUnavailable,
                    "carrier watermark unavailable",
                )
            })?;
        if self
            .dispatcher
            .carrier_quiescence_prefix_tx(tx, &self.track, after)
            .await
            .map_err(CarrierOwnerError::PredicateFailed)?
            .iter()
            .map(|fact| &fact.fingerprint)
            .ne(self.arrivals.iter().map(|fact| &fact.fingerprint))
        {
            return Err(CarrierOwnerError::refused(
                OwnerRefusal::PendingObservations,
                "carrier event arrivals changed",
            ));
        }
        Ok(())
    }
}
