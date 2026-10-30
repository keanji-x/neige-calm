//! Quiescence runs on the queue/turn owner so a tick cannot start a turn between
//! an idle check and the stop. Earlier observations are already folded; later ones
//! make the sleep ineligible or are refused after the observation gate closes.
use super::*;

/// Owner-local refusal, kept separate from SQL/provider failures and public errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OwnerRefusal {
    TurnInFlight,
    OpenAsk,
    PendingMessages,
    PendingObservations,
    PendingSubmission,
    SessionUnavailable,
    Changed,
}

#[derive(Debug)]
pub enum OwnerProofError {
    Refused {
        reason: OwnerRefusal,
        error: CalmError,
    },
    Failed(CalmError),
}
impl OwnerProofError {
    pub fn refused(reason: OwnerRefusal, error: CalmError) -> Self {
        Self::Refused { reason, error }
    }
    pub fn into_error(self) -> CalmError {
        match self {
            Self::Refused { error, .. } | Self::Failed(error) => error,
        }
    }
}
impl From<CalmError> for OwnerProofError {
    fn from(error: CalmError) -> Self {
        Self::Failed(error)
    }
}

impl PlannerHarness {
    /// Hold the existing turn/compaction issuance fence through a lifecycle write.
    /// Unlike hibernation, this neither stops the owner nor releases native context.
    pub(crate) async fn lock_if_idle(&self) -> Result<tokio::sync::MutexGuard<'_, ()>> {
        self.lock_if_idle_typed()
            .await
            .map_err(OwnerProofError::into_error)
    }

    pub(crate) async fn hibernate_if_idle(&self) -> Result<bool> {
        let _durable = tokio::time::timeout(
            Duration::from_secs(5),
            self.inner.durable_observation.lock(),
        )
        .await
        .map_err(|_| {
            CalmError::ServiceUnavailable("Planner hibernation ingress wait expired".into())
        })?;
        // A previous cancelled release may already be closing this owner.
        // Its flag is not this hibernation request's settlement acknowledgement.
        if self.inner.shutting_down.load(Ordering::SeqCst) {
            return Ok(false);
        }
        match &self.inner.observations {
            ObservationIngress::Running(sender) => {
                let (done, answer) = oneshot::channel();
                sender
                    .try_send(HarnessObservationCommand::Hibernate { done })
                    .map_err(map_observation_send_error)?;
                match tokio::time::timeout(Duration::from_secs(5), answer).await {
                    Ok(answer) => answer.map_err(|_| {
                        CalmError::Conflict(
                            "Planner stopped before hibernation was confirmed".into(),
                        )
                    })?,
                    Err(_) => {
                        // timeout drops the receiver. Synchronize with the gate:
                        // either quiesce already closed input (remove its owner),
                        // or it will see the cancelled reply and stay retryable.
                        if *self
                            .inner
                            .observations_closed
                            .lock()
                            .expect("observation gate")
                        {
                            Ok(true)
                        } else {
                            Err(CalmError::ServiceUnavailable(
                                "Planner hibernation wait expired".into(),
                            ))
                        }
                    }
                }
            }
            #[cfg(feature = "fixtures")]
            ObservationIngress::Unstarted(sender) => {
                quiesce(
                    &self.inner,
                    sender.capacity() == sender.max_capacity(),
                    None,
                )
                .await
            }
        }
    }

    pub(crate) async fn release_hibernated_resources(&self) -> Result<()> {
        self.inner.backend.hibernate().await
    }
}

pub(super) async fn quiesce(
    inner: &Arc<Inner>,
    ingress_empty: bool,
    reply: Option<&oneshot::Sender<Result<bool>>>,
) -> Result<bool> {
    let _issuance = inner.issuance.lock().await;
    if inner.parked.is_parked() || !idle_for_lifecycle(inner, ingress_empty).await? {
        return Ok(false);
    }
    #[cfg(feature = "fixtures")]
    crate::test_seams::pause_point(
        crate::test_seams::PLANNER_HIBERNATION_QUIESCE,
        inner.card_id.as_str(),
    )
    .await;
    if !inner.inputs.checkpoint_for_lifecycle(inner).await? {
        return Ok(false);
    }
    // observe_delivery holds this mutex through enqueue. Recheck the ingress under
    // that gate so an enqueue cannot disappear behind the stop command.
    {
        let mut closed = inner.observations_closed.lock().expect("observation gate");
        // A timed-out request must not close an owner after its caller has left.
        if reply.is_some_and(oneshot::Sender::is_closed) {
            return Ok(false);
        }
        if let ObservationIngress::Running(sender) = &inner.observations
            && sender.capacity() != sender.max_capacity()
        {
            return Ok(false);
        }
        *closed = true;
        inner.shutting_down.store(true, Ordering::SeqCst);
    }
    held::release_all(inner);
    Ok(true)
}

