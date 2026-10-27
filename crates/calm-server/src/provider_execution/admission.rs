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

/// An admission written into the caller's writer transaction, which it now owns.
/// [`Admitted::commit`] is the only COMMIT and the only path to a drive; callers write
/// their own effects through [`Admitted::tx_mut`]. Dropping it rolls the transaction
/// back and releases the driver registration and the Track fence without a drive.
#[must_use = "commit the admitting transaction, or drop it to roll back without a drive"]
pub(crate) struct Admitted<'a> {
    // Declared first: dropping queues the rollback before the driver and fence release.
    tx: calm_truth::db::sqlite::ObservedTransaction<'static>,
    #[cfg(feature = "fixtures")]
    session: String,
    claim: Claim,
    guard: DriverGuard,
    ticket: PendingDriveTicket,
    engine: ExecutionEngine,
    held_fence: AdmittedFence<'a>,
    fence: OccupancyFence<'a>,
}

impl ProviderExecution {
    /// ① Admission core inside the caller's writer transaction, which it takes over.
    /// The caller checks Engine availability once at its entry, before BEGIN (see
    /// [`super::engine`]); it is not rechecked here, after the writer is acquired.
    /// Policy and intent run under the Track fence; driver registration precedes the
    /// claim and COMMIT, so RAII covers every later failure and cancellation. On error
    /// the transaction is dropped and rolls back.
    pub(crate) async fn admit_tx<'a, A: Admission>(
        &self,
        mut tx: calm_truth::db::sqlite::ObservedTransaction<'static>,
        engine: &ExecutionEngine,
        session: &str,
        admission: A,
        purpose: Purpose,
        fence: OccupancyFence<'a>,
    ) -> Result<Admitted<'a>> {
        let held_fence = fence.claim()?;
        admission.authorize_tx(&mut tx).await?;
        let intent = admission.intent_tx(&mut tx).await?;
        self.owner
            .alias_prefix()
            .validate(self.owner.as_ref(), session, &intent)?;
        held_fence.check(&intent.metadata.track)?;
        let id = intent.metadata.id.clone();
        let guard = self.drivers.claim(id.clone());
        let sql::Cas::Applied(claim) = sql::admit(
            &mut tx,
            session,
            id.as_str().into(),
            purpose,
            sql::InitialRelease::Held,
        )
        .await?
        else {
            return Err(conflict("admission lost"));
        };
        let ticket = engine.admit(&mut tx, intent).await.map_err(conflict)?;
        Ok(Admitted {
            tx,
            #[cfg(feature = "fixtures")]
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
    /// The admitting transaction, for the caller's writes that must commit with it.
    // WA2 turn issuance writes its phase CAS through this; until then only tests do.
    #[cfg(test)]
    pub(crate) fn tx_mut(&mut self) -> &mut sqlx::Transaction<'static, sqlx::Sqlite> {
        &mut self.tx
    }
    /// COMMIT the admitting transaction, then release the Track fence, then drive.
    pub(crate) async fn commit(self) -> Result<(Claim, DriverGuard)> {
        #[cfg(feature = "fixtures")]
        crate::test_seams::pause_point("c5d-before-occupancy-commit", &self.session).await;
        let Self {
            tx,
            claim,
            guard,
            ticket,
            engine,
            held_fence,
            fence,
            ..
        } = self;
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
