use super::CardRuntimeView;
use serde_json::json;

#[test]
fn runtime_activity_accepts_legacy_snapshots_without_inventing_a_timestamp() {
    let legacy = json!({
        "worker_session_id": "session", "kind": "terminal", "status": "idle"
    });
    let mut view: CardRuntimeView = serde_json::from_value(legacy.clone()).unwrap();
    assert_eq!(view.updated_at_ms, None);
    assert_eq!(serde_json::to_value(&view).unwrap(), legacy);

    view.updated_at_ms = Some(42);
    let current = serde_json::to_value(&view).unwrap();
    assert_eq!(current["updated_at_ms"], 42);
    let restored: CardRuntimeView = serde_json::from_value(current).unwrap();
    assert_eq!(restored.updated_at_ms, Some(42));
}
