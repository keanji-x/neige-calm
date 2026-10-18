use super::completed_commit_tests::Fixture;
use super::handoff_pause_tests::{params, request};
use super::*;
use crate::harness::handoff_pause::PauseState;

#[test]
fn handoff_pause_legacy_and_invalid_snapshot_contract() {
    let mut old = serde_json::to_value(HarnessSnapshot::initial(0, vec![])).unwrap();
    old.as_object_mut().unwrap().remove("handoff_pause");
    assert_eq!(
        HarnessSnapshot::parse_known(old.clone())
            .unwrap()
            .handoff_pause,
        PauseState::Unpaused
    );
    for bad in [
        serde_json::Value::Null,
        serde_json::json!({"state":"other"}),
        serde_json::json!({"state":"paused"}),
        serde_json::json!({"state":"paused","receipt":{"claim_id":"","card_id":"c","session_id":"s","track_id":"t"}}),
    ] {
        old["handoff_pause"] = bad;
        assert!(HarnessSnapshot::parse_known(old.clone()).is_none());
    }
}

#[tokio::test]
async fn handoff_pause_replayed_start_hint_cannot_repoint_paused_thread() {
    let f = Fixture::new().await;
    let receipt = f.harness.pause_for_handoff(request(&f)).await.unwrap();
    let mut replay = params(&f, f.stored().await);
    replay.thread_id = Some("obsolete-operation-thread".into());
    let (restored, _ingress) = PlannerHarness::run_unstarted_for_test(replay, 8);
    assert_eq!(
        restored.inner.thread_id.read().await.as_deref(),
        Some("thread-commit-wake")
    );
    restored.persist_snapshot().await.unwrap();
    assert_eq!(
        f.stored().await.last_thread_id.as_deref(),
        Some("thread-commit-wake")
    );
    assert_eq!(
        restored.resume_from_handoff(receipt).await.unwrap(),
        crate::harness::handoff_pause::ReleaseOutcome::HealthUnconfirmedAfterRecovery
    );
}

#[tokio::test]
async fn handoff_pause_checkpoints_owner_pending_systems_before_ack() {
    let f = Fixture::new().await;
    // Ordinary async observe has no durable ACK; model a prior failed async checkpoint by
    // staging on the production observation owner without saving it yet.
    let entry = QueueEntry::system(
        Observation::TrackGoal {
            text: "pending source".into(),
        },
        Some(15),
    )
    .unwrap();
    assert!(matches!(
        on_observation(&f.harness.inner, entry).await,
        EnqueueOutcome::Accepted { .. }
    ));
    assert!(f.stored().await.pending_entries().is_empty());
    let receipt = f.harness.pause_for_handoff(request(&f)).await.unwrap();
    assert_eq!(f.stored().await.pending_entries().len(), 1);
    assert_eq!(f.stored().await.push_watermark, 15);
    assert_eq!(
        f.stored().await.handoff_pause,
        PauseState::Paused { receipt }
    );
}
