//! Commands run to completion even if their reply receiver is dropped. SQL is the query authority.
use super::*;
use crate::harness::handoff_pause::{
    BackendFault, PauseQuery, PauseReceipt, PauseRequest, PauseState, ReleaseOutcome,
};

#[derive(Clone, Copy)]
pub(super) enum Action {
    Pause,
    Resume,
}

pub(super) enum Reply {
    State(PauseState),
    Released(ReleaseOutcome),
}

impl PlannerHarness {
    /// Internal Rust service API only. No user/AI ingress is wired to this primitive.
    pub async fn pause_for_handoff(&self, request: PauseRequest) -> Result<PauseReceipt> {
        match self.handoff_command(request, Action::Pause).await? {
            Reply::State(
                PauseState::Paused { receipt }
                | PauseState::FaultedPaused { receipt, .. }
                | PauseState::RecoveryHealthUnconfirmed { receipt },
            ) => Ok(receipt),
            _ => unreachable!("pause returns its committed claim"),
        }
    }

    /// Clears only the exact maintenance claim. Faulted release is not provider recovery.
    pub async fn resume_from_handoff(&self, receipt: PauseReceipt) -> Result<ReleaseOutcome> {
        if !receipt.matches_binding(
            &self.inner.card_id,
            &self.inner.worker_session_id,
            &self.inner.track_id,
        ) {
            return Err(conflict());
        }
        match crate::db::handoff_pause::query_handoff_pause(
            self.inner.repo.as_ref(),
            receipt.request().clone(),
        )
        .await
        {
            Ok(PauseQuery::Recorded(PauseState::Released { outcome, .. })) => return Ok(outcome),
            Ok(_) => {}
            Err(error) => {
                if !matches!(error, CalmError::Conflict(_)) {
                    close_observation_ingress(&self.inner);
                }
                return Err(error);
            }
        }
        match self
            .handoff_command(receipt.request().clone(), Action::Resume)
            .await?
        {
            Reply::Released(outcome) => Ok(outcome),
            _ => unreachable!("resume returns a typed release outcome"),
        }
    }

    /// After cancellation/lost reply, query or replay this same claim; never allocate a new one.
    /// FaultedPaused must cause a future handoff operation to abandon preparation.
    pub async fn query_handoff_pause(&self, request: PauseRequest) -> Result<PauseState> {
        if !request.matches_binding(
            &self.inner.card_id,
            &self.inner.worker_session_id,
            &self.inner.track_id,
        ) {
            return Err(conflict());
        }
        match crate::db::handoff_pause::query_handoff_pause(self.inner.repo.as_ref(), request)
            .await?
        {
            PauseQuery::Recorded(state) => Ok(state),
            PauseQuery::Unknown => Err(conflict()),
        }
    }

    async fn handoff_command(&self, request: PauseRequest, action: Action) -> Result<Reply> {
        let _ingress = self.inner.durable_observation.lock().await;
        match &self.inner.observations {
            ObservationIngress::Running(sender) => {
                let (answer, reply) = oneshot::channel();
                sender
                    .try_send(HarnessObservationCommand::HandoffPause {
                        request,
                        action,
                        answer,
                    })
                    .map_err(map_observation_send_error)?;
                reply.await.map_err(|_| {
                    CalmError::Conflict("handoff pause reply lost; query the same claim".into())
                })?
            }
            #[cfg(feature = "fixtures")]
            ObservationIngress::Unstarted(_) => handle(&self.inner, request, action).await,
        }
    }
}

pub(super) async fn require_unpaused(inner: &Inner) -> Result<()> {
    if inner.handoff_pause.lock().await.is_paused() {
        return Err(CalmError::Conflict(
            "conversation is paused for handoff preparation".into(),
        ));
    }
    Ok(())
}

fn conflict() -> CalmError {
    CalmError::Conflict("handoff pause claim or current conversation binding changed".into())
}

