//! Commands the engine asks the host to execute, and when the host may execute them.

use serde::{Deserialize, Serialize};

use crate::definition::{CheckSpec, Mode, ObjectiveSpec};
use crate::ids::{CheckId, ObjectiveKey, Revision};
use crate::ledger::{Decision, Ledger};
use crate::standard::{StandardHash, standard_hash};
use crate::verdict::Verdict;

/// Who a command acts on, which decides who must allow it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Authority {
    /// The planner's own session.
    OwnSession,
    /// The shared workspace.
    Workspace,
    /// Anything outside Neige; needs the user's approval first.
    External,
}

impl Authority {
    pub fn requires_user_approval(self) -> bool {
        self == Self::External
    }
}

/// The host's state as seen when a command is admitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostState {
    pub idle: bool,
    /// Turn inputs waiting for the host.
    pub queue_len: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeferReason {
    HostBusy,
    QueueNotEmpty,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefuseReason {
    UserApprovalRequired,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "admission", content = "reason")]
pub enum Admission {
    Ready,
    Defer(DeferReason),
    Refuse(RefuseReason),
}

pub trait Command {
    const NAME: &'static str;

    fn authority() -> Authority;

    /// Whether the host may execute the command now.
    fn admission(&self, host: &HostState) -> Admission;

    /// A key under which the host executes the command at most once.
    fn idempotency_key(&self) -> String;
}

/// Admits `command`: a command with external authority is refused until the user has
/// approved it, whatever its own admission says.
pub fn admit<C: Command>(command: &C, host: &HostState, user_approved: bool) -> Admission {
    if C::authority().requires_user_approval() && !user_approved {
        return Admission::Refuse(RefuseReason::UserApprovalRequired);
    }
    command.admission(host)
}

/// Compacts the planner's own context, replacing it with a brief of its objectives.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Compact {
    /// Position of this compaction in the session; it makes the idempotency key.
    pub sequence: u64,
    pub brief: CompactBrief,
}

impl Command for Compact {
    const NAME: &'static str = "compact";

    fn authority() -> Authority {
        Authority::OwnSession
    }

    fn admission(&self, host: &HostState) -> Admission {
        if !host.idle {
            Admission::Defer(DeferReason::HostBusy)
        } else if host.queue_len > 0 {
            Admission::Defer(DeferReason::QueueNotEmpty)
        } else {
            Admission::Ready
        }
    }

    fn idempotency_key(&self) -> String {
        format!("{}:{}", Self::NAME, self.sequence)
    }
}

/// Every command the engine can ask for.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "command", content = "call")]
pub enum CommandCall {
    Compact(Compact),
}

impl CommandCall {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Compact(_) => Compact::NAME,
        }
    }

    pub fn authority(&self) -> Authority {
        match self {
            Self::Compact(_) => Compact::authority(),
        }
    }

    pub fn idempotency_key(&self) -> String {
        match self {
            Self::Compact(c) => c.idempotency_key(),
        }
    }
}

