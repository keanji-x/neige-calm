//! The engine: a pure, deterministic reaction of the state to one event.
//!
//! [`react`] first applies the event to a copy of the state (the reducer), which yields
//! [`crate::trigger::Occurrence`]s; the trigger rules then map those to intents. Nothing here
//! performs IO or reads a clock.

/// Persisted Event serde contract; bump when that shape changes.
pub const EVENT_VERSION: u32 = 1;
/// Persisted State/reaction contract; bump on shape or react semantics changes.
pub const STATE_VERSION: u32 = 1;

mod event;
mod reduce;

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::check::SampleOutput;
use crate::definition::{BudgetLimit, ObjectiveSpec, Usage};
use crate::ids::{CheckId, EvaluationId, ObjectiveKey, Revision};
use crate::ledger::Ledger;
use crate::standard::Approval;
use crate::trigger;
use crate::verdict::{Results, Verdict};

pub use event::{Event, IgnoreReason, Intent, Notice};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct State {
    pub objectives: BTreeMap<ObjectiveKey, ObjectiveState>,
    /// The latest cumulative usage the host reported.
    pub usage: Usage,
    /// A compaction is waiting for the host to become idle.
    pub pending_compact: bool,
    /// Compactions executed so far; the next one's sequence number.
    pub compactions: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Open,
    Done,
    Closed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ObjectiveState {
    pub definition: ObjectiveSpec,
    /// The latest approval; it counts only while it matches the current standard.
    pub approval: Option<Approval>,
    pub ledger: Ledger,
    pub evaluation: Option<Evaluation>,
    /// Evaluations started so far; the next one's sequence number. It never goes back, so
    /// an [`EvaluationId`] names one evaluation for the objective's whole life.
    pub evaluations: u64,
    /// The verdict of the latest finished evaluation of the current standard.
    pub latest_verdict: Option<Verdict>,
    pub status: Status,
    /// Session usage when the objective was created; its budget counts from here.
    pub usage_at_start: Usage,
    /// Budget limits already reported as exhausted.
    pub budget_notified: BTreeSet<BudgetLimit>,
}

impl ObjectiveState {
    pub fn is_approved(&self) -> bool {
        self.approval
            .as_ref()
            .is_some_and(|a| a.is_valid_for(&self.definition))
    }
}

/// The evaluation of one revision. Externally tagged: an internally tagged enum buffers
/// its content, and buffered content cannot restore the integer sample keys.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Evaluation {
    /// Waiting for check samples, collected by check and sample index.
    Running {
        id: EvaluationId,
        revision: Revision,
        samples: BTreeMap<CheckId, BTreeMap<u32, SampleOutput>>,
    },
    /// Every sample arrived and the verdict is known; waiting to be settled.
    Ready {
        id: EvaluationId,
        revision: Revision,
        results: Results,
        verdict: Verdict,
    },
}

impl Evaluation {
    pub fn id(&self) -> EvaluationId {
        match self {
            Self::Running { id, .. } | Self::Ready { id, .. } => *id,
        }
    }

    pub fn revision(&self) -> &Revision {
        match self {
            Self::Running { revision, .. } | Self::Ready { revision, .. } => revision,
        }
    }
}

/// Reacts to one event: returns the next state and the intents for the host.
/// The same state and event always give the same result.
pub fn react(state: &State, event: &Event) -> (State, Vec<Intent>) {
    let mut next = state.clone();
    let occurrences = reduce::apply(&mut next, event);
    let intents = trigger::fire(&mut next, occurrences);
    (next, intents)
}