async fn admissible(inner: &Inner) -> Result<()> {
    use crate::harness::compact_outcome::CompactStatus;
    if !inner.state.lock().await.can_issue_turn()
        || inner
            .pending_queue
            .lock()
            .await
            .iter()
            .any(QueueEntry::is_user_authored)
        || !inner.held_requests.is_empty()
        || inner.backend.issuance_hold().is_some()
        || inner.provider_held.load(Ordering::SeqCst)
        || !inner.steered_into_running_turn.lock().await.is_empty()
        || inner.projection_client_id.lock().await.is_some()
        || inner.pending_rewind.lock().await.is_some()
        || inner
            .compact_outcome
            .lock()
            .await
            .as_ref()
            .is_some_and(|outcome| {
                matches!(
                    outcome.status,
                    CompactStatus::Started | CompactStatus::Unconfirmed
                )
            })
    {
        return Err(CalmError::Conflict(
            "conversation is not idle for handoff preparation".into(),
        ));
    }
    if let Some(thread) = inner.thread_id.read().await.as_deref()
        && (inner.backend.active_turn_id_for_thread(thread).is_some()
            || inner.backend.thread_sealed(thread))
    {
        return Err(conflict());
    }
    Ok(())
}

pub(super) async fn handle(
    inner: &Arc<Inner>,
    request: PauseRequest,
    action: Action,
) -> Result<Reply> {
    request.validate().map_err(|_| conflict())?;
    if !request.matches_binding(&inner.card_id, &inner.worker_session_id, &inner.track_id)
        || inner.shutting_down.load(Ordering::SeqCst)
    {
        return Err(conflict());
    }
    ensure_recovered_health(inner).await?;
    let _issuance = inner.issuance.lock().await;
    let before = inner.handoff_pause.lock().await.clone();
    if let PauseState::Released { receipt, outcome } = &before
        && receipt.request() == &request
    {
        return match action {
            Action::Pause => Err(conflict()),
            Action::Resume => {
                query_stored(inner, &before).await?;
                Ok(Reply::Released(outcome.clone()))
            }
        };
    }
    if before.is_paused() && before.receipt().is_some_and(|r| r.request() != &request) {
        return Err(conflict());
    }
    if matches!(action, Action::Resume) && !before.is_paused() {
        return Err(conflict());
    }
    if matches!(action, Action::Pause) && !before.is_paused() {
        admissible(inner).await?;
    }
    // These are the only fields this command stages. Queue/watermark cannot interleave on
    // this owner; compare the full snapshot after failure before allowing subsequent writes.
    let checkpoint = snapshot_for(inner).await;
    let state_before = inner.state.lock().await.clone();
    let after = match action {
        Action::Pause if before.is_paused() => before.clone(),
        Action::Pause => PauseState::Paused {
            receipt: PauseReceipt::from_request(request.clone()),
        },
        Action::Resume => before.release_active_claim().ok_or_else(conflict)?,
    };
    let result = if matches!(action, Action::Pause) && before.is_paused() {
        query_stored(inner, &before).await
    } else {
        // Only the serial owner stages this field. Public saves wait behind issuance, and the
        // existing snapshot writer captures the whole queue/watermark with the transition.
        *inner.handoff_pause.lock().await = after.clone();
        if matches!(action, Action::Resume)
            && let PauseState::Released { outcome, .. } = &after
            && let Some(state) = outcome.failed_state()
        {
            *inner.state.lock().await = state;
        }
        #[cfg(feature = "fixtures")]
        crate::test_seams::pause_point(
            crate::test_seams::HANDOFF_PAUSE_BEFORE_WRITE,
            inner.card_id.as_str(),
        )
        .await;
        match persist_snapshot_bound(
            inner,
            None,
            None,
            SnapshotWriteMode::HandoffPause {
                expected: before.clone(),
                prepare: matches!(action, Action::Pause),
            },
        )
        .await
        {
            Ok(true) => Ok(after),
            Ok(false) => Err(conflict()),
            Err(error) => Err(error),
        }
    };
    match result {
        Ok(state) => {
            *inner.handoff_pause.lock().await = state.clone();
            match (action, state) {
                (Action::Resume, PauseState::Released { outcome, .. }) => {
                    Ok(Reply::Released(outcome))
                }
                (Action::Pause, state) => Ok(Reply::State(state)),
                (Action::Resume, _) => Err(conflict()),
            }
        }
        Err(error) => {
            *inner.handoff_pause.lock().await = before;
            *inner.state.lock().await = state_before;
            // Restore the complete staged before-image. If SQL does not confirm that exact
            // snapshot (including queue and watermark), stop rather than installing partial facts.
            match inner
                .repo
                .session_projection_by_id(&inner.worker_session_id)
                .await
            {
                Ok(Some(row))
                    if row
                        .handle_state_json
                        .clone()
                        .and_then(HarnessSnapshot::parse_known)
                        .as_ref()
                        == Some(&checkpoint) => {}
                _ => close_observation_ingress(inner),
            }
            Err(error)
        }
    }
}