/// What survives a compaction. It is built only from objective definitions, ledgers and
/// verdicts, so logs, diffs and reasoning cannot reach it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CompactBrief {
    pub objectives: Vec<ObjectiveBrief>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryBrief {
    pub revision: Revision,
    pub decision: Decision,
    pub note: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ObjectiveBrief {
    pub key: ObjectiveKey,
    pub title: String,
    pub standard_hash: StandardHash,
    pub mode: Mode,
    /// The standard's check definitions: id, role, kind and parameters, such as commands,
    /// metric directions and tolerances, rubrics and fact subjects.
    pub checks: Vec<CheckSpec>,
    pub constraints: Vec<String>,
    pub baseline: Option<Revision>,
    pub ledger: Vec<EntryBrief>,
    /// Checks that regressed or goals not yet met in the latest verdict.
    pub open_blockers: Vec<CheckId>,
}

impl ObjectiveBrief {
    pub fn new(objective: &ObjectiveSpec, ledger: &Ledger, latest: Option<&Verdict>) -> Self {
        Self {
            key: objective.key.clone(),
            title: objective.title.clone(),
            standard_hash: standard_hash(objective),
            mode: objective.mode,
            checks: objective.checks.clone(),
            constraints: objective.constraints.clone(),
            baseline: ledger.baseline.as_ref().map(|b| b.revision.clone()),
            ledger: ledger
                .entries
                .iter()
                .map(|e| EntryBrief {
                    revision: e.revision.clone(),
                    decision: e.decision,
                    note: e.note.clone(),
                })
                .collect(),
            open_blockers: latest
                .map(|v| v.open_blockers().map(|r| r.check.clone()).collect())
                .unwrap_or_default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::{
        Aggregate, CheckConfig, CheckOutput, CommandOutput, CommandParams, Direction,
        EvaluatorParams, Fact, FactParams, MetricOutput, MetricParams, RubricItem,
    };
    use crate::definition::{Budget, CompactPolicy, Role};
    use crate::ledger::LedgerEntry;
    use crate::verdict::{Results, keepable};

    struct Publish;

    impl Command for Publish {
        const NAME: &'static str = "publish";

        fn authority() -> Authority {
            Authority::External
        }

        fn admission(&self, _: &HostState) -> Admission {
            Admission::Ready
        }

        fn idempotency_key(&self) -> String {
            Self::NAME.into()
        }
    }

    const IDLE: HostState = HostState {
        idle: true,
        queue_len: 0,
    };

    fn compact() -> Compact {
        Compact {
            sequence: 3,
            brief: CompactBrief {
                objectives: Vec::new(),
            },
        }
    }

    #[test]
    fn compact_is_ready_only_when_idle_with_an_empty_queue() {
        let c = compact();
        assert_eq!(admit(&c, &IDLE, false), Admission::Ready);
        let busy = HostState {
            idle: false,
            queue_len: 0,
        };
        assert_eq!(
            admit(&c, &busy, false),
            Admission::Defer(DeferReason::HostBusy)
        );
        let queued = HostState {
            idle: true,
            queue_len: 1,
        };
        assert_eq!(
            admit(&c, &queued, false),
            Admission::Defer(DeferReason::QueueNotEmpty)
        );
        assert_eq!(Compact::authority(), Authority::OwnSession);
        let call = CommandCall::Compact(c);
        assert_eq!(call.idempotency_key(), "compact:3");
        assert_eq!(call.name(), "compact");
        assert_eq!(call.authority(), Authority::OwnSession);
    }

    #[test]
    fn external_commands_need_user_approval() {
        assert_eq!(
            admit(&Publish, &IDLE, false),
            Admission::Refuse(RefuseReason::UserApprovalRequired)
        );
        assert_eq!(admit(&Publish, &IDLE, true), Admission::Ready);
    }

    #[test]
    fn brief_carries_standard_ledger_and_open_blockers() {
        let objective = ObjectiveSpec {
            key: ObjectiveKey::new("green"),
            title: "Tests pass".into(),
            mode: Mode::Converge,
            checks: vec![CheckSpec {
                id: CheckId::new("tests"),
                role: Role::Goal,
                config: CheckConfig::Command(CommandParams {
                    command: "cargo test".into(),
                }),
            }],
            constraints: vec!["keep the API".into()],
            budget: Budget {
                turns: 1,
                tokens: 1,
                wall_secs: 1,
            },
            compact: CompactPolicy::EverySettle,
        };
        let failing: Results = [(
            CheckId::new("tests"),
            CheckOutput::Command(CommandOutput { exit_code: 1 }),
        )]
        .into();
        let verdict = keepable(&objective, &Results::new(), &failing);
        let mut ledger = Ledger::default();
        let entry = LedgerEntry {
            revision: Revision::new("r1"),
            results: failing,
            decision: Decision::Keep,
            note: "starting point".into(),
        };
        ledger.settle(entry, &verdict).unwrap();
        let brief = ObjectiveBrief::new(&objective, &ledger, Some(&verdict));
        assert_eq!(brief.standard_hash, standard_hash(&objective));
        assert_eq!(brief.baseline, Some(Revision::new("r1")));
        assert_eq!(brief.ledger[0].note, "starting point");
        assert_eq!(brief.checks, objective.checks);
        assert_eq!(brief.open_blockers, vec![CheckId::new("tests")]);
        let without = ObjectiveBrief::new(&objective, &ledger, None);
        assert!(without.open_blockers.is_empty());
    }

    #[test]
    fn brief_carries_every_check_definition_but_no_outputs() {
        let check = |id: &str, config| CheckSpec {
            id: CheckId::new(id),
            role: Role::Ratchet,
            config,
        };
        let objective = ObjectiveSpec {
            key: ObjectiveKey::new("quality"),
            title: "Better code".into(),
            mode: Mode::Loop { plateau: 2 },
            checks: vec![
                check(
                    "tests",
                    CheckConfig::Command(CommandParams {
                        command: "cargo test".into(),
                    }),
                ),
                check(
                    "latency",
                    CheckConfig::Metric(MetricParams {
                        command: "bench".into(),
                        direction: Direction::Lower,
                        tolerance: Some(0.5),
                    }),
                ),
                check(
                    "review",
                    CheckConfig::Evaluator(EvaluatorParams {
                        rubric: vec![RubricItem {
                            id: "clarity".into(),
                            criterion: "reads clearly".into(),
                        }],
                        samples: 3,
                        aggregate: Aggregate::Median,
                    }),
                ),
                check(
                    "merged",
                    CheckConfig::Fact(FactParams {
                        fact: Fact::PrMerged,
                        subject: "#42".into(),
                    }),
                ),
            ],
            constraints: Vec::new(),
            budget: Budget {
                turns: 1,
                tokens: 1,
                wall_secs: 1,
            },
            compact: CompactPolicy::EverySettle,
        };
        let results: Results = [
            (
                CheckId::new("tests"),
                CheckOutput::Command(CommandOutput { exit_code: 0 }),
            ),
            (
                CheckId::new("latency"),
                CheckOutput::Metric(MetricOutput { value: 7.0 }),
            ),
        ]
        .into();
        let verdict = keepable(&objective, &Results::new(), &results);
        let mut ledger = Ledger::default();
        let entry = LedgerEntry {
            revision: Revision::new("r1"),
            results,
            decision: Decision::Discard,
            note: "measured".into(),
        };
        ledger.settle(entry, &verdict).unwrap();
        let brief = ObjectiveBrief::new(&objective, &ledger, Some(&verdict));
        let json = serde_json::to_value(&brief).unwrap();
        let params = |index: usize| json["checks"][index]["config"]["params"].clone();
        assert_eq!(params(0)["command"], "cargo test");
        assert_eq!(params(1)["command"], "bench");
        assert_eq!(params(1)["direction"], "lower");
        assert_eq!(params(1)["tolerance"], 0.5);
        assert_eq!(params(2)["rubric"][0]["criterion"], "reads clearly");
        assert_eq!(params(2)["samples"], 3);
        assert_eq!(params(2)["aggregate"], "median");
        assert_eq!(params(3)["fact"], "pr_merged");
        assert_eq!(params(3)["subject"], "#42");
        let text = json.to_string();
        for leaked in ["results", "output", "exit_code", "value", "finding"] {
            assert!(!text.contains(leaked), "the brief carries {leaked}: {text}");
        }
    }
}
