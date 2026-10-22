//! Objective definitions and their validation.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::check::CheckConfig;
use crate::diagnostic::{Diagnostic, Problem};
use crate::ids::{CheckId, ObjectiveKey};

/// One objective: what to reach, how it is checked, and the soft limits of pursuing it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ObjectiveDefinition {
    pub key: ObjectiveKey,
    pub title: String,
    pub mode: Mode,
    pub checks: Vec<CheckDefinition>,
    pub constraints: Vec<String>,
    pub budget: Budget,
    pub compact: CompactPolicy,
}

/// How an objective stops.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Work until every goal check passes.
    Converge,
    /// Keep improving until `plateau` consecutive settles bring no improvement.
    Loop { plateau: u32 },
}

/// What a check's result means for keeping a revision.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Must pass on every kept revision.
    Invariant,
    /// Must not regress against the baseline.
    Ratchet,
    /// Decides when a converging objective is done.
    Goal,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CheckDefinition {
    pub id: CheckId,
    pub role: Role,
    pub config: CheckConfig,
}

/// Soft limits: exhausting one reminds the planner, it does not stop work.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Budget {
    pub turns: u32,
    pub tokens: u64,
    pub wall_secs: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetLimit {
    Turns,
    Tokens,
    WallSecs,
}

/// Cumulative user-turn usage. Context tokens sum positive end-minus-start visible
/// occupancy deltas; either endpoint missing or invalidated contributes zero.
/// Turns count user turns, elapsed seconds sum their wall time. The engine reads no clock.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub turns: u32,
    pub context_tokens: u64,
    pub elapsed_secs: u64,
}

impl Budget {
    /// The limits `used` has reached, in [`BudgetLimit`] order.
    pub fn exhausted(&self, used: &Usage) -> Vec<BudgetLimit> {
        let reached = [
            (
                BudgetLimit::Turns,
                u64::from(used.turns) >= u64::from(self.turns),
            ),
            (BudgetLimit::Tokens, used.context_tokens >= self.tokens),
            (BudgetLimit::WallSecs, used.elapsed_secs >= self.wall_secs),
        ];
        reached
            .into_iter()
            .filter_map(|(limit, hit)| hit.then_some(limit))
            .collect()
    }
}

impl Usage {
    /// Usage accumulated since `start`.
    pub fn since(&self, start: &Usage) -> Usage {
        Usage {
            turns: self.turns.saturating_sub(start.turns),
            context_tokens: self.context_tokens.saturating_sub(start.context_tokens),
            elapsed_secs: self.elapsed_secs.saturating_sub(start.elapsed_secs),
        }
    }
}

/// When the session's context is compacted for this objective.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactPolicy {
    EverySettle,
    OnClose,
    None,
}

impl ObjectiveDefinition {
    pub fn check(&self, id: &CheckId) -> Option<&CheckDefinition> {
        self.checks.iter().find(|c| &c.id == id)
    }

