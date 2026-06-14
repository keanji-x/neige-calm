//! Card-derived decision-write sink for the pre-PR7 MCP tool paths.
//!
//! This module is a structural seam for #679 PR6a-2. The production MCP
//! handlers still derive their persisted actor from `ToolCallIdentity` and
//! call the card-aware inherent methods below. The principal-based
//! `DecisionSink::commit` entry remains inert until PR7 flips authority.

use crate::db::{RouteRepo, write_with_actor_events_typed};
use crate::error::CalmError;
use crate::event::{EditAuthor, Event, EventBus, EventScope};
use crate::ids::{ActorId, CardId, CoveId, WaveId};
use crate::mcp_server::registry::AppContext;
use crate::model::{Card, Wave, WaveLifecycle};
use crate::runtime_repo::RuntimeId;
use crate::state::WriteContext;
use crate::wave_lifecycle::{
    apply_requested_transition_in_tx, auto_promote_draft_in_tx, auto_transition_if_current_in_tx,
};
use crate::wave_report::{WaveReportPayload, persist_report};
use async_trait::async_trait;
use calm_exec::{AgentReactor, DecisionIntent, DecisionSink};
use calm_types::error::CoreError;
use calm_types::observation::Observation;
use calm_types::worker::{Principal, WorkerSessionId};
use std::sync::Arc;

#[derive(Clone)]
pub struct CardDecisionSink {
    repo: Arc<dyn RouteRepo>,
    events: EventBus,
    write: WriteContext,
}

impl CardDecisionSink {
    pub fn from_app_context(ctx: &Arc<AppContext>) -> Self {
        Self {
            repo: Arc::clone(&ctx.repo),
            events: ctx.events.clone(),
            write: ctx.write.clone(),
        }
    }

