//! Authentication suspension through the real queue ingress and run loop.
use crate::support::planner_queue_fixture::{
    Boot, Issuance, SEED_THREAD_ID, boot_with_issuance, idle_snapshot, post_input,
    post_input_with_attachments, send_json, upload_png,
};
use axum::http::StatusCode;
use calm_server::codex_appserver::Notification;
use serde_json::json;
use std::time::Duration;

fn fail(boot: &Boot) {
    boot.daemon.emit_notification_for_test(Notification::Other {method:"error".into(),params:json!({"error":{"message":"Your access token could not be refreshed because your refresh token was already used.","codexErrorInfo":"unauthorized"}})});
}
fn login(boot: &Boot) {
    boot.daemon.emit_notification_for_test(Notification::Other {
        method: "account/login/completed".into(),
        params: json!({"success":true,"error":null}),
    });
}
async fn settle_ticks(duration: Duration) {
    tokio::time::sleep(duration).await;
}

async fn select_model(boot: &Boot) {
    let (status, body) = send_json(
        boot.app.clone(),
        "PUT",
        format!("/api/cards/{}/planner/model", boot.planner_card.id),
        "user",
        json!({"model":"gpt-5","reasoning_effort":null}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn authentication_hold_preserves_queue_metadata_and_attachment_until_login() {
    let boot = boot_with_issuance(idle_snapshot(Vec::new()), Issuance::Live).await;
    select_model(&boot).await;
    fail(&boot);
    let (status, upload) = upload_png(
        boot.app.clone(),
        boot.planner_card.id.as_str(),
        b"authentication-fixture",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{upload}");
    let attachment = upload["attachmentId"].as_str().unwrap().to_string();
    let (status, posted) = post_input_with_attachments(
        boot.app.clone(),
        boot.planner_card.id.as_str(),
        "keep this message",
        &[attachment],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{posted}");
    let before = boot.harness.snapshot().await.pending_entries();
    assert_eq!(before.len(), 1);
    assert_eq!(before[0].attachments().len(), 1);
    settle_ticks(Duration::from_secs(6)).await;
    assert_eq!(
        boot.harness.snapshot().await.pending_entries(),
        before,
        "hold changes neither id/rev/time/message identity nor attachments"
    );
    assert_eq!(
        boot.harness.refused_issuances_for_test(),
        0,
        "hold must not repeatedly drain/rebuffer or write projections"
    );
    assert!(boot.daemon.started_turns_for_test().is_empty());
    assert!(
        boot.harness
            .issuance_block()
            .await
            .unwrap()
            .contains("Sign in again")
    );
    // Account presence is not a proof of repair.
    boot.daemon.emit_notification_for_test(Notification::Other {
        method: "account/updated".into(),
        params: json!({"authMode":"chatgpt"}),
    });
    settle_ticks(Duration::from_secs(3)).await;
    assert!(boot.daemon.started_turns_for_test().is_empty());
    login(&boot);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if !boot.daemon.started_turns_for_test().is_empty() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "queue did not resume after verified login"
        );
        settle_ticks(Duration::from_millis(100)).await;
    }
    let turns = boot.daemon.started_turns_for_test();
    assert_eq!(turns.len(), 1, "one queued batch continues once");
    assert_eq!(turns[0].1.len(), 2, "text plus the preserved image");
    assert!(boot.harness.snapshot().await.pending_entries().is_empty());
    assert_eq!(boot.harness.issuance_block().await, None);
    boot.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn authentication_hold_refuses_steer_without_taking_or_revising_the_entry() {
    let boot = boot_with_issuance(idle_snapshot(Vec::new()), Issuance::Live).await;
    select_model(&boot).await;
    post_input(
        boot.app.clone(),
        boot.planner_card.id.as_str(),
        "first turn",
    )
    .await;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while boot.daemon.started_turns_for_test().is_empty() {
        assert!(std::time::Instant::now() < deadline);
        settle_ticks(Duration::from_millis(100)).await;
    }
    fail(&boot);
    let (status, posted) = post_input(
        boot.app.clone(),
        boot.planner_card.id.as_str(),
        "keep queued",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{posted}");
    let before = boot.harness.snapshot().await.pending_entries();
    let changes_before = boot.event_payloads("harness.queue_changed").await.len();
    let entry = before[0].user_view().unwrap();
    let (status, answer) = send_json(
        boot.app.clone(),
        "POST",
        format!(
            "/api/cards/{}/planner/input/{}/steer",
            boot.planner_card.id, entry.id
        ),
        "user",
        json!({"if_entry_rev":entry.rev}),
    )
    .await;
    assert!(!status.is_success(), "held steer must refuse: {answer}");
    assert_eq!(boot.harness.snapshot().await.pending_entries(), before);
    assert!(boot.daemon.steered_turns_for_test().is_empty());
    assert_eq!(
        boot.event_payloads("harness.queue_changed").await.len(),
        changes_before,
        "held steer never takes/restores the entry"
    );
    assert_eq!(boot.daemon.started_turns_for_test().len(), 1);
    // Explicit edit/compact entrypoints share the native request fence.
    assert!(
        boot.daemon
            .thread_revert(SEED_THREAD_ID, "first-turn")
            .await
            .is_err()
    );
    assert!(
        boot.daemon
            .thread_compact_start(SEED_THREAD_ID)
            .await
            .is_err()
    );
    boot.harness.shutdown().await.unwrap();
}
