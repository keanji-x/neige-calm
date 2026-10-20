//! Whether a candidate revision may be kept, judged against the baseline's results.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::check::{CheckOutput, Comparison, Trend};
use crate::definition::{CheckDefinition, Mode, ObjectiveDefinition, Role};
use crate::ids::CheckId;

/// One evaluation's results, by check.
pub type Results = BTreeMap<CheckId, CheckOutput>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerdictKind {
    /// An invariant fails or a ratchet regresses: the engine refuses to keep it.
    Regression,
    /// Loop mode and nothing improved: keeping it is allowed but not advised.
    Discard,
    Keepable,
    /// Converge mode and every goal check passes.
    Done,
}

/// What one check contributed to a verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "finding", content = "comparison")]
pub enum Finding {
    Compared(Comparison),
    /// The baseline has no result for this check; the candidate was judged on its own.
    NoBaseline(Comparison),
    /// The candidate has no result for this check.
    Missing,
    /// A result does not belong to the check's kind.
    KindMismatch,
}

impl Finding {
    fn comparison(&self) -> Option<&Comparison> {
        match self {
            Self::Compared(c) | Self::NoBaseline(c) => Some(c),
            Self::Missing | Self::KindMismatch => None,
        }
    }

    fn passed(&self) -> bool {
        self.comparison().is_some_and(|c| c.passed)
    }

    fn trend(&self) -> Option<Trend> {
        match self {
            Self::Compared(c) => Some(c.trend),
            Self::NoBaseline(_) | Self::Missing | Self::KindMismatch => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckReason {
    pub check: CheckId,
    pub role: Role,
    pub finding: Finding,
    /// This check alone makes the candidate a regression.
    pub regresses: bool,
}

impl CheckReason {
    /// A regression, or a goal that does not pass yet.
    pub fn is_open_blocker(&self) -> bool {
        self.regresses || (self.role == Role::Goal && !self.finding.passed())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verdict {
    pub kind: VerdictKind,
    /// One reason per check, in definition order.
    pub reasons: Vec<CheckReason>,
}

impl Verdict {
    pub fn refuses_keep(&self) -> bool {
        self.kind == VerdictKind::Regression
    }

    /// Whether any check improved against the baseline.
    pub fn improved(&self) -> bool {
        self.reasons
            .iter()
            .any(|r| r.finding.trend() == Some(Trend::Better))
    }

    pub fn open_blockers(&self) -> impl Iterator<Item = &CheckReason> {
        self.reasons.iter().filter(|r| r.is_open_blocker())
    }
}

fn finding(check: &CheckDefinition, baseline: &Results, candidate: &Results) -> Finding {
    let Some(candidate) = candidate.get(&check.id) else {
        return Finding::Missing;
    };
    let compared = match baseline.get(&check.id) {
        Some(baseline) => check
            .config
            .compare(baseline, candidate)
            .map(Finding::Compared),
        None => check
            .config
            .compare(candidate, candidate)
            .map(Finding::NoBaseline),
    };
    compared.unwrap_or(Finding::KindMismatch)
}

fn regresses(role: Role, finding: &Finding) -> bool {
    match role {
        Role::Invariant => !finding.passed(),
        Role::Ratchet => match finding.comparison() {
            Some(c) => matches!(finding, Finding::Compared(_)) && c.trend == Trend::Worse,
            None => true,
        },
        Role::Goal => false,
    }
}

/// Judges `candidate` against `baseline` for `objective`:
/// every invariant must pass and no ratchet may regress, else [`VerdictKind::Regression`];
/// in converge mode, passing every goal check is [`VerdictKind::Done`]; in loop mode, a
/// candidate that improves no check against a present baseline is [`VerdictKind::Discard`].
pub fn keepable(
    objective: &ObjectiveDefinition,
    baseline: &Results,
    candidate: &Results,
) -> Verdict {
    let reasons: Vec<CheckReason> = objective
        .checks
        .iter()
        .map(|check| {
            let finding = finding(check, baseline, candidate);
            CheckReason {
                check: check.id.clone(),
                role: check.role,
                regresses: regresses(check.role, &finding),
                finding,
            }
        })
        .collect();
    let goals_pass = || {
        let mut goals = reasons.iter().filter(|r| r.role == Role::Goal).peekable();
        goals.peek().is_some() && goals.all(|r| r.finding.passed())
    };
    let comparable = reasons.iter().any(|r| r.finding.trend().is_some());
    let improved = reasons
        .iter()
        .any(|r| r.finding.trend() == Some(Trend::Better));
    let kind = if reasons.iter().any(|r| r.regresses) {
        VerdictKind::Regression
    } else {
        match objective.mode {
            Mode::Converge if goals_pass() => VerdictKind::Done,
            Mode::Loop { .. } if comparable && !improved => VerdictKind::Discard,
            Mode::Converge | Mode::Loop { .. } => VerdictKind::Keepable,
        }
    };
    Verdict { kind, reasons }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::{CheckConfig, CommandOutput, CommandParams};

    fn objective_with(roles: &[(&str, Role)], mode: Mode) -> ObjectiveDefinition {
        ObjectiveDefinition {
            key: crate::ids::ObjectiveKey::new("o"),
            title: "t".into(),
            mode,
            checks: roles
                .iter()
                .map(|(id, role)| CheckDefinition {
                    id: CheckId::new(*id),
                    role: *role,
                    config: CheckConfig::Command(CommandParams {
                        command: "true".into(),
                    }),
                })
                .collect(),
            constraints: Vec::new(),
            budget: crate::definition::Budget {
                turns: 1,
                tokens: 1,
                wall_secs: 1,
            },
            compact: crate::definition::CompactPolicy::None,
        }
    }

    fn exits(pairs: &[(&str, i32)]) -> Results {
        pairs
            .iter()
            .map(|(id, code)| {
                (
                    CheckId::new(*id),
                    CheckOutput::Command(CommandOutput { exit_code: *code }),
                )
            })
            .collect()
    }

    #[test]
    fn open_blockers_are_regressions_and_unmet_goals() {
        let objective = objective_with(
            &[("inv", Role::Invariant), ("goal", Role::Goal)],
            Mode::Converge,
        );
        let verdict = keepable(
            &objective,
            &Results::new(),
            &exits(&[("inv", 0), ("goal", 1)]),
        );
        assert_eq!(verdict.kind, VerdictKind::Keepable);
        let open: Vec<&str> = verdict.open_blockers().map(|r| r.check.as_str()).collect();
        assert_eq!(open, vec!["goal"]);
    }

    #[test]
    fn finding_records_missing_baseline_and_mismatch() {
        let objective = objective_with(&[("a", Role::Ratchet)], Mode::Converge);
        let check = &objective.checks[0];
        let no_base = finding(check, &Results::new(), &exits(&[("a", 0)]));
        assert!(matches!(no_base, Finding::NoBaseline(_)));
        let mut wrong = Results::new();
        wrong.insert(
            CheckId::new("a"),
            CheckOutput::Fact(crate::check::FactOutput { holds: true }),
        );
        assert_eq!(
            finding(check, &exits(&[("a", 0)]), &wrong),
            Finding::KindMismatch
        );
        assert_eq!(
            finding(check, &Results::new(), &Results::new()),
            Finding::Missing
        );
    }
}