    pub async fn commit_worker_task_report(
        &self,
        actor: ActorId,
        card_id_str: String,
        event: Event,
    ) -> Result<(), CalmError> {
        let card = self
            .repo
            .card_get(&card_id_str)
            .await
            .map_err(|e| CalmError::Internal(format!("emit: card lookup: {e}")))?
            .ok_or_else(|| {
                CalmError::Internal(format!(
                    "emit: bound card {card_id_str} not found (deleted mid-connection?)"
                ))
            })?;
        let wave = self
            .repo
            .wave_get(card.wave_id.as_str())
            .await
            .map_err(|e| CalmError::Internal(format!("emit: wave lookup: {e}")))?
            .ok_or_else(|| {
                CalmError::Internal(format!(
                    "emit: wave {} for card {} not found",
                    card.wave_id.as_str(),
                    card_id_str
                ))
            })?;

        let scope = EventScope::Card {
            card: CardId::from(card_id_str.clone()),
            wave: wave.id.clone(),
            cove: wave.cove_id.clone(),
        };
        let wave_scope = EventScope::Wave {
            wave: wave.id.clone(),
            cove: wave.cove_id.clone(),
        };
        let wave_id = wave.id.clone();

        write_with_actor_events_typed::<(), _>(
            self.repo.as_ref(),
            None,
            &self.events,
            &self.write,
            move |tx| {
                let event = event.clone();
                let actor = actor.clone();
                let scope = scope.clone();
                let wave_scope = wave_scope.clone();
                let wave_id = wave_id.clone();
                let worker_card_id = card_id_str.clone();
                Box::pin(async move {
                    // Issue #644 PR-B — flip the matching plan-task row INSIDE
                    // the same tx that persists the worker's report event
                    // (design §3): one tx, no event-persisted-but-row-stale
                    // crash window. The flips are guarded
                    // (`status IN ('dispatched','running')`, wave-pinned, and
                    // the done-flip skips gated rows), so a legacy
                    // `calm.task.dispatch` key with no tasks row, an already
                    // terminal row, or a foreign-wave id all no-op. This hook
                    // lives ONLY in the worker-role-gated
                    // `calm.task.complete` / `calm.task.fail` handlers — spec
                    // verdict emissions (`calm.task.verdict`, wave_state.rs)
                    // never run it, so verdicts can never flip rows.
                    let now = crate::model::now_ms();
                    let flip = match &event {
                        Event::TaskCompleted {
                            idempotency_key, ..
                        } => Some((idempotency_key.clone(), true)),
                        Event::TaskFailed {
                            idempotency_key, ..
                        } => Some((idempotency_key.clone(), false)),
                        _ => None,
                    };
                    // Issue #644 PR-C (§3): the `Working → Reviewing`
                    // auto-promotion is SUPPRESSED for a gated task's
                    // success report — the self-report is a claim, not
                    // evidence; the gate-result tx performs the promotion
                    // instead, on ANY gate verdict. Worker `task.failed`
                    // promotes as today (no gate runs on failure), and
                    // legacy keys with no tasks row keep today's behavior.
                    let mut suppress_promotion = false;
                    if let Some((task_id, success)) = flip {
                        // Round-4 review F1 — unstamped-row ownership proof:
                        // the REPORTING card must be the card the task's
                        // worker-spawn operation created (immutable op
                        // target, stamped in the same tx as the card). The
                        // card payload's `idempotency_key` is NOT proof —
                        // payloads are patchable via `PATCH /api/cards/{id}`,
                        // so a forged sibling payload could otherwise steal
                        // the report-beats-running-stamp window. For rows
                        // already stamped, the `worker_card_id = card` guard
                        // inside the flip implies the same binding.
                        let reporter = crate::db::sqlite::TaskReporter::Card {
                            card_id: worker_card_id.as_str(),
                            owns_key: crate::db::sqlite::worker_op_targets_card_tx(
                                tx,
                                &task_id,
                                &worker_card_id,
                            )
                            .await?,
                        };
                        let rows = if success {
                            match crate::db::sqlite::task_report_success_from_worker_tx(
                                tx,
                                &task_id,
                                wave_id.as_str(),
                                reporter,
                                now,
                            )
                            .await?
                            {
                                crate::db::sqlite::SuccessReportFlip::Done => 1,
                                crate::db::sqlite::SuccessReportFlip::Verifying => {
                                    // Gated row handed to the gate runner —
                                    // the gate-result tx promotes (§3).
                                    suppress_promotion = true;
                                    1
                                }
                                crate::db::sqlite::SuccessReportFlip::None => 0,
                            }
                        } else {
                            crate::db::sqlite::task_fail_from_worker_tx(
                                tx,
                                &task_id,
                                wave_id.as_str(),
                                reporter,
                                "worker-reported",
                                now,
                            )
                            .await?
                        };
                        // Round-2 review F3 — disambiguate a 0-row flip before
                        // emitting terminal side effects:
                        //   (i)   no tasks row for the key (legacy
                        //         `calm.task.dispatch` worker) → emit exactly
                        //         as before;
                        //   (iii) row already TERMINAL → duplicate/retried
                        //         report; keep emitting (consumers tolerate
                        //         duplicate task events per key, design §1.3 —
                        //         verdict emissions and report-retry
                        //         idempotency depend on it);
                        //   (iv)  row ACTIVE (`dispatched`/`running` — the
                        //         only states the guarded flip targets) and
                        //         the ownership guard rejected the reporter
                        //         → refuse the whole write: no event, no
                        //         Working → Reviewing transition; the
                        //         caller is told it does not own the task.
                        // Any other 0-row cause keeps today's emit behavior:
                        // (round-6 review) statuses the guarded UPDATE could
                        // never have matched — a legacy `calm.task.dispatch`
                        // key colliding with a still-`pending` plan row (or
                        // a `verifying` row whose gate is in flight) carries
                        // no ownership signal, so the legacy event must keep
                        // persisting with the row left untouched.
                        if rows == 0
                            && let Some(row) = crate::db::sqlite::task_get_tx(tx, &task_id).await?
                        {
                            // Issue #644 PR-C (§3): a duplicate / retried
                            // success report for a GATED row (already
                            // `verifying`, or already terminal via a gate
                            // verdict) must not promote — exactly one
                            // promotion per gated task, in the gate-result
                            // tx.
                            if success && row.gate_json.is_some() {
                                suppress_promotion = true;
                            }
                            if matches!(
                                row.status,
                                crate::model::TaskStatus::Dispatched
                                    | crate::model::TaskStatus::Running
                            ) {
                                let owns = match &row.worker_card_id {
                                    Some(owner) => *owner == worker_card_id,
                                    None => matches!(
                                        reporter,
                                        crate::db::sqlite::TaskReporter::Card {
                                            owns_key: true,
                                            ..
                                        }
                                    ),
                                };
                                if !owns {
                                    return Err(CalmError::Forbidden(format!(
                                        "task {task_id} is not owned by reporting card \
                                         {worker_card_id}; report rejected"
                                    )));
                                }
                            }
                        }
                    }

                    let mut events = vec![(actor, scope, event)];
                    if !suppress_promotion
                        && let Some(auto_events) = auto_transition_if_current_in_tx(
                            tx,
                            &wave_id,
                            crate::model::WaveLifecycle::Working,
                            crate::model::WaveLifecycle::Reviewing,
                            &ActorId::Kernel,
                            Some("[auto] first task report".to_string()),
                        )
                        .await?
                    {
                        events.extend(
                            auto_events
                                .into_iter()
                                .map(|event| (ActorId::Kernel, wave_scope.clone(), event)),
                        );
                    }
                    Ok(((), events))
                })
            },
        )
        .await?;

        Ok(())
    }

