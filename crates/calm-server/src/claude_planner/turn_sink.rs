//! Role-owned turn state and callbacks for the stream-json protocol loop.
use super::approvals::{Approvals, NO_APPROVAL_SURFACE, write_response};
use super::driver::TerminalEvent;
use super::session::Shared;
use super::spawn::{InstructionsFile, SpawnPurpose};
use super::turn_slot::{TerminalCause, TurnSlot, deadline_reached};
use crate::error::CalmError;
use crate::provider_execution as execution;
use crate::session_projection_repo::{AgentProvider, ThreadAttribution};
use calm_types::worker::WorkerSessionId;
use provider::claude::permission::{CanUseTool, PermissionDecision, permission_response};
use provider::claude::protocol::{ControlResponseOut, ControlResponseOutBody, Record, SystemInit};
use provider::claude::translate::TurnTranslator;
use provider::events::{PlannerEvent, PlannerEventKind};
use std::sync::{Arc, atomic::AtomicBool};
use std::time::Duration;
use uuid::Uuid;
const BIND_TIMEOUT: Duration = Duration::from_secs(5);

#[async_trait::async_trait]
pub(crate) trait TurnSink: Send {
    fn slot(&self) -> Arc<TurnSlot>;
    /// Supply the admitted execution's owner and cancellation guard before reading stdout.
    fn cancellation(&self, claim: &execution::Claim) -> execution::Cancellation;
    /// Observe a matching session init before role validation, even if validation then fails.
    /// Planner uses this ordering to retain the CLI's session for the next resume.
    async fn observe_init(&mut self, init: &SystemInit);
    /// Only validated records reach this callback. While draining, `answering` is false:
    /// record output normally, but do not issue control replies on the closing stdin.
    async fn record(&mut self, line: &str, record: &Record, answering: bool);
    /// Apply role-specific result policy after recording the result and before settlement.
    fn result_event(&self, event: TerminalEvent) -> TerminalEvent;
    /// Package the frozen protocol outcome for the execution owner. The owner sets
    /// `qualified` only when completion evidence qualifies the foreground outcome.
    fn known_end(
        self,
        claim: &execution::Claim,
        event: TerminalEvent,
        cause: Option<TerminalCause>,
        qualified: Arc<AtomicBool>,
    ) -> Option<execution::KnownEnd>;
}

pub(crate) struct PlannerSink {
    pub shared: Arc<Shared>,
    pub slot: Arc<TurnSlot>,
    pub translator: TurnTranslator,
    pub instructions: InstructionsFile,
    pub thread: Uuid,
    pub turn: Uuid,
    pub purpose: SpawnPurpose,
    pub bind: bool,
    pub approvals: Approvals,
    pub compacted: bool,
    pub(in crate::claude_planner) execution: super::session::execution_end::Association,
}

