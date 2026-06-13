use calm_truth::db::sqlite::worker_session_status_transition_allowed;
use calm_truth::worker::WorkerSessionState;
use serde_json::Value;

const GOLDEN: &str = include_str!("../../calm-server/tests/goldens/runtime_status_matrix.json");

const ALL_STATES: [(&str, WorkerSessionState); 7] = [
    ("starting", WorkerSessionState::Starting),
    ("running", WorkerSessionState::Running),
    ("idle", WorkerSessionState::Idle),
    ("turn_pending", WorkerSessionState::TurnPending),
    ("failed", WorkerSessionState::Failed),
    ("exited", WorkerSessionState::Exited),
    ("superseded", WorkerSessionState::Superseded),
];

#[test]
fn worker_session_status_matrix_matches_runtime_golden() {
    let golden: Value = serde_json::from_str(GOLDEN).expect("parse runtime matrix golden");

    let golden_statuses: Vec<&str> = golden["statuses"]
        .as_array()
        .expect("statuses array")
        .iter()
        .map(|v| v.as_str().expect("status string"))
        .collect();
    let expected_statuses: Vec<&str> = ALL_STATES.iter().map(|(name, _)| *name).collect();
    assert_eq!(
        golden_statuses, expected_statuses,
        "worker session state vocabulary must stay aligned with the PR0 runtime golden"
    );

    let matrix = golden["matrix"].as_object().expect("matrix object");
    assert_eq!(
        matrix.len(),
        ALL_STATES.len(),
        "matrix must have one row per worker session state"
    );

    let mut allow_count = 0usize;
    for (from_name, from) in ALL_STATES {
        let row = matrix[from_name].as_object().expect("matrix row object");
        assert_eq!(
            row.len(),
            ALL_STATES.len(),
            "matrix row {from_name} must cover every worker session state"
        );
        for (to_name, to) in ALL_STATES {
            let expected = match row[to_name].as_str().expect("allow/deny cell") {
                "allow" => {
                    allow_count += 1;
                    true
                }
                "deny" => false,
                other => panic!("unexpected matrix cell {from_name}->{to_name}: {other}"),
            };
            assert_eq!(
                worker_session_status_transition_allowed(from, to),
                expected,
                "worker_session_status_transition_allowed({from_name} -> {to_name}) drifted from runtime_status_matrix.json"
            );
        }
    }
    assert_eq!(
        allow_count, 14,
        "runtime_status_matrix.json allow-count changed; requires FROZEN-VECTOR-CHANGE review"
    );
}