    pub async fn commit_spec_verdict(
        &self,
        actor: ActorId,
        card_id_str: String,
        message: String,
        lifecycle: Option<WaveLifecycle>,
        event: Event,
    ) -> Result<(), CalmError> {
        let card = self
            .repo
            .card_get(&card_id_str)
            .await
            .map_err(|e| CalmError::Internal(format!("wave_state: card lookup: {e}")))?
            .ok_or_else(|| {
                CalmError::Internal(format!(
                    "wave_state: bound card {card_id_str} not found (deleted mid-connection?)"
                ))
            })?;
        let wave = self
            .repo
            .wave_get(card.wave_id.as_str())
            .await
            .map_err(|e| CalmError::Internal(format!("wave_state: wave lookup: {e}")))?
            .ok_or_else(|| {
                CalmError::Internal(format!(
                    "wave_state: wave {} for card {} not found",
                    card.wave_id.as_str(),
                    card_id_str
                ))
            })?;
        let scope = EventScope::Wave {
            wave: wave.id.clone(),
            cove: wave.cove_id.clone(),
        };
        let wave_id = wave.id.clone();
        let wave_scope = scope.clone();

        write_with_actor_events_typed::<(), _>(
            self.repo.as_ref(),
            None,
            &self.events,
            &self.write,
            move |tx| {
                let event = event.clone();
                let actor = actor.clone();
                let scope = scope.clone();
                let wave_scope = wave_scope.clone();
                let wave_id = wave_id.clone();
                let message = message.clone();
                Box::pin(async move {
                    let mut events = Vec::new();
                    if let Some(auto_events) = auto_promote_draft_in_tx(tx, &wave_id).await? {
                        events.extend(
                            auto_events
                                .into_iter()
                                .map(|event| (ActorId::Kernel, wave_scope.clone(), event)),
                        );
                    }
                    if let Some(target) = lifecycle
                        && let Some(lifecycle_events) = apply_requested_transition_in_tx(
                            tx,
                            &wave_id,
                            target,
                            &actor,
                            message.clone(),
                        )
                        .await?
                    {
                        events.extend(
                            lifecycle_events
                                .into_iter()
                                .map(|event| (actor.clone(), wave_scope.clone(), event)),
                        );
                    }
                    events.push((actor, scope, event));
                    Ok(((), events))
                })
            },
        )
        .await?;

        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn commit_report_write(
        &self,
        actor: ActorId,
        wave: Wave,
        report_card: Card,
        current_payload: WaveReportPayload,
        next: WaveReportPayload,
        agent_message: String,
        lifecycle: Option<WaveLifecycle>,
    ) -> Result<Card, CalmError> {
        persist_report(
            self.repo.as_ref(),
            &self.events,
            &self.write,
            actor,
            EditAuthor::Spec,
            wave,
            report_card,
            current_payload,
            next,
            Some(agent_message),
            lifecycle,
            true,
        )
        .await
    }
}

#[async_trait]
impl DecisionSink for CardDecisionSink {
    async fn commit(
        &self,
        _principal: &Principal,
        _intent: DecisionIntent,
    ) -> Result<(), CoreError> {
        Err(CoreError::Internal(
            "CardDecisionSink production path uses the card-aware methods; principal commit lands with PR7"
                .into(),
        ))
    }
}

/// Structural-only spec-harness reactor for #679 PR6a-2.
///
/// No `worker_sessions` row backs a spec harness yet, and nothing reads this
/// principal until PR7. The live run loop still delivers observations through
/// the existing turn queue and is deliberately not wired to this stub.
#[derive(Clone, Debug)]
pub struct SpecHarnessAgentReactor {
    runtime_id: RuntimeId,
    wave_id: WaveId,
    cove_id: CoveId,
}

impl SpecHarnessAgentReactor {
    pub fn new(runtime_id: RuntimeId, wave_id: WaveId, cove_id: CoveId) -> Self {
        Self {
            runtime_id,
            wave_id,
            cove_id,
        }
    }
}

#[async_trait]
impl AgentReactor for SpecHarnessAgentReactor {
    fn principal(&self) -> Principal {
        Principal::Agent {
            session_id: WorkerSessionId::from(self.runtime_id.clone()),
            wave_id: self.wave_id.clone(),
            cove_id: self.cove_id.clone(),
        }
    }

    async fn react(&self, _observation: &Observation) -> Result<Vec<DecisionIntent>, CoreError> {
        Ok(vec![])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn spec_harness_agent_reactor_is_inert_and_shapes_principal() {
        let reactor = SpecHarnessAgentReactor::new(
            "runtime-1".to_string(),
            WaveId::from("wave-1"),
            CoveId::from("cove-1"),
        );

        assert_eq!(
            reactor.principal(),
            Principal::Agent {
                session_id: WorkerSessionId::from("runtime-1"),
                wave_id: WaveId::from("wave-1"),
                cove_id: CoveId::from("cove-1"),
            }
        );

        let intents = reactor
            .react(&Observation::WaveGoal {
                text: "goal".into(),
            })
            .await
            .expect("react succeeds");
        assert!(intents.is_empty());
    }
}
