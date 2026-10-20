//! Canonical maintenance transitions use the existing runtime frame and compact outcome.
use super::*;
use crate::auth::HumanInputSource;
use crate::db::sqlite::conversation::{
    RuntimeExecutionFence, current_input_carrier_tx, persist_runtime_frame_tx,
};
use crate::harness::backend::MaintenanceTarget;
use crate::harness::compact_outcome::{CompactOutcome, CompactStatus};
use crate::harness::native_admission::NativePromptAdmission;
use crate::harness::run_loop::canonical::{self, CommittedFrame};
use crate::harness::snapshot::{CanonicalHarnessSnapshot, canonical_runtime_header_value};

fn target(inner: &Inner, outcome: &CompactOutcome) -> MaintenanceTarget {
    MaintenanceTarget {
        card_id: inner.card_id.to_string(),
        track_id: inner.track_id.to_string(),
        worker_session_id: inner.worker_session_id.clone(),
        thread_id: outcome.thread_id.clone(),
        operation_id: outcome.operation_id.clone(),
    }
}

fn require_idle(snapshot: &CanonicalHarnessSnapshot) -> Result<()> {
    if !matches!(
        snapshot.header.phase,
        HarnessPhaseTag::Idle | HarnessPhaseTag::TurnCompleted
    ) || snapshot.canonical_inputs.active.is_some()
        || !snapshot.canonical_inputs.pending.is_empty()
        || snapshot.header.pending_rewind.is_some()
        || snapshot.header.interruption_intent.is_some()
        || snapshot.header.wedged_reason.is_some()
        || snapshot
            .header
            .compact_outcome
            .as_ref()
            .is_some_and(|outcome| unresolved_maintenance(&outcome.status))
    {
        return Err(CalmError::Conflict(
            "Wait for pending input and maintenance to finish before compacting.".into(),
        ));
    }
    if snapshot.header.last_turn_id.is_none() || snapshot.header.last_thread_id.is_none() {
        return Err(CalmError::Conflict(
            "Send a message before compacting this conversation.".into(),
        ));
    }
    Ok(())
}

enum TransitionAdmission {
    Start {
        source: HumanInputSource,
        target: MaintenanceTarget,
    },
    Settlement,
}

/// Only the runtime header is replaced. Truth preserves canonical inputs, including concurrent pending input.
async fn write(
    inner: &Inner,
    expected: &CommittedFrame,
    next: CanonicalHarnessSnapshot,
    admission: TransitionAdmission,
) -> Result<CommittedFrame> {
    let (card, worker, fence, provider, track) = (
        inner.card_id.to_string(),
        inner.worker_session_id.clone(),
        expected.fence.clone(),
        inner.backend.provider(),
        inner.track_id.to_string(),
    );
    write_in_tx_typed(inner.repo.as_ref(), move |tx| {
        Box::pin(async move {
            match admission {
                TransitionAdmission::Start { source, target } => {
                    let carrier = current_input_carrier_tx(tx, &card, &worker).await?;
                    let current: CanonicalHarnessSnapshot =
                        serde_json::from_value(carrier.snapshot().clone())?;
                    require_idle(&current)?;
                    require_binding(tx, &carrier, &provider, &track, &target.thread_id).await?;
                    crate::conversation_inputs::authorize_human_resource_tx(
                        tx,
                        &source,
                        carrier.card_id(),
                    )
                    .await?;
                    crate::conversation_archive::require_unarchived_tx(tx, &card).await?;
                }
                TransitionAdmission::Settlement => {}
            }
            canonical::written(
                persist_runtime_frame_tx(
                    tx,
                    &card,
                    &worker,
                    &fence,
                    canonical_runtime_header_value(&next)?,
                )
                .await?,
            )
        })
    })
    .await
}

