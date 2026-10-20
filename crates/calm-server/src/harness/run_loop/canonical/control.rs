//! Existing loop controls commit their header on the sole current carrier.
//! Native effects remain with the backend owner; this module owns no queue,
//! Human source, retry driver, or second execution registry.
use super::*;
use crate::auth::HumanInputSource;
use crate::db::sqlite::conversation::current_input_fact_carrier_tx;
use crate::harness::backend::{BudgetControlScope, ExecutionEndTarget};
use crate::harness::input_dispatch::execution_target_tx;
use calm_types::harness::{
    HarnessControlIntent, HarnessInterruptionIntent, HarnessInterruptionReason, HarnessStopRequest,
    HarnessTimeoutIntent, HarnessTimeoutRequest,
};

#[derive(Clone)]
enum Purpose {
    Warning { start: String },
    Interrupt { intent: HarnessInterruptionIntent },
}

/// Live invocation data revalidated after the actual native connection gate.
/// Kernel execution policy controls the original execution, not Human work.
#[derive(Clone)]
struct Admission {
    inner: Arc<Inner>,
    target: ExecutionEndTarget,
    purpose: Purpose,
}

impl Admission {
    async fn admit(&self) -> Result<()> {
        let (card, worker, provider, target, purpose) = (
            self.inner.card_id.to_string(),
            self.inner.worker_session_id.clone(),
            self.inner.backend.provider(),
            self.target.clone(),
            self.purpose.clone(),
        );
        write_in_tx_typed(self.inner.repo.as_ref(), move |tx| Box::pin(async move {
            let carrier = current_input_fact_carrier_tx(tx, &card, &worker).await?;
            if execution_target_tx(tx, &card, &worker, &provider).await?.as_ref() != Some(&target) {
                return Err(CalmError::Conflict("control execution changed at native admission".into()));
            }
            let next: CanonicalHarnessSnapshot = serde_json::from_value(carrier.snapshot().clone())?;
            let admitted = match purpose {
                Purpose::Warning { start } => carrier.is_execution_current()
                    && next.header.phase == HarnessPhaseTag::TurnRunning
                    && matches!(&next.header.turn_budget, budget::FrozenBudget::Planner { start_attempt, warning_attempted:true,.. }
                        if start_attempt.as_str() == start),
                // An accepted exact drain survives failure of new-work admission.
                // The execution target above and this durable intent still bind it.
                Purpose::Interrupt { intent } => next.header.interruption_intent.as_ref() == Some(&intent),
            };
            if !admitted {
                return Err(CalmError::Conflict("control intent is no longer current".into()));
            }
            Ok(())
        })).await
    }
}

enum Change {
    Warning {
        target: ExecutionEndTarget,
        start: String,
    },
    Interrupt {
        target: ExecutionEndTarget,
        intent: HarnessInterruptionIntent,
        admission: StopAdmission,
    },
    Failed {
        reason: String,
    },
}

enum StopAdmission {
    Human(HumanInputSource),
    KernelBudget,
}

/// Derive every proposal from the current protected frame, never a cache. The
/// whole execution fence excludes newer pending input but includes the full
/// header/active/attempts. The Truth writer preserves that pending authority.
async fn transition(inner: &Arc<Inner>, change: Change) -> Result<CommittedFrame> {
    let (card, worker, provider) = (
        inner.card_id.to_string(),
        inner.worker_session_id.clone(),
        inner.backend.provider(),
    );
    let next = write_in_tx_typed(inner.repo.as_ref(), move |tx| {
        Box::pin(async move {
            let carrier = current_input_fact_carrier_tx(tx, &card, &worker).await?;
            let mut next: CanonicalHarnessSnapshot =
                serde_json::from_value(carrier.snapshot().clone())?;
            match change {
                Change::Warning { target, start } => {
                    require_running(&carrier, &next, &target)?;
                    if execution_target_tx(tx, &card, &worker, &provider)
                        .await?
                        .as_ref()
                        != Some(&target)
                    {
                        return Err(CalmError::Conflict("budget execution changed".into()));
                    }
                    match &mut next.header.turn_budget {
                        budget::FrozenBudget::Planner {
                            start_attempt,
                            warning_attempted,
                            ..
                        } if start_attempt.as_str() == start && !*warning_attempted => {
                            *warning_attempted = true
                        }
                        _ => {
                            return Err(CalmError::Conflict(
                                "budget reminder already attempted or changed".into(),
                            ));
                        }
                    }
                }
                Change::Interrupt {
                    target,
                    intent,
                    admission,
                } => {
                    require_running(&carrier, &next, &target)?;
                    if let StopAdmission::Human(source) = admission {
                        crate::conversation_inputs::authorize_human_resource_tx(
                            tx, &source, &carrier,
                        )
                        .await?;
                    }
                    if execution_target_tx(tx, &card, &worker, &provider)
                        .await?
                        .as_ref()
                        != Some(&target)
                    {
                        return Err(CalmError::Conflict("stop execution changed".into()));
                    }
                    next.header.phase = HarnessPhaseTag::IssuingInterrupt;
                    next.header.interruption_intent = Some(intent);
                }
                Change::Failed { reason } => {
                    next.header.phase = HarnessPhaseTag::Wedged;
                    next.header.wedged_reason = Some(reason);
                }
            }
            let persisted = persist_runtime_frame_tx(
                tx,
                &card,
                &worker,
                &RuntimeExecutionFence::from_carrier(&carrier),
                header_value(&next)?,
            )
            .await?;
            let status = if !carrier.is_execution_current()
                || next.header.phase == HarnessPhaseTag::Wedged
            {
                calm_types::worker::WorkerSessionState::Failed
            } else if next.header.phase == HarnessPhaseTag::TurnRunning {
                calm_types::worker::WorkerSessionState::Running
            } else {
                calm_types::worker::WorkerSessionState::TurnPending
            };
            crate::db::sqlite::session_set_harness_observation_runtime_tx(
                tx,
                &worker,
                status,
                next.header.last_thread_id.as_deref(),
                next.header.last_turn_id.as_deref(),
            )
            .await?;
            written(persisted)
        })
    })
    .await?;
    publish(inner, next.clone()).await;
    Ok(next)
}

