//! calm-truth's first-start read (`session_conversation::card_conversation`) names three facts
//! this crate owns; each is pinned to its owner (#2228). The payload's card key is pinned beside
//! the frozen field names, in `the_persisted_payload_field_names_are_frozen`.
use calm_truth::db::sqlite::{PLANNER_START_OPERATION_KIND, TERMINAL_OPERATION_PHASES};
use serde_json::json;

use super::{AppServerInteractKind, Operation, Phase, PhaseTag, operation_result_from};
use crate::routes::conversations_shared::PLANNER_HARNESS_START;

#[test]
fn the_start_kind_is_the_servers() {
    assert_eq!(PLANNER_START_OPERATION_KIND, PLANNER_HARNESS_START);
}

/// One list makes both the exhaustive `match` in `phase` and `ALL_PHASES`, so a new `PhaseTag`
/// fails to compile until it is listed, and once listed it is compared.
macro_rules! phases {
    ($($tag:ident => $phase:expr),* $(,)?) => {
        const ALL_PHASES: &[PhaseTag] = &[$(PhaseTag::$tag),*];
        fn phase(tag: PhaseTag) -> Phase {
            match tag {
                $(PhaseTag::$tag => $phase),*
            }
        }
    };
}

phases! {
    Pending => Phase::Pending,
    TxCommitted => Phase::TxCommitted,
    AppServerInteract => Phase::AppServerInteract {
        kind: AppServerInteractKind::MintAndAwait { thread_id: None },
    },
    SpawnStarted => Phase::SpawnStarted,
    SpawnSucceeded => Phase::SpawnSucceeded,
    Parked => Phase::Parked,
    Succeeded => Phase::Succeeded,
    Compensating => Phase::Compensating,
    Failed => Phase::Failed,
    Stuck => Phase::Stuck {
        reason: "stuck".into(),
        since: 0,
    },
}

/// Terminal means what the operation runtime means by it: the phase carries an outcome.
#[test]
fn the_terminal_phases_are_the_phases_with_an_outcome() {
    let mut terminal: Vec<&str> = ALL_PHASES
        .iter()
        .copied()
        .filter(|&tag| {
            let operation = Operation {
                id: "op".into(),
                operation_key: "op".into(),
                kind: PLANNER_HARNESS_START.into(),
                idempotency_key: None,
                payload_hash: String::new(),
                target_type: "card".into(),
                target_id: None,
                target: json!({}),
                payload: json!({}),
                tx_output: None,
                phase: phase(tag),
                phase_detail: None,
                attempt: 1,
                last_error: None,
                compensation_state: None,
                lease_owner: None,
                lease_until_ms: None,
                spawn_artifacts: None,
                parked_at_ms: None,
                parked_deadline_ms: None,
            };
            assert_eq!(operation.phase.tag(), tag);
            operation_result_from(&operation).unwrap().is_some()
        })
        .map(PhaseTag::as_str)
        .collect();
    let mut named = TERMINAL_OPERATION_PHASES.to_vec();
    terminal.sort_unstable();
    named.sort_unstable();
    assert_eq!(named, terminal);
}