async fn current_payload(
    inner: &Inner,
    source: HumanInputSource,
    expected: CommittedFrame,
    target: MaintenanceTarget,
) -> Result<Value> {
    let (card, worker, provider) = (
        inner.card_id.to_string(),
        inner.worker_session_id.clone(),
        inner.backend.provider(),
    );
    write_in_tx_typed(inner.repo.as_ref(), move |tx| {
        Box::pin(async move {
            let carrier = current_input_carrier_tx(tx, &card, &worker).await?;
            require_binding(tx, &carrier, &provider, &target.track_id, &target.thread_id).await?;
            crate::conversation_inputs::authorize_human_resource_tx(tx, &source, carrier.card_id())
                .await?;
            crate::conversation_archive::require_unarchived_tx(tx, &card).await?;
            let frame: CanonicalHarnessSnapshot =
                serde_json::from_value(carrier.snapshot().clone())?;
            if frame.header.phase != HarnessPhaseTag::Compacting
                || frame.header.last_thread_id.as_ref() != Some(&target.thread_id)
                || !frame
                    .header
                    .compact_outcome
                    .as_ref()
                    .is_some_and(|outcome| {
                        outcome.operation_id == target.operation_id
                            && outcome.thread_id == target.thread_id
                            && outcome.status == CompactStatus::Started
                    })
                || RuntimeExecutionFence::from_carrier(&carrier) != expected.fence
                || !carrier.checkpoint().pending.is_empty()
                || carrier.checkpoint().active.is_some()
            {
                return Err(CalmError::Conflict(
                    "maintenance operation or frame changed before stdin".into(),
                ));
            }
            Ok(crate::db::sqlite::card_execution_shape_tx(tx, &card)
                .await?
                .payload)
        })
    })
    .await
}

pub(in crate::harness::run_loop) async fn start(
    inner: &Arc<Inner>,
    source: HumanInputSource,
) -> Result<()> {
    let track = match super::super::issuance::lock_track_for_issuance(inner).await? {
        super::super::issuance::TrackIssuance::Acquired(guard) => guard,
        super::super::issuance::TrackIssuance::Busy
        | super::super::issuance::TrackIssuance::ShuttingDown
        | super::super::issuance::TrackIssuance::Superseded => {
            return Err(CalmError::Conflict(
                "conversation is busy or unavailable".into(),
            ));
        }
    };
    let _issuance = inner.issuance.lock().await;
    if inner.inputs.canonical().is_none()
        || inner.shutting_down.load(Ordering::SeqCst)
        || inner.backend.issuance_hold().is_some()
        || !inner.held_requests.is_empty()
        || inner.issuance_paused.load(Ordering::SeqCst)
    {
        return Err(CalmError::Conflict(
            "conversation is unavailable for canonical maintenance".into(),
        ));
    }
    let previous = canonical::read_current(inner).await?;
    require_idle(&previous.snapshot)?;
    let thread = previous
        .snapshot
        .header
        .last_thread_id
        .clone()
        .ok_or_else(|| CalmError::Conflict("maintenance has no thread".into()))?;
    if inner.backend.thread_sealed(&thread)
        || inner.backend.active_turn_id_for_thread(&thread).is_some()
    {
        return Err(CalmError::Conflict("conversation is busy or sealed".into()));
    }
    let outcome = CompactOutcome {
        operation_id: crate::model::new_id(),
        thread_id: thread.clone(),
        turn_id: None,
        status: CompactStatus::Started,
    };
    let maintenance = target(inner, &outcome);
    let mut next = previous.snapshot.clone();
    next.header.phase = HarnessPhaseTag::Compacting;
    next.header.compact_outcome = Some(outcome);
    let committed = write(
        inner,
        &previous,
        next,
        TransitionAdmission::Start {
            source: source.clone(),
            target: maintenance.clone(),
        },
    )
    .await?;
    canonical::publish(inner, committed.clone()).await;
    drop(track);
    let owner = MaintenanceAdmission {
        inner,
        source: &source,
        frame: &committed,
        target: &maintenance,
    };
    let admission = NativePromptAdmission::new(
        || owner.check_current(),
        || Box::pin(async { owner.payload().await.map(drop) }),
    );
    let submission = match inner.backend.compact_request(&owner).await {
        Ok(request) => {
            inner
                .backend
                .compact_canonical(maintenance.clone(), request, &admission)
                .await
        }
        Err(error) => Err(error),
    };
    let (status, error) = match submission {
        Ok(CompactSubmission::Submitted { .. }) => return Ok(()),
        Err(error) | Ok(CompactSubmission::NotSubmitted { error, .. }) => {
            (CompactStatus::Refused, error)
        }
        Ok(CompactSubmission::Unconfirmed { error, .. }) => (CompactStatus::Unconfirmed, error),
    };
    let mut next = committed.snapshot.clone();
    if status == CompactStatus::Refused {
        next.header.phase = previous.snapshot.header.phase;
        if let Some(outcome) = next.header.compact_outcome.as_mut() {
            outcome.status = status;
        }
    } else {
        wedge(&mut next);
    }
    let settled = write(inner, &committed, next, TransitionAdmission::Settlement).await?;
    canonical::publish(inner, settled).await;
    Err(error)
}