fn require_running(
    carrier: &CurrentInputCarrier,
    frame: &CanonicalHarnessSnapshot,
    target: &ExecutionEndTarget,
) -> Result<()> {
    if !carrier.is_execution_current()
        || frame.header.phase != HarnessPhaseTag::TurnRunning
        || frame.header.interruption_intent.is_some()
        || frame.header.wedged_reason.is_some()
        || target.worker_session_id != carrier.worker_session_id()
    {
        return Err(CalmError::Conflict(
            "the original execution is no longer running".into(),
        ));
    }
    Ok(())
}

pub(in crate::harness::run_loop) async fn select_stop(
    inner: &Arc<Inner>,
) -> Result<ExecutionEndTarget> {
    let (card, worker, provider) = (
        inner.card_id.to_string(),
        inner.worker_session_id.clone(),
        inner.backend.provider(),
    );
    write_in_tx_typed(inner.repo.as_ref(), move |tx| Box::pin(async move {
        let carrier = current_input_fact_carrier_tx(tx, &card, &worker).await?;
        let current: CanonicalHarnessSnapshot = serde_json::from_value(carrier.snapshot().clone())?;
        if current.header.phase == HarnessPhaseTag::IssuingTurn {
            return Err(CalmError::Conflict("The turn has not confirmed its native identity; retry Stop once it is running.".into()));
        }
        let target = execution_target_tx(tx, &card, &worker, &provider).await?
            .ok_or_else(|| CalmError::Conflict("Stop has no confirmed original native target".into()))?;
        require_running(&carrier, &current, &target)?;
        Ok(target)
    })).await
}

pub(in crate::harness::run_loop) async fn select_human_stop(
    inner: &Arc<Inner>,
    source: &HumanInputSource,
) -> Result<ExecutionEndTarget> {
    let (card, worker, source) = (
        inner.card_id.to_string(),
        inner.worker_session_id.clone(),
        source.clone(),
    );
    write_in_tx_typed(inner.repo.as_ref(), move |tx| {
        Box::pin(async move {
            let carrier = current_input_fact_carrier_tx(tx, &card, &worker).await?;
            crate::conversation_inputs::authorize_human_resource_tx(tx, &source, &carrier).await
        })
    })
    .await?;
    select_stop(inner).await
}

fn stop_request(inner: &Inner, target: &ExecutionEndTarget) -> Result<HarnessStopRequest> {
    let (thread_id, turn_id) = target.scope.thread_and_turn();
    Ok(HarnessStopRequest {
        session_id: inner.worker_session_id.clone(),
        card_id: inner.card_id.to_string(),
        track_id: inner.track_id.to_string(),
        thread_id,
        turn_id,
        requested_at_ms: crate::model::now_ms(),
    })
}

async fn start_interrupt(
    inner: &Arc<Inner>,
    target: ExecutionEndTarget,
    intent: HarnessInterruptionIntent,
    authorization: StopAdmission,
    answer: Option<oneshot::Sender<Result<()>>>,
) -> Result<()> {
    if let Err(error) = transition(
        inner,
        Change::Interrupt {
            target: target.clone(),
            intent: intent.clone(),
            admission: authorization,
        },
    )
    .await
    {
        if let Some(answer) = answer {
            let _ = answer.send(Err(error));
            return Ok(());
        }
        return Err(error);
    }
    *inner.issuance_block.lock().await =
        Some("Stop requested; waiting for this execution to confirm its end.".into());
    let admission = Admission {
        inner: inner.clone(),
        target,
        purpose: Purpose::Interrupt { intent },
    };
    tokio::spawn(async move {
        let result = admission
            .inner
            .backend
            .interrupt_execution(
                &admission.target,
                &crate::harness::native_admission::NativePromptAdmission::new(
                    || Ok(()),
                    || Box::pin(admission.admit()),
                ),
            )
            .await;
        if result.is_err() {
            tracing::warn!(
                "original execution control was not confirmed; its durable intent remains held"
            );
        }
        if let Some(answer) = answer {
            let _ = answer.send(result);
        }
    });
    Ok(())
}

