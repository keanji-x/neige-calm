//! What the engine consumes ([`Event`]) and what it asks of the host ([`Intent`]).

use serde::{Deserialize, Serialize};

use crate::check::SampleOutput;
use crate::command::CommandCall;
use crate::definition::{BudgetLimit, ObjectiveSpec, Usage};
use crate::diagnostic::Diagnostic;
use crate::ids::{CheckId, EvaluationId, ObjectiveKey, Revision};
use crate::ledger::{Decision, SettleError};
use crate::standard::{Approval, StandardHash};
use crate::verdict::VerdictKind;

/// Something that happened, reported by the host or the planner. Events are the only
/// input of the engine, so a log of them replays to the same state and intents.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "event")]
pub enum Event {
    /// An objective was created or redefined.
    SpecChanged {
        objective: ObjectiveSpec,
    },
    ApprovalGranted {
        objective: ObjectiveKey,
        approval: Approval,
    },
    /// The planner asks to evaluate `revision` against an objective.
    Requested {
        objective: ObjectiveKey,
        revision: Revision,
    },
    /// One sample of one check of `evaluation` finished on `revision`.
    CheckFinished {
        objective: ObjectiveKey,
        evaluation: EvaluationId,
        check: CheckId,
        revision: Revision,
        sample: u32,
        output: SampleOutput,
    },
    /// A new revision was delivered; every open, approved objective evaluates it.
    RevisionDelivered {
        revision: Revision,
    },
    /// The planner settles an objective's ready `evaluation`.
    Settled {
        objective: ObjectiveKey,
        evaluation: EvaluationId,
        decision: Decision,
        note: String,
    },
    Closed {
        objective: ObjectiveKey,
    },
    /// The host finished a turn and has `queue_len` turn inputs waiting.
    HostIdle {
        queue_len: u32,
    },
    /// Cumulative session usage.
    Usage {
        usage: Usage,
    },
}

/// Something the engine asks the host to do.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "intent")]
pub enum Intent {
    /// Run one sample of one check on `revision` and report it as [`Event::CheckFinished`]
    /// of the same `evaluation`.
    RunCheck {
        objective: ObjectiveKey,
        evaluation: EvaluationId,
        check: CheckId,
        revision: Revision,
        sample: u32,
    },
    Execute {
        call: CommandCall,
    },
    Notify {
        notice: Notice,
    },
}

/// A message for the planner.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "notice")]
pub enum Notice {
    DefinitionRejected {
        objective: ObjectiveKey,
        diagnostics: Vec<Diagnostic>,
    },
    /// The objective's current standard needs a human's approval.
    ApprovalRequired {
        objective: ObjectiveKey,
        standard_hash: StandardHash,
    },
    /// The standard changed, so the earlier approval no longer holds.
    ApprovalInvalidated {
        objective: ObjectiveKey,
    },
    /// Every check finished; the planner can settle.
    EvaluationReady {
        objective: ObjectiveKey,
        evaluation: EvaluationId,
        revision: Revision,
        verdict: VerdictKind,
    },
    SettleRefused {
        objective: ObjectiveKey,
        error: SettleError,
    },
    PlateauReached {
        objective: ObjectiveKey,
    },
    ObjectiveDone {
        objective: ObjectiveKey,
    },
    /// A soft budget limit was reached; reported once per limit.
    BudgetExhausted {
        objective: ObjectiveKey,
        limit: BudgetLimit,
    },
    EventIgnored {
        reason: IgnoreReason,
    },
}

/// Why an event changed nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IgnoreReason {
    UnknownObjective,
    /// The objective is done or closed.
    NotOpen,
    /// The approval is for a different standard than the current one.
    StaleApproval,
    UnknownCheck,
    /// The result is for an evaluation or revision that is not running.
    StaleResult,
    SampleOutOfRange,
    DuplicateSample,
    KindMismatch,
    /// The named evaluation is not ready to settle.
    NothingToSettle,
}