    pub fn validate(&self) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        if self.key.as_str().trim().is_empty() {
            out.push(Diagnostic::new("key", Problem::EmptyKey));
        }
        if self.title.trim().is_empty() {
            out.push(Diagnostic::new("title", Problem::EmptyTitle));
        }
        if self.checks.is_empty() {
            out.push(Diagnostic::new("checks", Problem::NoChecks));
        }
        let mut seen = BTreeSet::new();
        for check in &self.checks {
            let path = format!("checks.{}", check.id);
            if check.id.as_str().trim().is_empty() {
                out.push(Diagnostic::new(&path, Problem::EmptyCheckId));
            }
            if !seen.insert(&check.id) {
                out.push(Diagnostic::new(&path, Problem::DuplicateCheckId));
            }
            for problem in check.config.validate() {
                out.push(Diagnostic::new(&path, problem));
            }
        }
        let has_role = |role| self.checks.iter().any(|c| c.role == role);
        match self.mode {
            Mode::Converge if !has_role(Role::Goal) => {
                out.push(Diagnostic::new("mode", Problem::ConvergeWithoutGoal));
            }
            Mode::Loop { plateau } => {
                if !has_role(Role::Ratchet) {
                    out.push(Diagnostic::new("mode", Problem::LoopWithoutRatchet));
                }
                if plateau == 0 {
                    out.push(Diagnostic::new("mode.plateau", Problem::ZeroPlateau));
                }
            }
            Mode::Converge => {}
        }
        for (index, constraint) in self.constraints.iter().enumerate() {
            if constraint.trim().is_empty() {
                out.push(Diagnostic::new(
                    format!("constraints.{index}"),
                    Problem::EmptyConstraint,
                ));
            }
        }
        let zero = [
            (BudgetLimit::Turns, "budget.turns", self.budget.turns == 0),
            (
                BudgetLimit::Tokens,
                "budget.tokens",
                self.budget.tokens == 0,
            ),
            (
                BudgetLimit::WallSecs,
                "budget.wall_secs",
                self.budget.wall_secs == 0,
            ),
        ];
        for (limit, path, is_zero) in zero {
            if is_zero {
                out.push(Diagnostic::new(path, Problem::ZeroBudget { limit }));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::{
        Aggregate, CommandParams, Direction, EvaluatorParams, MetricParams, RubricItem,
    };

    fn command(id: &str, role: Role) -> CheckDefinition {
        CheckDefinition {
            id: CheckId::new(id),
            role,
            config: CheckConfig::Command(CommandParams {
                command: "make test".into(),
            }),
        }
    }

    fn objective(mode: Mode, checks: Vec<CheckDefinition>) -> ObjectiveDefinition {
        ObjectiveDefinition {
            key: ObjectiveKey::new("speed"),
            title: "Make it fast".into(),
            mode,
            checks,
            constraints: vec!["no new dependencies".into()],
            budget: Budget {
                turns: 10,
                tokens: 100_000,
                wall_secs: 3_600,
            },
            compact: CompactPolicy::EverySettle,
        }
    }

    fn problems(objective: &ObjectiveDefinition) -> Vec<(String, Problem)> {
        objective
            .validate()
            .into_iter()
            .map(|d| (d.path, d.problem))
            .collect()
    }

    #[test]
    fn a_well_formed_objective_has_no_diagnostics() {
        let converge = objective(Mode::Converge, vec![command("tests", Role::Goal)]);
        assert!(converge.validate().is_empty());
        let looping = objective(
            Mode::Loop { plateau: 3 },
            vec![CheckDefinition {
                id: CheckId::new("latency"),
                role: Role::Ratchet,
                config: CheckConfig::Metric(MetricParams {
                    command: "bench".into(),
                    direction: Direction::Lower,
                    tolerance: Some(0.1),
                }),
            }],
        );
        assert!(looping.validate().is_empty());
    }

    #[test]
    fn reports_empty_and_duplicate_checks() {
        assert_eq!(
            problems(&objective(Mode::Loop { plateau: 2 }, Vec::new())),
            vec![
                ("checks".into(), Problem::NoChecks),
                ("mode".into(), Problem::LoopWithoutRatchet),
            ]
        );
        let dup = objective(
            Mode::Converge,
            vec![
                command("tests", Role::Goal),
                command("tests", Role::Invariant),
            ],
        );
        assert_eq!(
            problems(&dup),
            vec![("checks.tests".into(), Problem::DuplicateCheckId)]
        );
    }

    #[test]
    fn reports_mode_requirements() {
        let no_goal = objective(Mode::Converge, vec![command("tests", Role::Invariant)]);
        assert_eq!(
            problems(&no_goal),
            vec![("mode".into(), Problem::ConvergeWithoutGoal)]
        );
        let flat = objective(Mode::Loop { plateau: 0 }, vec![command("t", Role::Ratchet)]);
        assert_eq!(
            problems(&flat),
            vec![("mode.plateau".into(), Problem::ZeroPlateau)]
        );
    }

    #[test]
    fn reports_kind_problems_under_the_check_path() {
        let zero_samples = CheckDefinition {
            id: CheckId::new("review"),
            role: Role::Goal,
            config: CheckConfig::Evaluator(EvaluatorParams {
                rubric: vec![RubricItem {
                    id: "clarity".into(),
                    criterion: "reads clearly".into(),
                }],
                samples: 0,
                aggregate: Aggregate::Majority,
            }),
        };
        assert_eq!(
            problems(&objective(Mode::Converge, vec![zero_samples])),
            vec![("checks.review".into(), Problem::ZeroSamples)]
        );
    }

    #[test]
    fn reports_blank_text_and_zero_budgets() {
        let mut bad = objective(Mode::Converge, vec![command("tests", Role::Goal)]);
        bad.key = ObjectiveKey::new("");
        bad.title = " ".into();
        bad.constraints.push(String::new());
        bad.budget.tokens = 0;
        assert_eq!(
            problems(&bad),
            vec![
                ("key".into(), Problem::EmptyKey),
                ("title".into(), Problem::EmptyTitle),
                ("constraints.1".into(), Problem::EmptyConstraint),
                (
                    "budget.tokens".into(),
                    Problem::ZeroBudget {
                        limit: BudgetLimit::Tokens
                    }
                ),
            ]
        );
    }

    #[test]
    fn budget_reports_reached_limits() {
        let budget = Budget {
            turns: 5,
            tokens: 1_000,
            wall_secs: 60,
        };
        let used = Usage {
            turns: 5,
            context_tokens: 999,
            elapsed_secs: 61,
        };
        assert_eq!(
            budget.exhausted(&used),
            vec![BudgetLimit::Turns, BudgetLimit::WallSecs]
        );
        let start = Usage {
            turns: 2,
            context_tokens: 0,
            elapsed_secs: 100,
        };
        assert_eq!(
            used.since(&start),
            Usage {
                turns: 3,
                context_tokens: 999,
                elapsed_secs: 0,
            }
        );
    }
}
