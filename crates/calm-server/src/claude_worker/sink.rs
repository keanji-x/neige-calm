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
use std::sync::{Arc, OnceLock, atomic::AtomicBool};
use std::time::Duration;
use uuid::Uuid;

/// The one bound on each write the sink makes while the CLI runs; a CLI that keeps writing does
/// not wait on the port.
const RECORD_TIMEOUT: Duration = Duration::from_secs(5);

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
///
/// Each method holds one fact per turn key and the first write wins: a repeat of the same value
/// for a key (a retry whose earlier attempt committed but reported an error) succeeds without a
/// second fact, and a different value for a key that already has one is refused with a conflict,
/// never overwritten. Init, result and settlement are separate facts; none replaces another.
/// `record_outcome` must agree with an existing result fact or refuse the write. A result write
/// may commit despite returning Err or being cancelled, so completion replays that same value.
#[async_trait::async_trait]
pub(crate) trait WorkerTurnLedger: Send + Sync {
    /// The session a matching `init` named, written before the role check reads it.
    async fn record_init(&self, turn: &WorkerTurnKey, init: &WorkerInitRecord) -> Result<()>;
    /// The outcome a validated `result` gave, written as soon as the sink reads it: before stdin
    /// closes, the CLI exits, Stop or settlement, so before any [`execution::KnownEnd`] exists. A
    /// completion without a known end (the kernel restarted after the result, §3.6) reads this
    /// fact first: present, the turn ended by its result and is not continued.
    async fn record_result(&self, turn: &WorkerTurnKey, result: &WorkerTurnOutcome) -> Result<()>;
    /// The owner's settlement record, written by [`WorkerEnd::settle`] inside `complete`.
    async fn record_outcome(&self, turn: &WorkerTurnKey, record: &WorkerTurnRecord) -> Result<()>;
}

/// The worker's [`TurnSink`]. It holds no session of its own: the turn's session is the
/// protocol profile's ([`super::role::WorkerRole`]), which the shared loop checks every record against.
pub(crate) struct WorkerSink {
    slot: Arc<TurnSlot>,
    owner: execution::ProviderExecution,
    pool: crate::db::sqlite::SqliteDb,
    engine: crate::execution_boot::ExecutionEngine,
    intent_key: String,
    turn: OnceLock<WorkerTurnKey>,
    ledger: Arc<dyn WorkerTurnLedger>,
    /// A validated result, retained even when its durable acknowledgement is uncertain.
    result: Option<WorkerTurnOutcome>,
}

impl WorkerSink {
    pub(crate) fn new(
        slot: Arc<TurnSlot>,
        owner: execution::ProviderExecution,
        pool: crate::db::sqlite::SqliteDb,
        engine: crate::execution_boot::ExecutionEngine,
        // WA2-D will take this from Purpose::WorkerTurn; until then the issuer supplies it.
        intent_key: String,
        ledger: Arc<dyn WorkerTurnLedger>,
    ) -> Self {
        Self {
            slot,
            owner,
            pool,
            engine,
            intent_key,
            turn: OnceLock::new(),
            ledger,
            result: None,
        }
    }

    fn turn(&self) -> &WorkerTurnKey {
        self.turn
            .get()
            .expect("protocol bound the claim before reading")
    }

    /// Stop the turn with `failure`, from a callback that reads output.
    fn fail(&self, failure: String) {
        self.slot.record(TerminalCause::Failed(failure));
        self.slot.arm_stop(tokio::time::Instant::now());
    }

    /// Write a validated result's outcome through the port before the loop settles anything.
    async fn record_result(&mut self, event: &TerminalEvent) {
        let outcome: WorkerTurnOutcome = decide(self.slot.cause().as_ref(), event).into();
        self.result = Some(outcome.clone());
        let written = self
            .slot
            .bounded(
                RECORD_TIMEOUT,
                self.ledger.record_result(self.turn(), &outcome),
            )
            .await;
        let failure = match written {
            Some(Ok(())) => {
                return;
            }
            Some(Err(error)) => error.to_string(),
            None => "cut off by its bound".into(),
        };
        tracing::warn!(
            worker_session_id = %self.turn().worker_session_id,
            %failure,
            "claude worker: result acknowledgement uncertain; stopping the turn"
        );
        let failure = format!("result acknowledgement uncertain: {failure}");
        self.fail(failure);
    }
}

#[async_trait::async_trait]
impl TurnSink for WorkerSink {
    fn slot(&self) -> Arc<TurnSlot> {
        self.slot.clone()
    }
    fn cancellation(&self, claim: &execution::Claim) -> execution::Cancellation {
        let turn = WorkerTurnKey {
            worker_session_id: claim.session().into(),
            intent_key: self.intent_key.clone(),
            execution_id: claim.record().execution_id.clone(),
        };
        self.turn.set(turn).expect("one protocol drive per sink");
        execution::Cancellation {
            owner: self.owner.clone(),
            pool: self.pool.clone(),
            engine: self.engine.clone(),
            session: self.turn().worker_session_id.clone(),
            claim: claim.clone(),
            armed: true,
        }
    }
    /// Unlike the Planner, which resumes from memory and retries the bind, a worker's next turn
    /// is chosen from the port alone: an init that cannot be recorded stops the turn. The CLI
    /// sends `init` only once it has the turn's input (E8), so a transcript may already exist that
    /// the port does not name; how the next turn starts then is the issuing layer's choice (WA2-D).
    /// The shared loop calls this only for an init naming the profile's thread, the one source of
    /// the turn's session.
    async fn observe_init(&mut self, init: &SystemInit) {
        let record = WorkerInitRecord {
            session: init.session_id,
            claude_code_version: init.claude_code_version.clone(),
        };
        let written = self
            .slot
            .bounded(
                RECORD_TIMEOUT,
                self.ledger.record_init(self.turn(), &record),
            )
            .await;
        let failure = match written {
            Some(Ok(())) => return,
            Some(Err(error)) => error.to_string(),
            None => "cut off by its bound".into(),
        };
        tracing::warn!(
            worker_session_id = %self.turn().worker_session_id,
            %failure,
            "claude worker: init not recorded; stopping the turn"
        );
        self.fail(format!("init not recorded: {failure}"));
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
            record => {
                if let Some(event) = TerminalEvent::of_result(record) {
                    self.record_result(&event).await;
                }
            }
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
        _stream_complete: bool,
        qualified: Arc<AtomicBool>,
    ) -> Option<execution::KnownEnd> {
        let result_observed = self.result.is_some();
        let outcome = self
            .result
            .unwrap_or_else(|| decide(cause.as_ref(), &event).into());
        Some(execution::KnownEnd::ClaudeWorkerTurn(Box::new(WorkerEnd {
            outcome,
            result_observed,
            turn: self.turn.into_inner().expect("bound claim"),
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
    /// Replay a read result before recording settlement, including uncertain acknowledgements.
    pub(super) result_observed: bool,
    pub(super) slot: Arc<TurnSlot>,
    pub(super) ledger: Arc<dyn WorkerTurnLedger>,
    pub(super) qualified: Arc<AtomicBool>,
}

impl WorkerEnd {
    /// Record the outcome for exactly the execution `record` names; a write failure is returned,
    /// so `finish` keeps the claim and retries. The foreground outcome qualifies only once
    /// recorded with the Engine's settlement proof, after any observed result is durably replayed.
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
        if self.result_observed {
            self.ledger.record_result(&self.turn, &self.outcome).await?;
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
