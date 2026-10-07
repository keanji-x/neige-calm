use super::Event;
use serde_json::json;

#[test]
fn forge_pr_checks_snapshot_is_complete_or_explicitly_historical() {
    let legacy = json!({
        "track_id": "track-01", "pr_number": 1, "conclusion": "success"
    });
    let historical = Event::from_kind_and_payload("forge.pr.checks", legacy.clone()).unwrap();
    assert_eq!(historical.payload_value(), legacy);
    let mut current = legacy;
    current["snapshot"] = json!({"head_sha": "exact-head", "mergeable": "mergeable"});
    let event = Event::from_kind_and_payload("forge.pr.checks", current.clone()).unwrap();
    assert_eq!(event.payload_value(), current);
    for field in ["head_sha", "mergeable"] {
        let mut incomplete = current.clone();
        incomplete["snapshot"]
            .as_object_mut()
            .unwrap()
            .remove(field);
        assert!(
            Event::from_kind_and_payload("forge.pr.checks", incomplete).is_err(),
            "missing {field} must be rejected"
        );
    }
}

/// #2170: each failed check carries a name and exactly one usable locator.
#[test]
fn forge_pr_checks_failed_checks_round_trip_and_need_a_locator() {
    let current = json!({
        "track_id": "track-01", "pr_number": 1, "conclusion": "failure",
        "snapshot": {"head_sha": "exact-head", "mergeable": "mergeable"},
        "failed_checks": [
            {"name": "lint", "url": "https://ci.example/lint"},
            {"name": "legacy status", "id": "SC_kw1"}
        ]
    });
    let event = Event::from_kind_and_payload("forge.pr.checks", current.clone()).unwrap();
    assert_eq!(event.payload_value(), current);
    for check in [
        json!({"name": "lint"}),
        json!({"url": "https://ci.example/lint"}),
    ] {
        let mut incomplete = current.clone();
        incomplete["failed_checks"] = json!([check]);
        assert!(
            Event::from_kind_and_payload("forge.pr.checks", incomplete).is_err(),
            "{check} must be rejected"
        );
    }
}

#[test]
fn forge_pr_checks_diagnostics_and_completeness_round_trip() {
    let value = json!({
        "track_id":"track-01", "pr_number":1,"conclusion":"failure",
        "snapshot":{"head_sha":"head","mergeable":"mergeable","all_checks_completed":false},
        "failed_checks":[
            {"name":"shard-1","id":"C1","diagnostics":{"status":"available", "failed_tests":["case_one"],"failed_steps":[],"error_summary":"assertion failed", "log_url":"https://github.com/o/r/actions/runs/1/job/2", "truncated":false}},
            {"name":"shard-2","id":"C2","diagnostics":{"status":"unavailable","reason":"no log permission"}}
        ]
    });
    let event = Event::from_kind_and_payload("forge.pr.checks", value.clone()).unwrap();
    assert_eq!(event.payload_value(), value);
    for field in [
        "failed_tests",
        "failed_steps",
        "error_summary",
        "log_url",
        "truncated",
    ] {
        let mut invalid = value.clone();
        invalid["failed_checks"][0]["diagnostics"]
            .as_object_mut()
            .unwrap()
            .remove(field);
        assert!(
            Event::from_kind_and_payload("forge.pr.checks", invalid).is_err(),
            "{field}"
        );
    }
    let mut empty = value;
    empty["failed_checks"][0]["diagnostics"]["failed_tests"] = json!([]);
    assert!(Event::from_kind_and_payload("forge.pr.checks", empty).is_err());
}
