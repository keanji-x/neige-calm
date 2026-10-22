//! The reducer: applies one event to the state and reports what occurred.

use std::collections::BTreeSet;
use std::collections::btree_map::Entry;

use super::{Evaluation, Event, IgnoreReason, Notice, ObjectiveState, State, Status};
use crate::check::SampleOutput;
use crate::definition::ObjectiveDefinition;
use crate::ids::{CheckId, EvaluationId, ObjectiveKey, Revision};
use crate::ledger::{Decision, LedgerEntry};
use crate::standard::{Approval, standard_hash};
use crate::trigger::Occurrence;
use crate::verdict::{Results, keepable};

pub(super) fn apply(state: &mut State, event: &Event) -> Vec<Occurrence> {
    match event {
        Event::DefinitionChanged { objective } => define(state, objective),
        Event::ApprovalGranted {
            objective,
            approval,
        } => approve(state, objective, approval),
        Event::Requested {
            objective,
            revision,
        } => request(state, objective, revision),
        Event::CheckFinished {
            objective,
            evaluation,
            check,
            revision,
            sample,
            output,
        } => finish(
            state,
            objective,
            *evaluation,
            check,
            revision,
            *sample,
            output,
        ),
        Event::RevisionDelivered { revision } => deliver(state, revision),
        Event::Settled {
            objective,
            evaluation,
            decision,
            note,
        } => settle(state, objective, *evaluation, *decision, note),
        Event::Closed { objective } => close(state, objective),
        Event::HostIdle { queue_len } => vec![Occurrence::HostIdle {
            queue_len: *queue_len,
        }],
        Event::Usage { usage } => {
            state.usage = *usage;
            vec![Occurrence::UsageReported]
        }
    }
}

fn ignored(reason: IgnoreReason) -> Vec<Occurrence> {
    vec![Occurrence::Notice(Notice::EventIgnored { reason })]
}

/// The open objective `key`, or why the event must be ignored.
fn open<'a>(
    state: &'a mut State,
    key: &ObjectiveKey,
) -> Result<&'a mut ObjectiveState, Vec<Occurrence>> {
    match state.objectives.get_mut(key) {
        None => Err(ignored(IgnoreReason::UnknownObjective)),
        Some(objective) if objective.status != Status::Open => Err(ignored(IgnoreReason::NotOpen)),
        Some(objective) => Ok(objective),
    }
}

fn approval_required(objective: &ObjectiveDefinition) -> Occurrence {
    Occurrence::Notice(Notice::ApprovalRequired {
        objective: objective.key.clone(),
        standard_hash: standard_hash(objective),
    })
}

fn define(state: &mut State, definition: &ObjectiveDefinition) -> Vec<Occurrence> {
    let key = definition.key.clone();
    let diagnostics = definition.validate();
    if !diagnostics.is_empty() {
        return vec![Occurrence::Notice(Notice::DefinitionRejected {
            objective: key,
            diagnostics,
        })];
    }
    let usage = state.usage;
    if let Entry::Vacant(slot) = state.objectives.entry(key.clone()) {
        slot.insert(ObjectiveState {
            definition: definition.clone(),
            approval: None,
            ledger: Default::default(),
            evaluation: None,
            evaluations: 0,
            latest_verdict: None,
            status: Status::Open,
            usage_at_start: usage,
            budget_notified: BTreeSet::new(),
        });
        return vec![approval_required(definition)];
    }
    let objective = match open(state, &key) {
        Ok(objective) => objective,
        Err(ignored) => return ignored,
    };
    let standard_changed = standard_hash(&objective.definition) != standard_hash(definition);
    let previous = std::mem::replace(&mut objective.definition, definition.clone());
    let still_exhausted = definition.budget.exhausted(&objective.used(&usage));
    objective
        .budget_notified
        .retain(|limit| still_exhausted.contains(limit));
    if !standard_changed {
        return Vec::new();
    }
    let was_approved = objective.approval.take().is_some();
    objective.evaluation = None;
    objective.latest_verdict = None;
    objective.ledger.retain_comparable(&previous, definition);
    let mut out = Vec::new();
    if was_approved {
        out.push(Occurrence::Notice(Notice::ApprovalInvalidated {
            objective: key,
        }));
    }
    out.push(approval_required(definition));
    out
}

fn approve(state: &mut State, key: &ObjectiveKey, approval: &Approval) -> Vec<Occurrence> {
    let objective = match open(state, key) {
        Ok(objective) => objective,
        Err(ignored) => return ignored,
    };
    if !approval.is_valid_for(&objective.definition) {
        return ignored(IgnoreReason::StaleApproval);
    }
    objective.approval = Some(approval.clone());
    Vec::new()
}