pub(in crate::harness::run_loop) fn wedge(snapshot: &mut CanonicalHarnessSnapshot) {
    snapshot.header.phase = HarnessPhaseTag::Wedged;
    snapshot.header.wedged_reason = Some(
        "Compaction outcome is unconfirmed; it will not be submitted again automatically.".into(),
    );
    if let Some(outcome) = snapshot.header.compact_outcome.as_mut() {
        outcome.status = CompactStatus::Unconfirmed;
    }
}

/// Poll the exact installed maintenance driver. Native notifications are wakeups, never proof.
pub(in crate::harness::run_loop) async fn observe(inner: &Arc<Inner>) -> Result<bool> {
    let current = canonical::read_current(inner).await?;
    if current.snapshot.header.phase != HarnessPhaseTag::Compacting {
        return Ok(false);
    }
    let Some(outcome) = current
        .snapshot
        .header
        .compact_outcome
        .as_ref()
        .filter(|outcome| outcome.status == CompactStatus::Started)
    else {
        return Ok(true);
    };
    let observed = inner
        .backend
        .observe_maintenance(&target(inner, outcome))
        .await?;
    let Some(observed) = observed else {
        return Ok(true);
    };
    let mut next = current.snapshot.clone();
    if outcome
        .turn_id
        .as_ref()
        .is_some_and(|turn| turn != &observed.turn_id)
    {
        wedge(&mut next);
    } else {
        if let Some(outcome) = next.header.compact_outcome.as_mut() {
            outcome.turn_id = Some(observed.turn_id);
        }
        match observed.settled {
            Some(true) => {
                next.header.phase = HarnessPhaseTag::TurnCompleted;
                if let Some(outcome) = next.header.compact_outcome.as_mut() {
                    outcome.status = CompactStatus::Completed;
                }
                canonical::invalidate_context_usage(&mut next.header);
            }
            Some(false) => wedge(&mut next),
            None => {}
        }
    }
    if next != current.snapshot {
        let committed = write(inner, &current, next, TransitionAdmission::Settlement).await?;
        canonical::publish(inner, committed).await;
    }
    Ok(true)
}

