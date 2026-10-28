//! The worker's turn sink (§3.2): records the turn through a worker-owned durable port and
//! packages its end for the worker execution owner. No carrier, conversation, approvals or
//! Planner `Shared`; decoding, interrupt, drain and settlement stay in the shared protocol loop.
use super::role::{ReportedSession, WorkerStart};
use crate::claude_planner::driver::{TerminalEvent, decide};
use crate::claude_planner::turn_sink::{TurnSink, answer_unattended};
use crate::claude_planner::turn_slot::{TerminalCause, TurnSlot};
use crate::error::Result;
use crate::provider_execution::{self as execution, CompletionEvidence};
use provider::claude::permission::CanUseTool;
use provider::claude::protocol::{Record, SystemInit};
use provider::claude::translate::TurnOutcome;
use std::sync::{Arc, atomic::AtomicBool};
use std::time::Duration;
use uuid::Uuid;

/// The one bound on recording an init; a CLI that keeps writing does not wait on the port.
const INIT_RECORD_TIMEOUT: Duration = Duration::from_secs(5);

/// The denial a worker's model reads when the CLI still asks for a tool approval (OQ3).
pub(crate) const NO_APPROVER: &str =
    "this task worker has no one to approve tool use; continue without it or report the blocker";

/// Which turn a record belongs to: the worker session, the issuing turn row's key, and the
/// admitted execution.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct WorkerTurnKey {
    pub worker_session_id: String,
    pub intent_key: String,
    pub execution_id: String,
}

