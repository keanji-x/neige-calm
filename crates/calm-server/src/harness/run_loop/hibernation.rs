//! Quiescence runs on the queue/turn owner so a tick cannot start a turn between
//! an idle check and the stop. Earlier observations are already folded; later ones
//! make the sleep ineligible or are refused after the observation gate closes.
use super::*;

impl PlannerHarness {
    pub(crate) async fn hibernate_if_idle(&self) -> Result<bool> {
        let _durable = tokio::time::timeout(
            Duration::from_secs(5),
            self.inner.durable_observation.lock(),
        )
        .await
        .map_err(|_| {
            CalmError::ServiceUnavailable("Planner hibernation ingress wait expired".into())
        })?;
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
    let state = inner.state.lock().await.clone();
    if !ingress_empty
        || !matches!(
            state,
            HarnessState::Idle | HarnessState::TurnCompleted { .. } | HarnessState::Resumed { .. }
        )
        || !inner.historical_queue()?.lock().await.is_empty()
        || !inner.steered_into_running_turn.lock().await.is_empty()
    {
        return Ok(false);
    }
    let thread = inner.thread_id.read().await.clone();
    if thread
        .as_deref()
        .is_some_and(|thread| inner.backend.active_turn_id_for_thread(thread).is_some())
    {
        return Ok(false);
    }
    // Persist before closing input: the snapshot writer deliberately skips a
    // shutting-down owner. A failed write leaves the owner live and retryable.
    #[cfg(feature = "fixtures")]
    crate::test_seams::pause_point(
        crate::test_seams::PLANNER_HIBERNATION_QUIESCE,
        inner.card_id.as_str(),
    )
    .await;
    persist_snapshot(inner).await?;
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