/// A boundary can invalidate retained occupancy even if later cleanup fails.
/// Only this operation's installed driver/local turn can supply that boundary.
pub(in crate::harness::run_loop) async fn boundary(inner: &Arc<Inner>, turn: &str) -> Result<bool> {
    let current = canonical::read_current(inner).await?;
    if !matches!(
        current.snapshot.header.phase,
        HarnessPhaseTag::Compacting | HarnessPhaseTag::Wedged
    ) {
        return Ok(false);
    }
    let Some(outcome) = current
        .snapshot
        .header
        .compact_outcome
        .as_ref()
        .filter(|outcome| {
            matches!(
                outcome.status,
                CompactStatus::Started | CompactStatus::Unconfirmed
            )
        })
    else {
        return Ok(false);
    };
    let Some(observed) = inner
        .backend
        .observe_maintenance(&target(inner, outcome))
        .await?
    else {
        return Ok(false);
    };
    if observed.turn_id != turn
        || outcome
            .turn_id
            .as_deref()
            .is_some_and(|bound| bound != turn)
    {
        return Ok(false);
    }
    let mut next = current.snapshot.clone();
    canonical::invalidate_context_usage(&mut next.header);
    let committed = write(inner, &current, next, TransitionAdmission::Settlement).await?;
    canonical::publish(inner, committed).await;
    Ok(true)
}

fn unresolved_maintenance(status: &CompactStatus) -> bool {
    match status {
        CompactStatus::Started | CompactStatus::Unconfirmed => true,
        CompactStatus::Completed
        | CompactStatus::Interrupted
        | CompactStatus::Failed
        | CompactStatus::Refused => false,
    }
}

/// Keep provider/owner/thread facts at the existing binding owner. Header CAS
/// covers the operation; the live worker row supplies its current native binding.
async fn require_binding(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    carrier: &crate::db::sqlite::conversation::CurrentInputCarrier,
    provider: &crate::session_projection_repo::AgentProvider,
    track: &str,
    thread: &str,
) -> Result<()> {
    let (binding, runtime) = crate::harness::profile::input_carrier_binding_tx(tx, carrier).await?;
    if binding.provider != *provider
        || runtime.track_id.as_str() != track
        || runtime.thread_id.as_deref() != Some(thread)
    {
        return Err(CalmError::Conflict(
            "maintenance provider, owner or original thread changed".into(),
        ));
    }
    Ok(())
}

/// Borrow the one committed maintenance guard and actual Human issuer. This
/// supplies metadata and writer admission without entering historical selection.
struct MaintenanceAdmission<'a> {
    inner: &'a Inner,
    source: &'a HumanInputSource,
    frame: &'a CommittedFrame,
    target: &'a MaintenanceTarget,
}

impl MaintenanceAdmission<'_> {
    async fn payload(&self) -> Result<Value> {
        current_payload(
            self.inner,
            self.source.clone(),
            self.frame.clone(),
            self.target.clone(),
        )
        .await
    }
}

impl SelectionSource for MaintenanceAdmission<'_> {
    fn check_current(&self) -> Result<()> {
        self.source.current_principal()?;
        if self.inner.backend.thread_sealed(&self.target.thread_id)
            || self.inner.shutting_down.load(Ordering::SeqCst)
            || self.inner.backend.issuance_hold().is_some()
            || !self.inner.held_requests.is_empty()
            || self.inner.issuance_paused.load(Ordering::SeqCst)
        {
            return Err(CalmError::Conflict(
                "maintenance owner is sealed or unavailable".into(),
            ));
        }
        Ok(())
    }

    fn admit_native_read(&self) -> futures_util::future::BoxFuture<'_, Result<()>> {
        Box::pin(async {
            self.payload().await?;
            self.check_current()
        })
    }

    async fn card_payload(&self) -> std::result::Result<Value, IssuanceRefusal> {
        self.payload()
            .await
            .map_err(|error| IssuanceRefusal::retryable(error.to_string()))
    }

    async fn installation_cwd(&self) -> std::result::Result<String, IssuanceRefusal> {
        self.payload()
            .await
            .map_err(|error| IssuanceRefusal::retryable(error.to_string()))?;
        let track = self
            .inner
            .repo
            .track_get(self.target.track_id.as_str())
            .await
            .map_err(|error| IssuanceRefusal::retryable(error.to_string()))?
            .ok_or_else(|| {
                IssuanceRefusal::retryable("maintenance track no longer exists".into())
            })?;
        Ok(track.workspace.agent_cwd().to_owned())
    }
}