async fn idle_for_lifecycle(inner: &Arc<Inner>, ingress_empty: bool) -> Result<bool> {
    Ok(idle_for_lifecycle_guarded(inner, ingress_empty, false)
        .await?
        .is_none())
}

async fn idle_for_lifecycle_guarded(
    inner: &Arc<Inner>,
    ingress_empty: bool,
    native_guarded: bool,
) -> Result<Option<OwnerRefusal>> {
    let state = inner.state.lock().await.clone();
    if inner
        .planner_compaction
        .lock()
        .await
        .as_ref()
        .is_some_and(|r| r.pending())
    {
        return Ok(Some(OwnerRefusal::TurnInFlight));
    }
    if !ingress_empty {
        return Ok(Some(OwnerRefusal::PendingObservations));
    }
    if !matches!(
        state,
        HarnessState::Idle | HarnessState::TurnCompleted { .. } | HarnessState::Resumed { .. }
    ) {
        return Ok(Some(OwnerRefusal::TurnInFlight));
    }
    if let Some(reason) = inner.inputs.lifecycle_refusal(inner).await? {
        return Ok(Some(reason));
    }
    if !inner.steered_into_running_turn.lock().await.is_empty() {
        return Ok(Some(OwnerRefusal::PendingMessages));
    }
    let thread = inner.thread_id.read().await.clone();
    if !native_guarded
        && thread
            .as_deref()
            .is_some_and(|thread| inner.backend.active_turn_id_for_thread(thread).is_some())
    {
        return Ok(Some(OwnerRefusal::TurnInFlight));
    }
    Ok(None)
}

/// Parks issuance, never ingress. No lock guard crosses the caller's SQL window.
/// Acquire/recheck before opening SQL; durable predicates are rechecked in SQL.
pub struct IdleCarrierOwner<'a> {
    handle: &'a PlannerHarness,
    native: crate::harness::backend::CarrierIdleGuard<'a>,
    park: crate::harness::carrier_park::ParkToken<'a>,
}
impl IdleCarrierOwner<'_> {
    pub(crate) fn check_current(
        &self,
        card: &str,
        worker: &str,
    ) -> std::result::Result<(), OwnerRefusal> {
        let inner = &self.handle.inner;
        if inner.card_id.as_str() != card
            || inner.worker_session_id != worker
            || !self.park.is_current()
        {
            return Err(OwnerRefusal::Changed);
        }
        if !self.native.recheck() || inner.shutting_down.load(Ordering::SeqCst) {
            return Err(OwnerRefusal::SessionUnavailable);
        }
        if !inner.held_requests.is_empty() {
            return Err(OwnerRefusal::OpenAsk);
        }
        Ok(())
    }

    pub async fn recheck(&self) -> Result<()> {
        self.recheck_typed()
            .await
            .map_err(OwnerProofError::into_error)
    }

    pub async fn recheck_typed(&self) -> std::result::Result<(), OwnerProofError> {
        let inner = &self.handle.inner;
        let _issuance = inner.issuance.lock().await;
        let reason = match self.check_current(inner.card_id.as_str(), &inner.worker_session_id) {
            Err(reason) => Some(reason),
            Ok(()) => idle_for_lifecycle_guarded(inner, ingress_empty(inner), true).await?,
        };
        if let Some(reason) = reason {
            return Err(OwnerProofError::refused(
                reason,
                CalmError::Conflict("carrier cannot prove safe detachment".into()),
            ));
        }
        Ok(())
    }
}

