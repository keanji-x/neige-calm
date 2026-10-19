//! Trigger rules: what the host is asked to do after something occurred.
//!
//! - an evaluation started: run every sample of every check;
//! - a settle under [`CompactPolicy::EverySettle`], or a close under
//!   [`CompactPolicy::OnClose`]: a compaction becomes pending;
//! - the host is idle and a compaction is pending: execute it once it is admitted;
//! - a settle that reaches the loop plateau, or a done settle: notify;
//! - a soft budget limit is reached: notify once per limit;
//! - anything the reducer reports as a notice: forward it.

use crate::command::{
    Admission, CommandCall, Compact, CompactBrief, HostState, ObjectiveBrief, admit,
};
use crate::definition::CompactPolicy;
use crate::engine::{Evaluation, Intent, Notice, State, Status};
use crate::ids::ObjectiveKey;
use crate::ledger::Decision;

/// What the reducer observed while applying one event.
#[derive(Clone, Debug, PartialEq)]
pub enum Occurrence {
    Notice(Notice),
    EvaluationStarted {
        objective: ObjectiveKey,
    },
    Settled {
        objective: ObjectiveKey,
        decision: Decision,
    },
    Closed {
        objective: ObjectiveKey,
    },
    HostIdle {
        queue_len: u32,
    },
    UsageReported,
}

/// Applies the trigger rules to `occurrences`, in order.
pub fn fire(state: &mut State, occurrences: Vec<Occurrence>) -> Vec<Intent> {
    let mut intents = Vec::new();
    for occurrence in occurrences {
        match occurrence {
            Occurrence::Notice(notice) => intents.push(Intent::Notify { notice }),
            Occurrence::EvaluationStarted { objective } => {
                run_checks(state, &objective, &mut intents)
            }
            Occurrence::Settled {
                objective,
                decision,
            } => after_settle(state, objective, decision, &mut intents),
            Occurrence::Closed { objective } => {
                if compact_policy(state, &objective) == Some(CompactPolicy::OnClose) {
                    state.pending_compact = true;
                }
            }
            Occurrence::HostIdle { queue_len } => compact_when_idle(state, queue_len, &mut intents),
            Occurrence::UsageReported => remind_budgets(state, &mut intents),
        }
    }
    intents
}

fn compact_policy(state: &State, key: &ObjectiveKey) -> Option<CompactPolicy> {
    state.objectives.get(key).map(|o| o.definition.compact)
}

fn run_checks(state: &State, key: &ObjectiveKey, intents: &mut Vec<Intent>) {
    let Some(objective) = state.objectives.get(key) else {
        return;
    };
    let Some(Evaluation::Running { id, revision, .. }) = &objective.evaluation else {
        return;
    };
    for check in &objective.definition.checks {
        for sample in 0..check.config.samples() {
            intents.push(Intent::RunCheck {
                objective: key.clone(),
                evaluation: *id,
                check: check.id.clone(),
                revision: revision.clone(),
                sample,
            });
        }
    }
}

fn after_settle(
    state: &mut State,
    key: ObjectiveKey,
    decision: Decision,
    intents: &mut Vec<Intent>,
) {
    let Some(objective) = state.objectives.get(&key) else {
        return;
    };
    if objective.ledger.plateau_reached(&objective.definition.mode) {
        intents.push(Intent::Notify {
            notice: Notice::PlateauReached {
                objective: key.clone(),
            },
        });
    }
    if objective.definition.compact == CompactPolicy::EverySettle {
        state.pending_compact = true;
    }
    if decision == Decision::Done {
        intents.push(Intent::Notify {
            notice: Notice::ObjectiveDone { objective: key },
        });
    }
}

fn compact_when_idle(state: &mut State, queue_len: u32, intents: &mut Vec<Intent>) {
    if !state.pending_compact {
        return;
    }
    let compact = Compact {
        sequence: state.compactions,
        brief: CompactBrief {
            objectives: state
                .objectives
                .values()
                .map(|o| ObjectiveBrief::new(&o.definition, &o.ledger, o.latest_verdict.as_ref()))
                .collect(),
        },
    };
    let host = HostState {
        idle: true,
        queue_len,
    };
    // Compaction acts on the planner's own session, which needs no user approval.
    match admit(&compact, &host, false) {
        Admission::Ready => {
            state.pending_compact = false;
            state.compactions += 1;
            intents.push(Intent::Execute {
                call: CommandCall::Compact(compact),
            });
        }
        Admission::Defer(_) | Admission::Refuse(_) => {}
    }
}

fn remind_budgets(state: &mut State, intents: &mut Vec<Intent>) {
    let usage = state.usage;
    for (key, objective) in &mut state.objectives {
        if objective.status != Status::Open {
            continue;
        }
        let used = usage.since(&objective.usage_at_start);
        for limit in objective.definition.budget.exhausted(&used) {
            if objective.budget_notified.insert(limit) {
                intents.push(Intent::Notify {
                    notice: Notice::BudgetExhausted {
                        objective: key.clone(),
                        limit,
                    },
                });
            }
        }
    }
}
