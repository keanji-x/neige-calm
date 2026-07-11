//! Issue #679 PR0-B — data-driven golden for the wave lifecycle FSM edge table.
//!
//! `tests/goldens/wave_fsm_edges.json` enumerates the FULL decision space of
//! `wave_lifecycle::validate_transition`: 9 from-states × 9 to-states × 4
//! actor kinds = 324 rows, each mapped to `"ok"` / `"illegal_edge"` /
//! `"not_authorized"`. The file was generated from the implementation and
//! hand-checked against the rule table in `wave_lifecycle.rs` docs (40 `ok`
//! rows = 22 distinct legal edges [incl. #741-4 dead-root draft→failed +
//! planning→failed] + 9×2 same-state idempotent shortcuts;
//! Worker/Other are denied everywhere). It is now the contract: any change
//! to the validator's answer for any cell fails this test and requires a
//! conscious golden update.
//!
//! Regenerate after an *intentional* FSM change with:
//!
//! ```sh
//! REGEN_WAVE_FSM_GOLDEN=1 cargo test -p calm-server --test wave_suite wave_fsm_golden::
//! ```
//!
//! then diff + hand-verify the result before committing.

use calm_server::ids::{ActorId, CardId};
use calm_server::model::WaveLifecycle;
use calm_server::wave_lifecycle::{ActorKind, TransitionError, validate_transition};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const ALL_STATES: [WaveLifecycle; 9] = [
    WaveLifecycle::Draft,
    WaveLifecycle::Planning,
    WaveLifecycle::Dispatching,
    WaveLifecycle::Working,
    WaveLifecycle::Blocked,
    WaveLifecycle::Reviewing,
    WaveLifecycle::Done,
    WaveLifecycle::Canceled,
    WaveLifecycle::Failed,
];

/// Actor-kind labels as persisted in the golden, in row order.
const ALL_KINDS: [&str; 4] = ["user", "spec_agent", "worker", "other"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct EdgeRow {
    from: WaveLifecycle,
    to: WaveLifecycle,
    actor_kind: String,
    outcome: String,
}

fn golden_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/goldens/wave_fsm_edges.json")
}

/// Every `ActorId` that classifies into the given golden actor-kind label.
/// The golden is keyed on the semantic kind; asserting through ALL concrete
/// representatives also pins `actor_kind()`'s classification (Kernel and
/// KernelDispatcher behave as SpecAgent, AiClaude as Worker, ...).
fn representatives(kind: &str) -> Vec<ActorId> {
    match kind {
        "user" => vec![ActorId::User],
        "spec_agent" => vec![
            ActorId::AiSpec(CardId::from("spec-card-golden")),
            ActorId::Kernel,
            ActorId::KernelDispatcher,
        ],
        "worker" => vec![
            ActorId::AiCodex(CardId::from("codex-card-golden")),
            ActorId::AiClaude(CardId::from("claude-card-golden")),
        ],
        "other" => vec![ActorId::Plugin("plugin-golden".into())],
        other => panic!("unknown actor_kind label in golden: {other:?}"),
    }
}

fn expected_actor_kind(label: &str) -> ActorKind {
    match label {
        "user" => ActorKind::User,
        "spec_agent" => ActorKind::SpecAgent,
        "worker" => ActorKind::Worker,
        "other" => ActorKind::Other,
        other => panic!("unknown actor_kind label in golden: {other:?}"),
    }
}

fn outcome_of(res: &Result<(), TransitionError>) -> &'static str {
    match res {
        Ok(()) => "ok",
        Err(TransitionError::IllegalEdge { .. }) => "illegal_edge",
        Err(TransitionError::NotAuthorized { .. }) => "not_authorized",
    }
}

/// Recompute the full edge table from the current implementation, in the
/// golden's canonical row order. Uses the first representative per kind;
/// the verification test cross-checks the remaining representatives.
fn compute_table() -> Vec<EdgeRow> {
    let mut rows = Vec::with_capacity(ALL_STATES.len() * ALL_STATES.len() * ALL_KINDS.len());
    for from in ALL_STATES {
        for to in ALL_STATES {
            for kind in ALL_KINDS {
                let actor = &representatives(kind)[0];
                rows.push(EdgeRow {
                    from,
                    to,
                    actor_kind: kind.to_string(),
                    outcome: outcome_of(&validate_transition(from, to, actor)).to_string(),
                });
            }
        }
    }
    rows
}

fn render_table(rows: &[EdgeRow]) -> String {
    // One row per line so review diffs stay cell-granular.
    let mut out = String::from("[\n");
    for (i, row) in rows.iter().enumerate() {
        out.push_str("  ");
        out.push_str(&serde_json::to_string(row).expect("serialize edge row"));
        if i + 1 < rows.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str("]\n");
    out
}

#[test]
fn edge_table_matches_golden() {
    if std::env::var_os("REGEN_WAVE_FSM_GOLDEN").is_some() {
        std::fs::write(golden_path(), render_table(&compute_table()))
            .expect("write regenerated golden");
        panic!(
            "wave_fsm_edges.json regenerated from the current implementation; \
             hand-verify the diff, commit, and re-run without REGEN_WAVE_FSM_GOLDEN"
        );
    }

    let raw = std::fs::read_to_string(golden_path()).expect("read wave_fsm_edges.json");
    let golden: Vec<EdgeRow> = serde_json::from_str(&raw).expect("parse wave_fsm_edges.json");

    // Structural pins on the golden itself (human-verified facts).
    assert_eq!(
        golden.len(),
        324,
        "expected 9 states × 9 states × 4 actor kinds"
    );
    let ok_rows = golden.iter().filter(|r| r.outcome == "ok").count();
    assert_eq!(
        ok_rows, 40,
        "expected 22 legal distinct edges (incl. #741-4 dead-root \
         draft→failed + planning→failed) + 18 same-state idempotent rows"
    );
    assert!(
        golden
            .iter()
            .filter(|r| r.actor_kind == "worker" || r.actor_kind == "other")
            .all(|r| r.outcome == "not_authorized"),
        "workers and plugins must be denied everywhere"
    );

    // Cell-by-cell: implementation must answer exactly what the golden says,
    // for EVERY concrete ActorId that maps to the row's actor kind.
    let computed = compute_table();
    assert_eq!(computed.len(), golden.len());
    for (row, comp) in golden.iter().zip(&computed) {
        assert_eq!(
            (row.from, row.to, row.actor_kind.as_str()),
            (comp.from, comp.to, comp.actor_kind.as_str()),
            "golden row order drifted from canonical (from × to × kind) enumeration"
        );
        for actor in representatives(&row.actor_kind) {
            let res = validate_transition(row.from, row.to, &actor);
            assert_eq!(
                outcome_of(&res),
                row.outcome,
                "validate_transition({:?} -> {:?}, {actor:?}) diverged from golden",
                row.from,
                row.to,
            );
            // NotAuthorized must echo the classified actor kind + edge.
            if let Err(TransitionError::NotAuthorized {
                from,
                to,
                actor_kind,
            }) = res
            {
                assert_eq!((from, to), (row.from, row.to));
                assert_eq!(actor_kind, expected_actor_kind(&row.actor_kind));
            }
        }
    }
}
