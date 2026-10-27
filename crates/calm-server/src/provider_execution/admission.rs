//! Caller-owned policy and intent construction share the admission writer transaction.
use super::*;

#[async_trait]
pub trait Admission: Send + Sync {
    async fn authorize_tx(&self, tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>) -> Result<()>;
    async fn intent_tx(
        self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    ) -> Result<ExecutionIntent>;
}

/// Planner preparation is process-free and already owns its ephemeral payload.
/// It imposes no additional admission policy.
pub struct PreparedIntent(pub ExecutionIntent);
#[async_trait]
impl Admission for PreparedIntent {
    async fn authorize_tx(&self, _: &mut sqlx::Transaction<'_, sqlx::Sqlite>) -> Result<()> {
        Ok(())
    }
    async fn intent_tx(
        self,
        _: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    ) -> Result<ExecutionIntent> {
        Ok(self.0)
    }
}

pub(crate) use crate::harness::run_loop::issuance::TrackAdmissionFence;

/// A take-once transfer from the run loop to the occupancy writer.
pub(crate) struct TrackFenceHandoff(std::sync::Mutex<Option<TrackAdmissionFence>>);
impl TrackFenceHandoff {
    pub(crate) fn new(fence: TrackAdmissionFence) -> Self {
        Self(std::sync::Mutex::new(Some(fence)))
    }
    pub(crate) fn release(&self) {
        self.0.lock().expect("Track fence handoff").take();
    }
}

pub(crate) enum OccupancyFence<'a> {
    ReleaseOnCommit(&'a TrackFenceHandoff),
    Held(&'a TrackAdmissionFence),
}
/// Admission consumes a handoff once inside the writer transaction. Keeping the
/// guard local also covers cancellation and every error after it is consumed.
pub(super) enum AdmittedFence<'a> {
    Owned(TrackAdmissionFence),
    Borrowed(&'a TrackAdmissionFence),
}
impl<'a> OccupancyFence<'a> {
    pub(super) fn claim(&self) -> Result<AdmittedFence<'a>> {
        match self {
            Self::Held(fence) => Ok(AdmittedFence::Borrowed(fence)),
            Self::ReleaseOnCommit(handoff) => handoff
                .0
                .lock()
                .expect("Track fence handoff")
                .take()
                .map(AdmittedFence::Owned)
                .ok_or_else(|| conflict("Planner admission Track fence is empty")),
        }
    }
}
impl AdmittedFence<'_> {
    pub(super) fn check(&self, track: &workspace_runtime::execution::TrackId) -> Result<()> {
        let fence = match self {
            Self::Owned(fence) => fence,
            Self::Borrowed(fence) => fence,
        };
        if fence.track().as_str() == track.0 {
            Ok(())
        } else {
            Err(conflict("Planner admission Track fence changed"))
        }
    }
}
impl Drop for OccupancyFence<'_> {
    fn drop(&mut self) {
        if let Self::ReleaseOnCommit(handoff) = self {
            handoff.release();
        }
    }
}

/// An admission written into the caller's writer transaction and not yet committed.
/// [`Admitted::commit`] is the only path to a drive. Dropping it instead (the caller's
/// transaction then rolls back) releases the driver registration and the Track fence.
#[must_use = "commit the admitting transaction, or drop to abandon it without a drive"]
pub(crate) struct Admitted<'a> {
    session: String,
    claim: Claim,
    guard: DriverGuard,
    ticket: PendingDriveTicket,
    engine: ExecutionEngine,
    held_fence: AdmittedFence<'a>,
    fence: OccupancyFence<'a>,
}

impl ProviderExecution {
    /// ① Admission core inside the caller's writer transaction. Policy and intent run
    /// under the Track fence; driver registration precedes the claim and COMMIT, so RAII
    /// covers every later failure and cancellation. On error the caller rolls back.
    pub(crate) async fn admit_tx<'a, A: Admission>(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        engine: &ExecutionEngine,
        session: &str,
        admission: A,
        purpose: Purpose,
        fence: OccupancyFence<'a>,
    ) -> Result<Admitted<'a>> {
        let engine = super::engine(Some(engine))?;
        let held_fence = fence.claim()?;
        admission.authorize_tx(tx).await?;
        let intent = admission.intent_tx(tx).await?;
        self.owner
            .alias_prefix()
            .validate(self.owner.as_ref(), session, &intent)?;
        held_fence.check(&intent.metadata.track)?;
        let id = intent.metadata.id.clone();
        let guard = self.drivers.claim(id.clone());
        let sql::Cas::Applied(claim) = sql::admit(
            tx,
            session,
            id.as_str().into(),
            purpose,
            sql::InitialRelease::Held,
        )
        .await?
        else {
            return Err(conflict("admission lost"));
        };
        let ticket = engine.admit(tx, intent).await.map_err(conflict)?;
        Ok(Admitted {
            session: session.into(),
            claim,
            guard,
            ticket,
            engine: engine.clone(),
            held_fence,
            fence,
        })
    }
}

impl Admitted<'_> {
    /// Commit the transaction that [`ProviderExecution::admit_tx`] wrote, then release
    /// the Track fence, then drive. A transaction without this claim is refused uncommitted.
    pub(crate) async fn commit(
        self,
        mut tx: calm_truth::db::sqlite::ObservedTransaction<'_>,
    ) -> Result<(Claim, DriverGuard)> {
        let Self {
            session,
            claim,
            guard,
            ticket,
            engine,
            held_fence,
            fence,
        } = self;
        let admitted = sql::load(&mut **tx, &session).await?;
        if admitted.as_ref().map(|c| &c.record().execution_id) != Some(&claim.record().execution_id)
        {
            return Err(conflict("admission is not in the committing transaction"));
        }
        #[cfg(feature = "fixtures")]
        crate::test_seams::pause_point("c5d-before-occupancy-commit", &session).await;
        tx.commit().await?;
        drop(held_fence);
        drop(fence);
        engine.drive(ticket.committed());
        Ok((claim, guard))
    }
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod turn_tests;