/// Starts evaluating `revision` unless that revision is already being evaluated.
fn start(objective: &mut ObjectiveState, revision: &Revision) -> Vec<Occurrence> {
    if objective
        .evaluation
        .as_ref()
        .is_some_and(|e| e.revision() == revision)
    {
        return Vec::new();
    }
    let id = EvaluationId::new(objective.evaluations);
    objective.evaluations += 1;
    objective.evaluation = Some(Evaluation::Running {
        id,
        revision: revision.clone(),
        samples: Default::default(),
    });
    vec![Occurrence::EvaluationStarted {
        objective: objective.definition.key.clone(),
    }]
}

fn request(state: &mut State, key: &ObjectiveKey, revision: &Revision) -> Vec<Occurrence> {
    let objective = match open(state, key) {
        Ok(objective) => objective,
        Err(ignored) => return ignored,
    };
    if !objective.is_approved() {
        return vec![approval_required(&objective.definition)];
    }
    start(objective, revision)
}

fn deliver(state: &mut State, revision: &Revision) -> Vec<Occurrence> {
    state
        .objectives
        .values_mut()
        .filter(|o| o.status == Status::Open && o.is_approved())
        .flat_map(|o| start(o, revision))
        .collect()
}

fn finish(
    state: &mut State,
    key: &ObjectiveKey,
    evaluation: EvaluationId,
    check: &CheckId,
    revision: &Revision,
    sample: u32,
    output: &SampleOutput,
) -> Vec<Occurrence> {
    let objective = match open(state, key) {
        Ok(objective) => objective,
        Err(ignored) => return ignored,
    };
    let Some(Evaluation::Running {
        id,
        revision: running,
        samples,
    }) = &mut objective.evaluation
    else {
        return ignored(IgnoreReason::StaleResult);
    };
    let id = *id;
    if id != evaluation || running != revision {
        return ignored(IgnoreReason::StaleResult);
    }
    let Some(config) = objective.definition.check(check).map(|c| &c.config) else {
        return ignored(IgnoreReason::UnknownCheck);
    };
    if sample >= config.samples() {
        return ignored(IgnoreReason::SampleOutOfRange);
    }
    if !config.accepts(output) {
        return ignored(IgnoreReason::KindMismatch);
    }
    let received = samples.entry(check.clone()).or_default();
    if received.contains_key(&sample) {
        return ignored(IgnoreReason::DuplicateSample);
    }
    received.insert(sample, output.clone());

    let checks = &objective.definition.checks;
    let complete = checks
        .iter()
        .all(|c| samples.get(&c.id).map_or(0, |s| s.len()) == c.config.samples() as usize);
    if !complete {
        return Vec::new();
    }
    // Counts and kinds were checked on arrival, so every check combines.
    let results: Results = checks
        .iter()
        .filter_map(|c| {
            let outputs = samples.get(&c.id)?.values().cloned().collect();
            Some((c.id.clone(), c.config.combine(outputs).ok()?))
        })
        .collect();
    let empty = Results::new();
    let baseline = objective.ledger.baseline_results().unwrap_or(&empty);
    let verdict = keepable(&objective.definition, baseline, &results);
    let notice = Notice::EvaluationReady {
        objective: key.clone(),
        evaluation: id,
        revision: revision.clone(),
        verdict: verdict.kind,
    };
    objective.latest_verdict = Some(verdict.clone());
    objective.evaluation = Some(Evaluation::Ready {
        id,
        revision: revision.clone(),
        results,
        verdict,
    });
    vec![Occurrence::Notice(notice)]
}

fn settle(
    state: &mut State,
    key: &ObjectiveKey,
    evaluation: EvaluationId,
    decision: Decision,
    note: &str,
) -> Vec<Occurrence> {
    let usage = state.usage;
    let objective = match open(state, key) {
        Ok(objective) => objective,
        Err(ignored) => return ignored,
    };
    let Some(Evaluation::Ready {
        id,
        revision,
        results,
        verdict,
    }) = &objective.evaluation
    else {
        return ignored(IgnoreReason::NothingToSettle);
    };
    if *id != evaluation {
        return ignored(IgnoreReason::NothingToSettle);
    }
    let entry = LedgerEntry {
        revision: revision.clone(),
        results: results.clone(),
        decision,
        note: note.to_owned(),
    };
    if let Err(error) = objective.ledger.settle(entry, verdict) {
        return vec![Occurrence::Notice(Notice::SettleRefused {
            objective: key.clone(),
            error,
        })];
    }
    objective.evaluation = None;
    if decision == Decision::Done {
        objective.status = Status::Done {
            usage: objective.used(&usage),
        };
    }
    vec![Occurrence::Settled {
        objective: key.clone(),
        decision,
    }]
}

fn close(state: &mut State, key: &ObjectiveKey) -> Vec<Occurrence> {
    match state.objectives.get_mut(key) {
        None => ignored(IgnoreReason::UnknownObjective),
        Some(objective) if matches!(objective.status, Status::Closed { .. }) => {
            ignored(IgnoreReason::NotOpen)
        }
        Some(objective) => {
            objective.status = Status::Closed {
                usage: objective.used(&state.usage),
            };
            objective.evaluation = None;
            vec![Occurrence::Closed {
                objective: key.clone(),
            }]
        }
    }
}
