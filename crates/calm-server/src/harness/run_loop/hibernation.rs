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
            || !idle_for_lifecycle(&self.inner, ingress_empty).await
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
    if !idle_for_lifecycle(inner, ingress_empty).await {
        return Ok(false);
    }
    #[cfg(feature = "fixtures")]
    crate::test_seams::pause_point(
        crate::test_seams::PLANNER_HIBERNATION_QUIESCE,
        inner.card_id.as_str(),
    )
    .await;
    settle_owner_input_before_release(inner).await?;
    // Sleep hands restoration to the durable snapshot before unloading native
    // context, even when no input transfer remains. Require the existing guarded
    // writer to commit before closing ingress; Err/false keeps this owner live.
    persist_owned_input_snapshot(inner).await?;
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

async fn idle_for_lifecycle(inner: &Arc<Inner>, ingress_empty: bool) -> bool {
    let state = inner.state.lock().await.clone();
    if !ingress_empty
        || !matches!(
            state,
            HarnessState::Idle | HarnessState::TurnCompleted { .. } | HarnessState::Resumed { .. }
        )
        || !inner.pending_queue.lock().await.is_empty()
        || !inner.steered_into_running_turn.lock().await.is_empty()
    {
        return false;
    }
    let thread = inner.thread_id.read().await.clone();
    if thread
        .as_deref()
        .is_some_and(|thread| inner.backend.active_turn_id_for_thread(thread).is_some())
    {
        return false;
    }
    true
}