async fn query_stored(inner: &Arc<Inner>, expected: &PauseState) -> Result<PauseState> {
    let request = expected.receipt().ok_or_else(conflict)?.request().clone();
    match crate::db::handoff_pause::query_handoff_pause(inner.repo.as_ref(), request).await? {
        PauseQuery::Recorded(stored) if &stored == expected => Ok(stored),
        _ => Err(conflict()),
    }
}

/// A raw reconstructed handle must also refuse inherited health evidence. Normal recovery
/// persists this conversion before installation; this covers direct internal constructors.
pub(super) async fn ensure_recovered_health(inner: &Arc<Inner>) -> Result<()> {
    if !inner.recovered_pause_health_pending.load(Ordering::SeqCst) {
        return Ok(());
    }
    let _issuance = inner.issuance.lock().await;
    if !inner.recovered_pause_health_pending.load(Ordering::SeqCst) {
        return Ok(());
    }
    let before = inner.handoff_pause.lock().await.clone();
    inner
        .handoff_pause
        .lock()
        .await
        .unconfirm_recovered_health();
    match persist_snapshot_bound(
        inner,
        None,
        None,
        SnapshotWriteMode::HandoffPause {
            expected: before,
            prepare: false,
        },
    )
    .await
    {
        Ok(true) => {
            inner
                .recovered_pause_health_pending
                .store(false, Ordering::SeqCst);
            Ok(())
        }
        result => {
            close_observation_ingress(inner);
            result.and(Err(conflict()))
        }
    }
}

/// Called by the notification owner after the normal thread identity filter. No queue, phase,
/// intent, compact or deadline fields change: pause admission already excluded active work.
pub(super) async fn record_backend_fault(inner: &Arc<Inner>, fault: BackendFault) -> Result<bool> {
    let _issuance = inner.issuance.lock().await;
    let before = inner.handoff_pause.lock().await.clone();
    if !before.is_paused() {
        return Ok(false);
    }
    let Some(receipt) = before.receipt().cloned() else {
        return Ok(false);
    };
    let after = match &before {
        PauseState::FaultedPaused { .. } => before.clone(),
        _ => PauseState::FaultedPaused { receipt, fault },
    };
    if before == after {
        return Ok(true);
    }
    *inner.handoff_pause.lock().await = after;
    match persist_snapshot_bound(
        inner,
        None,
        None,
        SnapshotWriteMode::HandoffPause {
            expected: before.clone(),
            prepare: false,
        },
    )
    .await
    {
        Ok(true) => Ok(true),
        result => {
            // Keep the irreversible observed fault sticky. SQL projection rollback is not
            // provider recovery. A replacement without this memory must unconfirm health.
            // A provider fault was observed even if its commit cannot be confirmed. Never
            // continue healthy or overwrite a potentially committed fault with a later ACK.
            close_observation_ingress(inner);
            result.and(Err(conflict()))
        }
    }
}