#[async_trait::async_trait]
impl TurnSink for PlannerSink {
    fn slot(&self) -> Arc<TurnSlot> {
        self.slot.clone()
    }
    fn cancellation(&self, claim: &execution::Claim) -> execution::Cancellation {
        let params = &self.shared.params;
        execution::Cancellation {
            owner: params.host.provider_execution.clone(),
            pool: params.repo.sqlite_db().expect("admitted database").clone(),
            engine: params.engine.as_ref().expect("admitted Engine").clone(),
            session: params.worker_session_id.clone(),
            claim: claim.clone(),
            armed: true,
        }
    }
    async fn observe_init(&mut self, init: &SystemInit) {
        // The CLI created (or resumed) this thread's session as soon as it names it, even
        // when a later check fails the turn: bind it, so the next spawn resumes it.
        if init.session_id == self.thread {
            // Publish before the persistence await: init and a durable bind are
            // independent facts, including when this driver is cancelled mid-bind.
            let first_init = self.shared.observe_turn(&self.slot, |facts| {
                !std::mem::replace(&mut facts.named_session, true)
            });
            if first_init == Some(true) && self.bind {
                match self
                    .slot
                    .bounded(BIND_TIMEOUT, self.bind_agent_session())
                    .await
                {
                    Some(persisted) => {
                        self.shared.observe_turn(&self.slot, |facts| {
                            facts.row_bound = persisted;
                        });
                    }
                    None => tracing::warn!(
                        "claude planner: agent session bind cut off by its bound; the next turn retries it"
                    ),
                }
            }
        }
    }
    async fn record(&mut self, line: &str, record: &Record, answering: bool) {
        if let Record::UserReplay { uuid, .. } = record
            && let super::session::execution_end::Association::Work(input) = &self.execution
            && uuid.simple().to_string() == input.proof.attempt().submission_id.as_str()
        {
            let params = &self.shared.params;
            let target = crate::harness::backend::ExecutionEndTarget {
                card_id: params.card_id.clone(),
                track_id: params.track_id.clone(),
                worker_session_id: params.worker_session_id.clone(),
                logical_turn_id: input.logical_turn_id.clone(),
                proof: input.proof.clone(),
                scope: crate::harness::backend::ExecutionEndScope::LocalProcess {
                    thread_id: self.thread,
                    local_turn_id: self.turn,
                },
            };
            match self
                .slot
                .bounded(
                    BIND_TIMEOUT,
                    crate::harness::input_dispatch::acknowledge(
                        params.repo.as_ref(),
                        target,
                        calm_types::runtime::AgentProvider::Claude,
                        crate::harness::backend::InputAcknowledgementBasis::ClaudeUserReplay
                            .as_str(),
                    ),
                )
                .await
            {
                Some(Ok(())) => {}
                Some(Err(error)) => {
                    tracing::warn!(%error, "Claude input acknowledgement retained as uncertain")
                }
                None => tracing::warn!(
                    "Claude input acknowledgement exceeded its bound; retained as uncertain"
                ),
            }
        }
        match record {
            Record::CompactBoundary => self.compacted = true,
            Record::Ignored { kind } => {
                tracing::debug!(kind, "claude planner: ignored stream record")
            }
            Record::ControlRequestIn {
                request_id,
                request,
            } if answering => self.on_control_request(request_id, request).await,
            Record::ControlRequestIn { request_id, .. } => tracing::debug!(
                request_id,
                "claude planner: control request not answered while stopping"
            ),
            Record::ControlCancelRequestIn { request_id } => match &self.approvals {
                Approvals::Held(connection) => connection.gone(request_id),
                Approvals::Refused => {
                    tracing::debug!(request_id, "claude planner: control request cancelled")
                }
            },
            _ => {}
        }
        for event in self
            .translator
            .translate_line(line, record, crate::model::now_ms())
        {
            // A compaction's own usage counts nowhere, like the rest of what it reads (`compact`).
            if self.purpose == SpawnPurpose::Turn {
                if let Some(total) = usage_total(&event) {
                    self.shared.observe_turn(&self.slot, |facts| {
                        facts.total_tokens = Some(total);
                    });
                }
                let _ = self.shared.events.send(event);
            } else if matches!(event.kind, PlannerEventKind::ContextReplaced { .. }) {
                let _ = self.shared.events.send(event);
            }
        }
    }
    fn result_event(&self, event: TerminalEvent) -> TerminalEvent {
        if self.purpose == SpawnPurpose::Compaction {
            super::compact::result_event(event, self.compacted)
        } else {
            event
        }
    }
    fn known_end(
        self,
        claim: &execution::Claim,
        event: TerminalEvent,
        cause: Option<TerminalCause>,
        qualified: Arc<AtomicBool>,
    ) -> Option<execution::KnownEnd> {
        let turn = match &claim.record().purpose {
            execution::Purpose::Turn { turn_id } | execution::Purpose::Compaction { turn_id } => {
                turn_id.clone()
            }
            _ => unreachable!(),
        };
        Some(execution::KnownEnd::ClaudeTurn(Box::new(
            super::execution::End {
                shared: self.shared,
                slot: self.slot,
                translator: self.translator,
                instructions: self.instructions,
                event,
                cause,
                qualified,
                thread: self.thread.to_string(),
                turn,
                approvals: self.approvals,
            },
        )))
    }
}

