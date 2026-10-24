use super::planner_replace::{
    TURN_B, emit_item, replace, two_turns, wait_for, wait_for_stored_phase,
};
use crate::support::planner_queue_fixture::{SEED_THREAD_ID, boot_with, idle_snapshot, send_json};
use axum::http::StatusCode;
use calm_server::codex_appserver::Notification;
use calm_server::harness::HarnessPhaseTag;
use calm_server::harness::HarnessState;
use serde_json::json;

/// The run read declares support even without a live backend; no provider process is opened.
#[tokio::test]
async fn compact_run_declares_capability_for_dormant_backend_bindings() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    boot.harness.shutdown().await.unwrap();
    boot.registry.remove(&boot.worker_session_id);
    for (provider, supported) in [
        ("codex", true),
        ("claude", true),
        ("opencode", true),
        ("grok", false),
    ] {
        sqlx::query(
            "UPDATE cards SET payload = json_set(payload, '$.planner_provider', ?1) WHERE id = ?2",
        )
        .bind(provider)
        .bind(boot.planner_card.id.as_str())
        .execute(boot.repo.pool())
        .await
        .unwrap();
        let (status, body) = send_json(
            boot.app.clone(),
            "GET",
            format!("/api/cards/{}/planner/run", boot.planner_card.id),
            "user",
            json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body["phase"].is_null(), "{body}");
        assert!(body["worker_session_id"].is_null(), "{body}");
        assert_eq!(body["manual_compact_supported"], supported, "{body}");
    }
}

#[tokio::test]
async fn compact_run_declares_codex_capability() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    let (status, body) = send_json(
        boot.app.clone(),
        "GET",
        format!("/api/cards/{}/planner/run", boot.planner_card.id),
        "user",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["manual_compact_supported"], true, "{body}");
    boot.harness.shutdown().await.unwrap();
}

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
        boot.harness.snapshot().await.unwrap().phase,
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

#[tokio::test]
async fn compact_dormant_feedback_never_exposes_retained_ordinary_occupancy() {
    use calm_server::harness::TokenUsage;
    use calm_server::harness::compact_outcome::{CompactOutcome, CompactStatus};
    for state in ["idle", "failed"] {
        for status in [CompactStatus::Completed, CompactStatus::Started] {
            let mut snapshot = idle_snapshot(vec![]);
            snapshot.last_turn_id = Some("previous-turn".into());
            snapshot.token_usage = Some(TokenUsage {
                used_tokens: 100000,
                total_tokens: 15,
                context_window: Some(200000),
                at_ms: 1,
            });
            snapshot.compact_outcome = Some(CompactOutcome {
                operation_id: "operation".into(),
                thread_id: SEED_THREAD_ID.into(),
                turn_id: Some("maintenance".into()),
                status: status.clone(),
            });
            let boot = boot_with(snapshot).await;
            boot.harness.shutdown().await.unwrap();
            boot.registry.remove(&boot.worker_session_id);
            sqlx::query("UPDATE worker_sessions SET state=? WHERE id=?")
                .bind(state)
                .bind(&boot.worker_session_id)
                .execute(boot.repo.pool())
                .await
                .unwrap();
            let (code, body) = send_json(
                boot.app.clone(),
                "GET",
                format!("/api/cards/{}/planner/run", boot.planner_card.id),
                "user",
                json!({}),
            )
            .await;
            assert_eq!(code, StatusCode::OK, "{body}");
            assert!(body["worker_session_id"].is_null(), "{body}");
            assert!(body["phase"].is_null(), "{body}");
            assert!(body["token_usage"].is_null(), "{body}");
            assert_eq!(
                body["compact_outcome"]["status"],
                if status == CompactStatus::Completed {
                    "completed"
                } else {
                    "unconfirmed"
                },
                "{body}"
            );
        }
    }
}