pub(in crate::harness::run_loop) async fn manual(
    inner: &Arc<Inner>,
    target: ExecutionEndTarget,
    source: HumanInputSource,
    answer: oneshot::Sender<Result<()>>,
) {
    let intent = stop_request(inner, &target).map(|request| {
        HarnessInterruptionIntent::Control(HarnessControlIntent::UserStopV1(request))
    });
    match intent {
        Ok(intent) => {
            let _ = start_interrupt(
                inner,
                target,
                intent,
                StopAdmission::Human(source),
                Some(answer),
            )
            .await;
        }
        Err(error) => {
            let _ = answer.send(Err(error));
        }
    }
}

pub(in crate::harness::run_loop) async fn timeout(inner: &Arc<Inner>, turn: &str) -> Result<()> {
    let target = select_stop(inner).await?;
    let request = stop_request(inner, &target)?;
    if request.turn_id != turn {
        return Err(CalmError::Conflict("watchdog target changed".into()));
    }
    let elapsed = match &*inner.state.lock().await {
        HarnessState::TurnRunning {
            turn_id,
            watchdog_from,
            ..
        } if turn_id == turn => watchdog_from.elapsed(),
        _ => return Err(CalmError::Conflict("watchdog execution changed".into())),
    };
    let intent =
        HarnessInterruptionIntent::Versioned(HarnessTimeoutIntent::V1(HarnessTimeoutRequest {
            session_id: request.session_id,
            card_id: request.card_id,
            track_id: request.track_id,
            thread_id: request.thread_id,
            turn_id: request.turn_id,
            requested_at_ms: request.requested_at_ms,
            reason: HarnessInterruptionReason::MaxTurnDuration,
            effective_elapsed_ms: elapsed.as_millis().try_into().expect("elapsed fits u64"),
            budget_ms: budget::limit(inner)
                .await
                .as_millis()
                .try_into()
                .expect("budget fits u64"),
        }));
    start_interrupt(inner, target, intent, StopAdmission::KernelBudget, None).await
}

pub(in crate::harness::run_loop) async fn warn(
    inner: &Arc<Inner>,
    turn: &str,
    budget: crate::planner_turn_budget::PlannerTurnBudget,
    start: &QueueEntryId,
) -> Result<()> {
    let target = select_stop(inner).await?;
    let request = stop_request(inner, &target)?;
    if request.turn_id != turn {
        return Err(CalmError::Conflict("budget target changed".into()));
    }
    transition(
        inner,
        Change::Warning {
            target: target.clone(),
            start: start.as_str().into(),
        },
    )
    .await?;
    let notice = crate::planner_turn_budget::BudgetControlNotice::for_turn(
        budget,
        &request.thread_id,
        &request.turn_id,
        start.as_str(),
    )
    .map_err(|error| CalmError::Conflict(error.into()))?;
    let admission = Admission {
        inner: inner.clone(),
        target,
        purpose: Purpose::Warning {
            start: start.as_str().into(),
        },
    };
    tokio::spawn(async move {
        let admit = crate::harness::native_admission::NativePromptAdmission::new(
            || Ok(()),
            || Box::pin(admission.admit()),
        );
        let work = admission.inner.backend.send_budget_notice(
            &notice,
            BudgetControlScope::Canonical(Box::new(admission.target.clone())),
            &admit,
        );
        if !matches!(tokio::time::timeout(Duration::from_secs(2), work).await, Ok(report) if report.outcome().is_ok())
        {
            tracing::debug!(
                "budget control outcome unconfirmed; the once-only attempt remains committed"
            );
        }
    });
    Ok(())
}

pub(in crate::harness::run_loop) async fn failed(inner: &Arc<Inner>, reason: &str) -> Result<()> {
    transition(
        inner,
        Change::Failed {
            reason: reason.into(),
        },
    )
    .await?;
    *inner.interrupt_deadline.lock().await = None;
    *inner.issuance_block.lock().await = Some(
        if reason == calm_types::harness::HARNESS_INTERRUPT_TIMEOUT_REASON {
            calm_types::harness::HARNESS_INTERRUPT_TIMEOUT_MESSAGE.into()
        } else {
            "This execution is held because its runtime reported a system error.".into()
        },
    );
    Ok(())
}