impl PlannerSink {
    /// Under `ask` a well-formed `can_use_tool` goes to the harness and the read goes on; every other
    /// request is answered here, and a CLI that stopped reading stdin does not hold the turn past
    /// its stop.
    async fn on_control_request(&mut self, request_id: &str, request: &serde_json::Value) {
        let can_use_tool = CanUseTool::from_request(request);
        if let (Approvals::Held(connection), Ok(Some(can_use_tool))) =
            (&self.approvals, &can_use_tool)
        {
            connection.open(&self.slot, request_id, can_use_tool);
            return;
        }
        answer_unattended(
            &self.slot,
            request_id,
            can_use_tool.is_ok_and(|request| request.is_none()),
            NO_APPROVAL_SURFACE,
        )
        .await;
    }

    /// Persist `agent_session_id` through the attribution bind, so a session opened for this row
    /// later resumes. A failed write is logged and `false`: this process resumes from memory and
    /// the next turn retries the bind.
    async fn bind_agent_session(&self) -> bool {
        let params = &self.shared.params;
        #[cfg(feature = "fixtures")]
        crate::test_seams::pause_point("c4-bind-session", &params.worker_session_id).await;
        let id = params.worker_session_id.clone();
        let thread = self.thread.to_string();
        let written = crate::db::write_in_tx_typed(params.repo.as_ref(), move |tx| {
            Box::pin(async move {
                // The bind writes `active_turn_id` too; the harness's snapshot owns that column,
                // so the row's current value is passed through unchanged.
                let row = crate::db::sqlite::session_get_tx(tx, &WorkerSessionId(id.clone()))
                    .await?
                    .ok_or_else(|| CalmError::NotFound(format!("worker session {id}")))?;
                crate::db::sqlite::session_bind_attribution_tx(
                    tx,
                    &id,
                    ThreadAttribution {
                        worker_session_id: id.clone(),
                        provider: AgentProvider::Claude,
                        thread_id: Some(thread.clone()),
                        session_id: Some(thread),
                        active_turn_id: row.active_turn_id,
                    },
                )
                .await?;
                Ok(())
            })
        })
        .await;
        match written {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!(
                    worker_session_id = %params.worker_session_id,
                    %error,
                    "claude planner: agent session id not persisted; the next turn retries it"
                );
                false
            }
        }
    }
}

/// The lifetime total a usage reading carries.
fn usage_total(event: &PlannerEvent) -> Option<i64> {
    match &event.kind {
        PlannerEventKind::TokenUsage { params } => {
            params["tokenUsage"]["total"]["totalTokens"].as_i64()
        }
        _ => None,
    }
}

/// Answer a control request no person sees, unless the turn's stop comes first: a CLI that stopped
/// reading stdin does not hold the turn past its stop.
pub(crate) async fn answer_unattended(
    slot: &TurnSlot,
    request_id: &str,
    other_subtype: bool,
    denial: &str,
) {
    let mut stop_rx = slot.stop_at.subscribe();
    tokio::select! {
        biased;
        _ = deadline_reached(&mut stop_rx, Duration::ZERO) => {
            tracing::debug!(request_id, "claude planner: control answer abandoned at the stop");
        }
        _ = answer_control_request(slot, request_id, other_subtype, denial) => {}
    }
}

/// A `can_use_tool` answered here (one under `never` or `full`, where `--permission-prompts none`
/// should not send it, or one without its recorded shape) is denied with `denial`, which goes back
/// to the model. Any other request gets an error response.
async fn answer_control_request(
    slot: &TurnSlot,
    request_id: &str,
    other_subtype: bool,
    denial: &str,
) {
    let response = if other_subtype {
        ControlResponseOut::new(ControlResponseOutBody::Error {
            request_id: request_id.to_string(),
            error: "unsupported control request".into(),
        })
    } else {
        permission_response(
            request_id,
            &PermissionDecision::Deny {
                message: denial.into(),
            },
        )
    };
    if let Err(error) = write_response(slot, &response).await {
        tracing::warn!(%error, request_id, "claude planner: control response not written");
    }
}
