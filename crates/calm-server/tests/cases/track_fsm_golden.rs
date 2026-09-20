//! Data-driven golden for the track lifecycle FSM edge table (`tests/goldens/track_fsm_edges.json`,
//! 9 from × 9 to × 4 actor kinds). Regenerate after an intentional FSM change with
//! `REGEN_TRACK_FSM_GOLDEN=1 cargo test -p calm-server --test track_suite track_fsm_golden::`, then hand-verify the diff.

use calm_server::ids::{ActorId, CardId};
use calm_server::model::TrackLifecycle;
use calm_server::track_lifecycle::{ActorKind, TransitionError, validate_transition};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const ALL_STATES: [TrackLifecycle; 9] = [
    TrackLifecycle::Draft,
    TrackLifecycle::Planning,
    TrackLifecycle::Dispatching,
    TrackLifecycle::Working,
    TrackLifecycle::Blocked,
    TrackLifecycle::Reviewing,
    TrackLifecycle::Done,
    TrackLifecycle::Canceled,
    TrackLifecycle::Failed,
];

/// Actor-kind labels as persisted in the golden, in row order.
const ALL_KINDS: [&str; 4] = ["user", "planner_agent", "worker", "other"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct EdgeRow {
    from: TrackLifecycle,
    to: TrackLifecycle,
    actor_kind: String,
    outcome: String,
}

fn golden_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/goldens/track_fsm_edges.json")
}

/// Every `ActorId` that classifies into the given golden actor-kind label; asserting through ALL concrete
/// representatives also pins `actor_kind()`'s classification.
fn representatives(kind: &str) -> Vec<ActorId> {
    match kind {
        "user" => vec![ActorId::User],
        "planner_agent" => vec![
            ActorId::AiPlanner(CardId::from("planner-card-golden")),
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
        "planner_agent" => ActorKind::PlannerAgent,
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

/// Recompute the full edge table from the current implementation, in the golden's canonical row order,
/// using the first representative per kind.
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
    if std::env::var_os("REGEN_TRACK_FSM_GOLDEN").is_some() {
        std::fs::write(golden_path(), render_table(&compute_table()))
            .expect("write regenerated golden");
        panic!(
            "track_fsm_edges.json regenerated from the current implementation; \
             hand-verify the diff, commit, and re-run without REGEN_TRACK_FSM_GOLDEN"
        );
    }

    let raw = std::fs::read_to_string(golden_path()).expect("read track_fsm_edges.json");
    let golden: Vec<EdgeRow> = serde_json::from_str(&raw).expect("parse track_fsm_edges.json");

    // Structural pins on the golden itself (human-verified facts).
    assert_eq!(
        golden.len(),
        324,
        "expected 9 states × 9 states × 4 actor kinds"
    );
    let ok_rows = golden.iter().filter(|r| r.outcome == "ok").count();
    assert_eq!(
        ok_rows, 45,
        "expected 27 legal distinct edges (incl. #741-4 dead-root \
         draft→failed + planning→failed, and the Planner-feedback-#3 \
         self-executed planning→reviewing) + 18 same-state idempotent rows"
    );
    assert!(
        golden
            .iter()
            .filter(|r| r.actor_kind == "worker" || r.actor_kind == "other")
            .all(|r| r.outcome == "not_authorized"),
        "workers and plugins must be denied everywhere"
    );

    // Cell-by-cell, for EVERY concrete ActorId that maps to the row's actor kind.
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