fn ingress_empty(inner: &Inner) -> bool {
    match &inner.observations {
        ObservationIngress::Running(sender) => sender.capacity() == sender.max_capacity(),
        #[cfg(feature = "fixtures")]
        ObservationIngress::Unstarted(sender) => sender.capacity() == sender.max_capacity(),
    }
}
impl PlannerHarness {
    pub async fn idle_carrier_owner(&self) -> Result<IdleCarrierOwner<'_>> {
        self.idle_carrier_owner_typed()
            .await
            .map_err(OwnerProofError::into_error)
    }

    async fn lock_if_idle_typed(
        &self,
    ) -> std::result::Result<tokio::sync::MutexGuard<'_, ()>, OwnerProofError> {
        let guard = tokio::time::timeout(Duration::from_secs(5), self.inner.issuance.lock())
            .await
            .map_err(|_| {
                OwnerProofError::refused(
                    OwnerRefusal::SessionUnavailable,
                    CalmError::ServiceUnavailable(
                        "Conversation is still settling; try again shortly.".into(),
                    ),
                )
            })?;
        let reason =
            if self.inner.shutting_down.load(Ordering::SeqCst) || self.inner.parked.is_parked() {
                Some(OwnerRefusal::SessionUnavailable)
            } else {
                idle_for_lifecycle_guarded(&self.inner, ingress_empty(&self.inner), false).await?
            };
        if let Some(reason) = reason {
            return Err(OwnerProofError::refused(reason, CalmError::Conflict("Wait for the reply and queued messages to finish before archiving this conversation.".into())));
        }
        Ok(guard)
    }

    pub async fn idle_carrier_owner_typed(
        &self,
    ) -> std::result::Result<IdleCarrierOwner<'_>, OwnerProofError> {
        let issuance = self.lock_if_idle_typed().await?;
        let thread = self.inner.thread_id.read().await.clone();
        let native = self
            .inner
            .backend
            .carrier_idle_for_detach(thread.as_deref())
            .await
            .ok_or_else(|| {
                OwnerProofError::refused(
                    OwnerRefusal::SessionUnavailable,
                    CalmError::Conflict("native owner cannot prove safe detachment".into()),
                )
            })?;
        let park = self.inner.parked.park().ok_or_else(|| {
            OwnerProofError::refused(
                OwnerRefusal::SessionUnavailable,
                CalmError::Conflict("carrier is already parked".into()),
            )
        })?;
        let proof = IdleCarrierOwner {
            handle: self,
            park,
            native,
        };
        drop(issuance);
        proof.recheck_typed().await?;
        Ok(proof)
    }
}

#[cfg(test)]
mod planner_switch_tests;

/// Recovery excludes new issuance while preserving the unresolved canonical frame.
/// Native idleness is a local fence only; the ACP lifecycle separately proves quiescence.
pub(crate) struct HeldCarrierOwner<'a> {
    handle: &'a PlannerHarness,
    native: crate::harness::backend::CarrierIdleGuard<'a>,
    park: crate::harness::carrier_park::ParkToken<'a>,
}
impl HeldCarrierOwner<'_> {
    pub(crate) fn recheck(&self) -> Result<()> {
        let inner = &self.handle.inner;
        if !self.park.is_current()
            || !self.native.recheck()
            || inner.shutting_down.load(Ordering::SeqCst)
            || !inner.held_requests.is_empty()
        {
            return Err(CalmError::Conflict(
                "recovery owner changed or is still active".into(),
            ));
        }
        Ok(())
    }
}
impl PlannerHarness {
    pub(crate) async fn held_carrier_owner(&self) -> Result<HeldCarrierOwner<'_>> {
        let _issuance = self.inner.issuance.lock().await;
        if !matches!(self.inner.inputs, RuntimeInputs::Canonical(_))
            || self
                .inner
                .planner_compaction
                .lock()
                .await
                .as_ref()
                .is_some_and(|r| r.pending())
        {
            return Err(CalmError::Conflict(
                "recovery requires a held canonical carrier".into(),
            ));
        }
        let thread = self.inner.thread_id.read().await.clone();
        let native = self
            .inner
            .backend
            .carrier_idle_for_recovery(thread.as_deref())
            .await
            .ok_or_else(|| CalmError::Conflict("old native execution is still active".into()))?;
        let park = self
            .inner
            .parked
            .park()
            .ok_or_else(|| CalmError::Conflict("carrier is already parked".into()))?;
        let owner = HeldCarrierOwner {
            handle: self,
            native,
            park,
        };
        owner.recheck()?;
        Ok(owner)
    }
}

impl PlannerHarness {
    pub(crate) async fn restore_normal_join_after_keep(&self) -> Result<()> {
        let _issuance = self.inner.issuance.lock().await;
        self.inner.backend.restore_normal_join_after_keep().await
    }
}