/// What a matching `init` reported (WA0 E1/E6): the session the next turn may resume, and the
/// CLI version, recorded and never compared.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct WorkerInitRecord {
    pub session: Uuid,
    pub claude_code_version: String,
}
impl WorkerInitRecord {
    /// A recorded init is what lets a later turn `--resume` (§3.6).
    pub(crate) fn resume(&self) -> WorkerStart {
        WorkerStart::Resume(ReportedSession::recorded(self.session))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum WorkerTurnOutcome {
    Completed,
    Interrupted,
    Failed { message: String },
}
impl From<TurnOutcome> for WorkerTurnOutcome {
    fn from(outcome: TurnOutcome) -> Self {
        match outcome {
            TurnOutcome::Completed => Self::Completed,
            TurnOutcome::Interrupted => Self::Interrupted,
            TurnOutcome::Failed { message } => Self::Failed { message },
        }
    }
}

/// A turn's end as its owner records it. `settled` is the Engine's foreground settlement proof;
/// without it the outcome is the protocol's reading alone.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct WorkerTurnRecord {
    pub outcome: WorkerTurnOutcome,
    pub settled: bool,
}

/// The worker layer's durable turn port. A kernel restart forgets memory, so a resume decision
/// and a turn's outcome rest only on what this port wrote. Each write returns only once durable.
#[async_trait::async_trait]
pub(crate) trait WorkerTurnLedger: Send + Sync {
    async fn record_init(&self, turn: &WorkerTurnKey, init: &WorkerInitRecord) -> Result<()>;
    async fn record_outcome(&self, turn: &WorkerTurnKey, record: &WorkerTurnRecord) -> Result<()>;
}

pub(crate) struct WorkerSink {
    pub slot: Arc<TurnSlot>,
    pub owner: execution::ProviderExecution,
    pub pool: crate::db::sqlite::SqliteDb,
    pub engine: crate::execution_boot::ExecutionEngine,
    pub turn: WorkerTurnKey,
    pub thread: Uuid,
    pub ledger: Arc<dyn WorkerTurnLedger>,
}

#[async_trait::async_trait]
impl TurnSink for WorkerSink {
    fn slot(&self) -> Arc<TurnSlot> {
        self.slot.clone()
    }
    fn cancellation(&self, claim: &execution::Claim) -> execution::Cancellation {
        execution::Cancellation {
            owner: self.owner.clone(),
            pool: self.pool.clone(),
            engine: self.engine.clone(),
            session: self.turn.worker_session_id.clone(),
            claim: claim.clone(),
            armed: true,
        }
    }
    /// Unlike the Planner, which resumes from memory and retries the bind, a worker's next turn
    /// is chosen from the port alone: an init that cannot be recorded stops the turn, so no
    /// transcript grows that a later turn would not resume.
    async fn observe_init(&mut self, init: &SystemInit) {
        if init.session_id != self.thread {
            return;
        }
        let record = WorkerInitRecord {
            session: init.session_id,
            claude_code_version: init.claude_code_version.clone(),
        };
        let written = self
            .slot
            .bounded(
                INIT_RECORD_TIMEOUT,
                self.ledger.record_init(&self.turn, &record),
            )
            .await;
        let failure = match written {
            Some(Ok(())) => return,
            Some(Err(error)) => error.to_string(),
            None => "cut off by its bound".into(),
        };
        tracing::warn!(
            worker_session_id = %self.turn.worker_session_id,
            %failure,
            "claude worker: init not recorded; stopping the turn"
        );
        self.slot.record(TerminalCause::Failed(format!(
            "init not recorded: {failure}"
        )));
        self.slot.arm_stop(tokio::time::Instant::now());
    }
    async fn record(&mut self, _: &str, record: &Record, answering: bool) {
        match record {
            Record::ControlRequestIn {
                request_id,
                request,
            } if answering => {
                let other_subtype = CanUseTool::from_request(request).is_ok_and(|r| r.is_none());
                answer_unattended(&self.slot, request_id, other_subtype, NO_APPROVER).await;
            }
            Record::ControlRequestIn { request_id, .. } => tracing::debug!(
                request_id,
                "claude worker: control request not answered while stopping"
            ),
            Record::Ignored { kind } => {
                tracing::debug!(kind, "claude worker: ignored stream record")
            }
            _ => {}
        }
    }
    fn result_event(&self, event: TerminalEvent) -> TerminalEvent {
        event
    }
    fn known_end(
        self,
        _: &execution::Claim,
        event: TerminalEvent,
        cause: Option<TerminalCause>,
        qualified: Arc<AtomicBool>,
    ) -> Option<execution::KnownEnd> {
        Some(execution::KnownEnd::ClaudeWorkerTurn(Box::new(WorkerEnd {
            outcome: decide(cause.as_ref(), &event).into(),
            turn: self.turn,
            slot: self.slot,
            ledger: self.ledger,
            qualified,
        })))
    }
}

/// A worker turn's frozen protocol outcome, consumed by the worker execution owner's `complete`.
pub struct WorkerEnd {
    pub(super) turn: WorkerTurnKey,
    pub(super) outcome: WorkerTurnOutcome,
    pub(super) slot: Arc<TurnSlot>,
    pub(super) ledger: Arc<dyn WorkerTurnLedger>,
    pub(super) qualified: Arc<AtomicBool>,
}

impl WorkerEnd {
    /// Record the outcome for exactly the execution `record` names; a write failure is returned,
    /// so `finish` keeps the claim and retries. The foreground outcome qualifies only once
    /// recorded with the Engine's settlement proof.
    pub(crate) async fn settle(
        self,
        session: &str,
        record: &execution::Record,
        evidence: &CompletionEvidence,
    ) -> Result<()> {
        if session != self.turn.worker_session_id || record.execution_id != self.turn.execution_id {
            return Err(execution::conflict(
                "Claude worker completion identity changed",
            ));
        }
        let settled = crate::claude_planner::execution::foreground_settled(evidence);
        self.ledger
            .record_outcome(
                &self.turn,
                &WorkerTurnRecord {
                    outcome: self.outcome,
                    settled,
                },
            )
            .await?;
        self.slot.settled.send_replace(true);
        self.qualified
            .store(settled, std::sync::atomic::Ordering::Release);
        Ok(())
    }
}
