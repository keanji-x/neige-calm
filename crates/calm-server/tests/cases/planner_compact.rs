use super::planner_replace::{
    TURN_B, emit_item, replace, two_turns, wait_for, wait_for_stored_phase,
};
use crate::support::planner_queue_fixture::{SEED_THREAD_ID, boot_with, idle_snapshot, send_json};
use axum::http::StatusCode;
use calm_server::codex_appserver::Notification;
use calm_server::harness::HarnessPhaseTag;
use calm_server::harness::HarnessState;
use serde_json::json;

#[tokio::test]
async fn compact_rest_requires_a_human_and_keeps_unknown_cards_private_from_agents() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    for id in [boot.planner_card.id.to_string(), "missing-card".into()] {
        let (status, _) = send_json(
            boot.app.clone(),
            "POST",
            format!("/api/cards/{id}/planner/compact"),
            "ai:codex",
            json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
    assert!(boot.daemon.compacted_threads_for_test().is_empty());
    boot.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn compact_rest_submits_to_current_thread_and_reports_busy_without_sending_text() {
    let mut snapshot = idle_snapshot(vec![]);
    snapshot.last_turn_id = Some("previous-turn".into());
    let boot = boot_with(snapshot).await;
    let uri = format!("/api/cards/{}/planner/compact", boot.planner_card.id);
    let (status, body) = send_json(boot.app.clone(), "POST", uri.clone(), "user", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["started"], true);
    assert_eq!(body["card_id"], boot.planner_card.id.as_str());
    assert_eq!(boot.daemon.compacted_threads_for_test(), [SEED_THREAD_ID]);
    assert_eq!(
        boot.harness.snapshot().await.phase,
        HarnessPhaseTag::Compacting
    );
    let (status, _) = send_json(boot.app.clone(), "POST", uri, "user", json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(boot.daemon.compacted_threads_for_test().len(), 1);
    boot.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn compact_preserves_latest_user_turn_replacement_with_maintenance_items() {
    let boot = two_turns().await;
    let (status, body) = send_json(
        boot.app.clone(),
        "POST",
        format!("/api/cards/{}/planner/compact", boot.planner_card.id),
        "user",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    boot.daemon
        .emit_notification_for_test(Notification::TurnStarted {
            thread_id: SEED_THREAD_ID.into(),
            turn: json!({"id":"maintenance-turn"}),
        });
    wait_for("compaction start", || async {
        matches!(
            boot.harness.state_for_test().await,
            HarnessState::CompactionRunning { .. }
        )
    })
    .await;
    emit_item(
        &boot,
        "maintenance-turn",
        json!({"id":"compact-item", "type":"contextCompaction"}),
    );
    boot.daemon
        .emit_notification_for_test(Notification::TurnCompleted {
            thread_id: SEED_THREAD_ID.into(),
            turn: json!({"id":"maintenance-turn", "status":"completed"}),
        });
    wait_for_stored_phase(&boot, "turn_completed", TURN_B).await;
    let (status, body) = replace(&boot, TURN_B, "edited after compact").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["entry_id"].is_string());
    boot.harness.shutdown().await.unwrap();
}
