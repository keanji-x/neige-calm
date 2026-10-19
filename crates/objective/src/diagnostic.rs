//! Validation diagnostics shared by objective definitions and check kinds.

use serde::{Deserialize, Serialize};

use crate::definition::BudgetLimit;

/// One validation problem, located by a dotted path such as `checks.lint.samples`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub path: String,
    pub problem: Problem,
}

impl Diagnostic {
    pub fn new(path: impl Into<String>, problem: Problem) -> Self {
        Self {
            path: path.into(),
            problem,
        }
    }
}

/// The closed set of problems validation reports.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "code")]
pub enum Problem {
    EmptyKey,
    EmptyTitle,
    NoChecks,
    EmptyCheckId,
    DuplicateCheckId,
    /// Converge mode needs a goal check to decide when it is done.
    ConvergeWithoutGoal,
    /// Loop mode needs a ratchet check to measure improvement against.
    LoopWithoutRatchet,
    ZeroPlateau,
    ZeroBudget {
        limit: BudgetLimit,
    },
    EmptyConstraint,
    EmptyCommand,
    InvalidTolerance,
    ZeroSamples,
    EmptyRubric,
    EmptyRubricItem,
    DuplicateRubricItem {
        item: String,
    },
    EmptyFactSubject,
}
