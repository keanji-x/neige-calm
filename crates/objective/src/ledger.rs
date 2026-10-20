//! The ledger of one objective: its baseline and every settled evaluation.

use serde::{Deserialize, Serialize};

use crate::definition::{Mode, ObjectiveDefinition};
use crate::ids::Revision;
use crate::verdict::{Results, Verdict, VerdictKind};

/// The kept revision later candidates are compared with. Each result was produced under the
/// objective's current configuration of its check; see [`Ledger::retain_comparable`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Baseline {
    pub revision: Revision,
    pub results: Results,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// Adopt the candidate as the new baseline.
    Keep,
    /// Record the candidate and leave the baseline where it is.
    Discard,
    /// Adopt the candidate and finish the objective.
    Done,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LedgerEntry {
    pub revision: Revision,
    pub results: Results,
    pub decision: Decision,
    pub note: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettleError {
    /// The verdict is a regression, so the candidate cannot be kept.
    RegressionRefused,
    /// Only a verdict of done can settle as done.
    NotDone,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Ledger {
    /// Absent until the first revision is kept.
    pub baseline: Option<Baseline>,
    pub entries: Vec<LedgerEntry>,
    /// Consecutive settles that brought no improvement.
    pub plateau: u32,
}

impl Ledger {
    pub fn baseline_results(&self) -> Option<&Results> {
        self.baseline.as_ref().map(|b| &b.results)
    }

    /// Keeps only the baseline results that `next` can still compare with: those of checks
    /// whose kind and parameters are the same in `previous` and `next`. A result produced under
    /// another configuration, such as another command or a kind change under the same id, says
    /// nothing about the new check, so that check is judged on its own until a revision is kept
    /// under its new definition. A role change keeps the result: the role decides what a
    /// comparison means, not whether one is possible. Ledger entries keep their results.
    pub fn retain_comparable(
        &mut self,
        previous: &ObjectiveDefinition,
        next: &ObjectiveDefinition,
    ) {
        let Some(baseline) = &mut self.baseline else {
            return;
        };
        baseline
            .results
            .retain(|id, _| match (previous.check(id), next.check(id)) {
                (Some(before), Some(after)) => before.config == after.config,
                _ => false,
            });
    }

    /// Records a settled candidate. Keep and done move the baseline; discard does not.
    /// A settle counts as progress when it improved a check or established the first baseline.
    pub fn settle(&mut self, entry: LedgerEntry, verdict: &Verdict) -> Result<(), SettleError> {
        match entry.decision {
            Decision::Keep | Decision::Done if verdict.refuses_keep() => {
                return Err(SettleError::RegressionRefused);
            }
            Decision::Done if verdict.kind != VerdictKind::Done => {
                return Err(SettleError::NotDone);
            }
            Decision::Keep | Decision::Done | Decision::Discard => {}
        }
        let adopts = entry.decision != Decision::Discard;
        let progressed = adopts && (self.baseline.is_none() || verdict.improved());
        self.plateau = if progressed { 0 } else { self.plateau + 1 };
        if adopts {
            self.baseline = Some(Baseline {
                revision: entry.revision.clone(),
                results: entry.results.clone(),
            });
        }
        self.entries.push(entry);
        Ok(())
    }

    /// Whether a looping objective has gone `plateau` settles without improvement.
    pub fn plateau_reached(&self, mode: &Mode) -> bool {
        match mode {
            Mode::Loop { plateau } => self.plateau >= *plateau,
            Mode::Converge => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::{Comparison, Trend};
    use crate::definition::Role;
    use crate::ids::CheckId;
    use crate::verdict::{CheckReason, Finding};

    fn verdict(kind: VerdictKind, trend: Trend) -> Verdict {
        Verdict {
            kind,
            reasons: vec![CheckReason {
                check: CheckId::new("m"),
                role: Role::Ratchet,
                finding: Finding::Compared(Comparison {
                    trend,
                    passed: true,
                }),
                regresses: kind == VerdictKind::Regression,
            }],
        }
    }

    fn entry(revision: &str, decision: Decision) -> LedgerEntry {
        LedgerEntry {
            revision: Revision::new(revision),
            results: Results::new(),
            decision,
            note: String::new(),
        }
    }

    fn baseline_revision(ledger: &Ledger) -> Option<&str> {
        ledger.baseline.as_ref().map(|b| b.revision.as_str())
    }

    #[test]
    fn keep_moves_the_baseline_and_discard_does_not() {
        let mut ledger = Ledger::default();
        let same = verdict(VerdictKind::Keepable, Trend::Same);
        ledger.settle(entry("r1", Decision::Keep), &same).unwrap();
        assert_eq!(baseline_revision(&ledger), Some("r1"));
        assert_eq!(ledger.plateau, 0, "the first baseline is progress");
        ledger
            .settle(entry("r2", Decision::Discard), &same)
            .unwrap();
        assert_eq!(baseline_revision(&ledger), Some("r1"));
        assert_eq!(ledger.entries.len(), 2);
    }

    #[test]
    fn plateau_counts_settles_without_improvement() {
        let mode = Mode::Loop { plateau: 2 };
        let mut ledger = Ledger::default();
        let better = verdict(VerdictKind::Keepable, Trend::Better);
        let flat = verdict(VerdictKind::Discard, Trend::Same);
        ledger.settle(entry("r1", Decision::Keep), &better).unwrap();
        ledger
            .settle(entry("r2", Decision::Discard), &flat)
            .unwrap();
        assert!(!ledger.plateau_reached(&mode));
        ledger.settle(entry("r3", Decision::Keep), &flat).unwrap();
        assert!(ledger.plateau_reached(&mode));
        ledger.settle(entry("r4", Decision::Keep), &better).unwrap();
        assert_eq!(ledger.plateau, 0);
        assert!(!ledger.plateau_reached(&Mode::Converge));
    }

    #[test]
    fn regressions_cannot_be_kept_and_done_needs_a_done_verdict() {
        let mut ledger = Ledger::default();
        let regression = verdict(VerdictKind::Regression, Trend::Worse);
        assert_eq!(
            ledger.settle(entry("r1", Decision::Keep), &regression),
            Err(SettleError::RegressionRefused)
        );
        assert_eq!(
            ledger.settle(entry("r1", Decision::Done), &regression),
            Err(SettleError::RegressionRefused)
        );
        let keepable = verdict(VerdictKind::Keepable, Trend::Better);
        assert_eq!(
            ledger.settle(entry("r1", Decision::Done), &keepable),
            Err(SettleError::NotDone)
        );
        assert!(ledger.entries.is_empty() && ledger.baseline.is_none());
        ledger
            .settle(entry("r1", Decision::Discard), &regression)
            .unwrap();
        let done = verdict(VerdictKind::Done, Trend::Better);
        ledger.settle(entry("r2", Decision::Done), &done).unwrap();
        assert_eq!(baseline_revision(&ledger), Some("r2"));
    }
}
