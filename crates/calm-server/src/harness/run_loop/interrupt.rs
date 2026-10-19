use super::*;
use provider::control::{InterruptOutcome, TurnIdentity};

pub(super) async fn issue_interrupt_for_turn(
    inner: &Arc<Inner>,
    target_turn_id: String,
    reason: String,
) -> Result<()> {
    let Some(thread_id) = inner.thread_id.read().await.clone() else {
        return Ok(());
    };
    {
        let mut state = inner.state.lock().await;
        if matches!(*state, HarnessState::Wedged { .. }) {
            return Ok(());
        }
        *inner.issued_turn_id.lock().await = None;
        if reason == "max_turn_duration" {
            use calm_types::harness::{
                HarnessInterruptionIntent, HarnessInterruptionReason, HarnessTimeoutIntent,
                HarnessTimeoutRequest,
            };
            if let HarnessState::TurnRunning {
                turn_id,
                watchdog_from,
                ..
            } = &*state
                && turn_id == &target_turn_id
            {
                let request = HarnessTimeoutRequest {
                    session_id: inner.worker_session_id.clone(),
                    card_id: inner.card_id.to_string(),
                    track_id: inner.track_id.to_string(),
                    thread_id: thread_id.clone(),
                    turn_id: target_turn_id.clone(),
                    reason: HarnessInterruptionReason::MaxTurnDuration,
                    requested_at_ms: crate::model::now_ms(),
                    effective_elapsed_ms: watchdog_from
                        .elapsed()
                        .as_millis()
                        .try_into()
                        .expect("elapsed fits u64"),
                    budget_ms: budget::limit(inner)
                        .await
                        .as_millis()
                        .try_into()
                        .expect("budget fits u64"),
                };
                *inner.interruption_intent.lock().await = Some(
                    HarnessInterruptionIntent::Versioned(HarnessTimeoutIntent::V1(request)),
                );
            }
        } else {
            // A later manual stop supersedes any earlier unconfirmed timeout request.
            *inner.interruption_intent.lock().await = None;
        }
        *state = HarnessState::Issuing {
            since: Instant::now(),
            kind: IssuingKind::Interrupt {
                target_turn_id: target_turn_id.clone(),
                reason: reason.clone(),
            },
        };
    }
    *inner.interrupt_deadline.lock().await = Some((
        target_turn_id.clone(),
        Instant::now() + inner.config.interrupt_completion_budget,
    ));
    persist_snapshot(inner).await?;
    let target = inner.backend.turn_identity(target_turn_id.clone());
    let outcome = IssueTurnHandle::from_reconciliation(inner)
        .await
        .turn_interrupt(InterruptRequest {
            turn: target.clone(),
        })
        .await;
    match (target, outcome) {
        (
            TurnIdentity::Native(_) | TurnIdentity::Local(_),
            InterruptOutcome::NativeAck
            | InterruptOutcome::NoTarget
            | InterruptOutcome::LocalSignalDelivered,
        )
        | (TurnIdentity::Local(_), InterruptOutcome::Unconfirmed { .. }) => {}
        (TurnIdentity::Native(_), InterruptOutcome::Unconfirmed { error: e, .. }) => {
            tracing::warn!(
                thread_id,
                turn_id = %target_turn_id,
                reason,
                error = %e,
                "planner harness turn/interrupt failed; interrupt timeout watchdog remains armed"
            );
        }
    }
    Ok(())
}
