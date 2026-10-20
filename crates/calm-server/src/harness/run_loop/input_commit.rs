//! The unfinished commit of an actual provider result, owned by the input actor.
//! Only identities/outcomes live here; pending, steered and restored input stay canonical.
use super::*;

#[derive(Debug)]
pub(super) enum Continuation {
    IssueResolved {
        client_id: QueueEntryId,
        turn_id: Option<String>,
    },
    SteerAccepted(SteerApplied),
}

impl Continuation {
    fn trace(&self) {
        match self {
            Self::IssueResolved { client_id, turn_id } => {
                tracing::debug!(%client_id, ?turn_id, "committing resolved issuance");
            }
            Self::SteerAccepted(outcome) => {
                tracing::debug!(entry_id = %outcome.entry_id, turn_id = %outcome.turn_id,
                    "committing accepted steer");
            }
        }
    }
}

/// Keep snapshot writers out until the result has been incorporated into canonical input.
pub(super) async fn retain_result(
    inner: &Inner,
    continuation: Continuation,
) -> tokio::sync::MutexGuard<'_, Option<Continuation>> {
    let mut slot = inner.input_commit.lock().await;
    assert!(
        slot.is_none(),
        "input actor must finish its previous result first"
    );
    *slot = Some(continuation);
    slot
}

/// One attempt, never a provider retry or a frozen-snapshot write. Terminal processing may have
/// changed the canonical queue since the result was accepted; commit that current state.
pub(super) async fn resume(inner: &Arc<Inner>) -> Result<()> {
    let mut slot = inner.input_commit.lock().await;
    let Some(continuation) = slot.as_ref() else {
        return Ok(());
    };
    continuation.trace();
    if let Continuation::SteerAccepted(outcome) = continuation
        && crate::operation::planner_harness_start_adapter::input_transfer::committed_transfer(
            inner.repo.as_ref(),
            inner.card_id.as_str(),
            &inner.worker_session_id,
            &crate::model::HarnessQueueEntryRef {
                id: outcome.entry_id.to_string(),
                rev: outcome.rev,
            },
        )
        .await?
        .is_some()
    {
        *slot = None;
        return Ok(());
    }
    drop(slot);
    let attempted = persist_owned_input_snapshot(inner).await;
    // An overlapping snapshot owner may have committed this very result and retired
    // the active guard before our attempt. Only that completion (not our Err/false)
    // discharges the obligation. No new result can start while this actor is resuming.
    if inner.input_commit.lock().await.is_none() {
        Ok(())
    } else {
        attempted
    }
}

/// The original issuance boundary alone retains its retired-row writer. Release and later ticks
/// cannot borrow that authority after MOVE. A current row still uses its original guarded writer.
pub(super) async fn commit_issue(inner: &Arc<Inner>) -> Result<()> {
    // This actual operation owns the commit even if ingress closed while the provider answered.
    // An active miss can still use only the original issuance boundary's retired writer.
    if !persist_guarded_snapshot(inner, None, None).await?
        && !persist_issuance_outcome(inner).await?
    {
        return Err(CalmError::Conflict(
            "conversation refused its issuance result checkpoint".into(),
        ));
    }
    Ok(())
}

fn busy() -> CalmError {
    CalmError::ServiceUnavailable(
        "conversation is committing its previous input; retry shortly".into(),
    )
}

/// Reversible commands do not begin another operation over an unresolved result. Reads,
/// replayable observations, held messages and terminal notifications remain available.
pub(super) async fn admit(
    inner: &Inner,
    command: HarnessObservationCommand,
) -> Option<HarnessObservationCommand> {
    if inner.input_commit.lock().await.is_none() {
        return Some(command);
    }
    match command {
        HarnessObservationCommand::Hibernate { done } => {
            let _ = done.send(Err(busy()));
        }
        HarnessObservationCommand::Compact { answer, source: _ } => {
            let _ = answer.send(Err(busy()));
        }
        HarnessObservationCommand::Durable { persisted, .. } => {
            let _ = persisted.send(Err(busy()));
        }
        HarnessObservationCommand::Mutate { applied, .. } => {
            let _ = applied.send(Err(busy()));
        }
        HarnessObservationCommand::Steer { applied, .. } => {
            let _ = applied.send(Err(busy()));
        }
        HarnessObservationCommand::Replace { answer, .. } => {
            let _ = answer.send(Err(busy()));
        }
        command @ (HarnessObservationCommand::InspectInput { .. }
        | HarnessObservationCommand::CanonicalInterrupt { .. }
        | HarnessObservationCommand::CanonicalSource { .. }
        | HarnessObservationCommand::Release { .. }
        | HarnessObservationCommand::Delivery(_)) => return Some(command),
    }
    None
}
