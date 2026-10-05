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
