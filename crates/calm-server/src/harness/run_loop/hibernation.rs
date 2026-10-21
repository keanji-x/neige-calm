//! Quiescence runs on the queue/turn owner so a tick cannot start a turn between
//! an idle check and the stop. Earlier observations are already folded; later ones
//! make the sleep ineligible or are refused after the observation gate closes.
use super::*;

impl PlannerHarness {
    /// Hold the existing turn/compaction issuance fence through a lifecycle write.
    /// Unlike hibernation, this neither stops the owner nor releases native context.
    pub(crate) async fn lock_if_idle(&self) -> Result<tokio::sync::MutexGuard<'_, ()>> {
        let guard = tokio::time::timeout(Duration::from_secs(5), self.inner.issuance.lock())
            .await
            .map_err(|_| {
                CalmError::ServiceUnavailable(
                    "Conversation is still settling; try again shortly.".into(),
                )
            })?;
        let ingress_empty = match &self.inner.observations {
            ObservationIngress::Running(sender) => sender.capacity() == sender.max_capacity(),
            #[cfg(feature = "fixtures")]
            ObservationIngress::Unstarted(sender) => sender.capacity() == sender.max_capacity(),
        };
        if self.inner.shutting_down.load(Ordering::SeqCst)
            || self.inner.parked.is_parked()
            || !idle_for_lifecycle(&self.inner, ingress_empty).await?
        {
            return Err(CalmError::Conflict("Wait for the reply and queued messages to finish before archiving this conversation.".into()));
        }
        Ok(guard)
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
    idle_for_lifecycle_guarded(inner, ingress_empty, false).await
}

async fn idle_for_lifecycle_guarded(
    inner: &Arc<Inner>,
    ingress_empty: bool,
    native_guarded: bool,
) -> Result<bool> {
    let state = inner.state.lock().await.clone();
    if !ingress_empty
        || !matches!(
            state,
            HarnessState::Idle | HarnessState::TurnCompleted { .. } | HarnessState::Resumed { .. }
        )
        || !inner.inputs.idle_for_lifecycle(inner).await?
        || !inner.steered_into_running_turn.lock().await.is_empty()
    {
        return Ok(false);
    }
    let thread = inner.thread_id.read().await.clone();
    if !native_guarded
        && thread
            .as_deref()
            .is_some_and(|thread| inner.backend.active_turn_id_for_thread(thread).is_some())
    {
        return Ok(false);
    }
    Ok(true)
}

/// Parks issuance, never ingress. No lock guard crosses the caller's SQL window.
/// Acquire/recheck before opening SQL; durable predicates are rechecked in SQL.
pub struct IdleCarrierOwner<'a> {
    handle: &'a PlannerHarness,
    native: crate::harness::backend::CarrierIdleGuard<'a>,
    park: crate::harness::carrier_park::ParkToken<'a>,
}
impl IdleCarrierOwner<'_> {
    pub(crate) fn matches_current(&self, card: &str, worker: &str) -> bool {
        let inner = &self.handle.inner;
        inner.card_id.as_str() == card
            && inner.worker_session_id == worker
            && self.park.is_current()
            && self.native.recheck()
            && !inner.shutting_down.load(Ordering::SeqCst)
            && inner.held_requests.is_empty()
    }

    pub async fn recheck(&self) -> Result<()> {
        let inner = &self.handle.inner;
        let _issuance = inner.issuance.lock().await;
        if !self.park.is_current()
            || inner.shutting_down.load(Ordering::SeqCst)
            || !inner.held_requests.is_empty()
            || !self.native.recheck()
            || !idle_for_lifecycle_guarded(inner, ingress_empty(inner), true).await?
        {
            return Err(CalmError::Conflict(
                "carrier cannot prove safe detachment".into(),
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
        let issuance = self.lock_if_idle().await?;
        let thread = self.inner.thread_id.read().await.clone();
        let native = self
            .inner
            .backend
            .carrier_idle_for_detach(thread.as_deref())
            .ok_or_else(|| {
                CalmError::Conflict("native owner cannot prove safe detachment".into())
            })?;
        let park = self
            .inner
            .parked
            .park()
            .ok_or_else(|| CalmError::Conflict("carrier is already parked".into()))?;
        let proof = IdleCarrierOwner {
            handle: self,
            park,
            native,
        };
        drop(issuance);
        proof.recheck().await?;
        Ok(proof)
    }
}
