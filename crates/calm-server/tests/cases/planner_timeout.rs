use super::planner_turn_budget::{setting, start, wait_for_state};
use crate::support::planner_queue_fixture::{Issuance, boot_with_issuance, idle_snapshot};
use calm_server::db::RepoRead;
use calm_server::harness::{HarnessState, IssuingKind};
use serde_json::json;
use std::time::Duration;

#[tokio::test]
async fn planner_timeout_confirmed_record_and_next_real_start() {
    use calm_server::codex_appserver::Notification;
    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
    setting(&boot, json!("1")).await;
    start(&boot).await;
    let HarnessState::TurnRunning { turn_id, .. } = boot.harness.state_for_test().await else {
        panic!("running")
    };
    boot.harness
        .rewind_turn_clock_for_test(Duration::from_secs(61))
        .await;
    wait_for_state(&boot.harness, |s| {
        matches!(
            s,
            HarnessState::Issuing {
                kind: IssuingKind::Interrupt { .. },
                ..
            }
        )
    })
    .await;
    boot.daemon
        .emit_notification_for_test(Notification::TurnCompleted {
            thread_id: "thread-pending-queue".into(),
            turn: json!({"id":turn_id,"status":"interrupted","harness_timeout":{"forged":true}}),
        });
    wait_for_state(&boot.harness, |s| {
        matches!(s, HarnessState::TurnCompleted { .. })
    })
    .await;
    let (status, rows) = crate::support::planner_queue_fixture::get(
        boot.app.clone(),
        format!("/api/cards/{}/harness/items", boot.planner_card.id),
    )
    .await;
    assert_eq!(status, 200);
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["method"] == "turn/completed")
        .unwrap();
    let params: serde_json::Value = serde_json::from_str(row["params"].as_str().unwrap()).unwrap();
    let evidence = &params["harness_interruption_reason"];
    assert!(params.get("harness_timeout").is_none());
    assert_eq!(evidence["version"], "v1", "{rows}");
    assert_eq!(evidence["request"]["turn_id"], turn_id);
    assert_eq!(evidence["request"]["budget_ms"], 60000);
    assert!(
        evidence["request"]["effective_elapsed_ms"]
            .as_u64()
            .unwrap()
            >= 61000
    );
    assert!(
        evidence["ended_at_ms"].as_i64().unwrap()
            >= evidence["request"]["requested_at_ms"].as_i64().unwrap()
    );
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        boot.daemon.started_turns_for_test().len(),
        1,
        "timeout cannot wake itself"
    );
    start(&boot).await;
    let input = format!("{:?}", boot.daemon.started_turns_for_test()[1]);
    assert!(input.contains("Trusted previous turn timeout"), "{input}");
    assert!(
        input.contains(&format!("planner_turn_timeout:{}", row["id"])),
        "{input}"
    );
    boot.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn planner_timeout_identity_fence_rejects_each_mismatched_intent() {
    use crate::support::planner_timeout_fixture::{finish, request};
    for field in ["session_id", "card_id", "track_id", "thread_id", "turn_id"] {
        let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
        let turn = request(&boot).await;
        sqlx::query("UPDATE worker_sessions SET handle_state_json=json_set(handle_state_json,?1,'foreign') WHERE id=?2")
            .bind(format!("$.interruption_intent.{field}")).bind(&boot.worker_session_id).execute(boot.repo.pool()).await.unwrap();
        let row = finish(&boot, &turn, "interrupted").await;
        let params: serde_json::Value = serde_json::from_str(&row.params).unwrap();
        assert!(params.get("harness_timeout").is_none(), "{field}: {params}");
        assert!(
            params.get("harness_interruption_reason").is_none(),
            "{field}: {params}"
        );
        boot.harness.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn planner_timeout_completed_manual_and_foreign_do_not_mark_timeout() {
    use crate::support::planner_timeout_fixture::{finish, request, start};
    for status in ["completed", "failed"] {
        let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
        let turn = request(&boot).await;
        let row = finish(&boot, &turn, status).await;
        let params: serde_json::Value = serde_json::from_str(&row.params).unwrap();
        assert!(params.get("harness_timeout").is_none(), "{params}");
        assert!(params.get("harness_interruption_reason").is_none());
        boot.harness.shutdown().await.unwrap();
    }
    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
    let turn = start(&boot).await;
    boot.harness.interrupt("user".into()).await.unwrap();
    let row = finish(&boot, &turn, "interrupted").await;
    assert!(row.timeout().is_none());
    assert!(!row.params.contains("harness_timeout"));
    boot.harness.shutdown().await.unwrap();
    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
    let turn = request(&boot).await;
    boot.daemon.emit_notification_for_test(
        calm_server::codex_appserver::Notification::TurnCompleted {
            thread_id: "foreign-thread".into(),
            turn: json!({"id":turn,"status":"interrupted"}),
        },
    );
    tokio::time::sleep(Duration::from_millis(120)).await;
    assert!(matches!(
        boot.harness.state_for_test().await,
        HarnessState::Issuing { .. }
    ));
    let row = finish(&boot, &turn, "interrupted").await;
    assert!(row.timeout().is_some());
    boot.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn planner_timeout_failed_start_replays_until_a_new_terminal() {
    use crate::support::planner_timeout_fixture::{finish, request, start};
    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
    let turn = request(&boot).await;
    let row = finish(&boot, &turn, "interrupted").await;
    let key = format!("planner_turn_timeout:{}", row.id);
    boot.daemon.fail_turn_start_for_test();
    crate::support::planner_queue_fixture::post_input(
        boot.app.clone(),
        boot.planner_card.id.as_str(),
        "continue",
    )
    .await;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while boot.harness.refused_issuances_for_test() == 0 {
        assert!(std::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    boot.daemon.clear_turn_start_failure_for_test();
    wait_for_state(&boot.harness, |s| {
        matches!(s, HarnessState::TurnRunning { .. })
    })
    .await;
    assert!(format!("{:?}", boot.daemon.started_turns_for_test()).contains(&key));
    let next = boot
        .harness
        .state_for_test()
        .await
        .active_turn_id()
        .unwrap();
    finish(&boot, &next, "failed").await;
    start(&boot).await;
    assert!(!format!("{:?}", boot.daemon.started_turns_for_test().last().unwrap()).contains(&key));
    boot.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn planner_timeout_recovery_reads_terminal_and_keeps_system_projection() {
    use crate::support::planner_timeout_fixture::{finish, recover, request, wait};
    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
    let turn = request(&boot).await;
    let row = finish(&boot, &turn, "interrupted").await;
    let key = format!("planner_turn_timeout:{}", row.id);
    // A failed start leaves a durable batch/projection; neither is a consumed receipt.
    boot.daemon.fail_turn_start_for_test();
    crate::support::planner_queue_fixture::post_input(
        boot.app.clone(),
        boot.planner_card.id.as_str(),
        "continue",
    )
    .await;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while boot.harness.refused_issuances_for_test() == 0 {
        assert!(std::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let recovered = recover(&boot).await;
    boot.daemon.clear_turn_start_failure_for_test();
    wait(&recovered, |s| {
        matches!(s, HarnessState::TurnRunning { .. })
    })
    .await;
    let delivered = format!("{:?}", boot.daemon.started_turns_for_test().last().unwrap());
    assert_eq!(delivered.matches(&key).count(), 1, "{delivered}");
    let (_, rows) = crate::support::planner_queue_fixture::get(
        boot.app.clone(),
        format!("/api/cards/{}/harness/items", boot.planner_card.id),
    )
    .await;
    let notices: Vec<_> = rows
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|r| r["input_segments"].as_array().into_iter().flatten())
        .filter(|s| s["text"].as_str().is_some_and(|t| t.contains(&key)))
        .collect();
    assert_eq!(notices.len(), 1, "{rows}");
    assert_eq!(notices[0]["presentation"], "system");
    recovered.shutdown().await.unwrap();
}

#[tokio::test]
async fn planner_timeout_assistant_still_receives_previous_timeout() {
    use crate::support::planner_timeout_fixture::{finish, request};
    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
    let turn = request(&boot).await;
    let row = finish(&boot, &turn, "interrupted").await;
    sqlx::query(concat!(
        "UPDATE cards SET role='assistant', ",
        "payload=json_set(payload,'$.harness_profile','assistant',",
        "'$.assistant_provider','codex') WHERE id=?1"
    ))
    .bind(boot.planner_card.id.as_str())
    .execute(boot.repo.pool())
    .await
    .unwrap();
    sqlx::query("UPDATE worker_sessions SET contract='assistant' WHERE id=?")
        .bind(&boot.worker_session_id)
        .execute(boot.repo.pool())
        .await
        .unwrap();
    // The generic ingress exercises the real start; the Planner-only REST route refuses Assistant.
    boot.harness
        .observe_user_message_durable(
            "continue".into(),
            vec![],
            calm_server::harness::SendKey::unique_for_test(),
        )
        .await
        .unwrap();
    wait_for_state(&boot.harness, |s| {
        matches!(s, HarnessState::TurnRunning { .. })
    })
    .await;
    let delivered = format!("{:?}", boot.daemon.started_turns_for_test().last().unwrap());
    assert!(
        delivered.contains(&format!("planner_turn_timeout:{}", row.id)),
        "{delivered}"
    );
    assert!(!delivered.contains("Trusted Planner turn budget"));
    boot.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn planner_timeout_queries_do_not_truncate_history_or_cross_identity() {
    use crate::support::planner_queue_fixture::SEED_THREAD_ID;
    use crate::support::planner_timeout_fixture::{finish, request};
    use calm_server::db::prelude::*;
    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
    let turn = request(&boot).await;
    let row = finish(&boot, &turn, "interrupted").await;
    for n in 0..1025 {
        let id = format!("later-failure-{n}");
        boot.repo
            .harness_turn_outcome_put(
                &boot.worker_session_id,
                boot.planner_card.id.as_str(),
                boot.planner_card.track_id.as_str(),
                SEED_THREAD_ID,
                &id,
                &json!({"id":id,"status":"failed"}).to_string(),
            )
            .await
            .unwrap();
    }
    let pending = boot
        .repo
        .harness_pending_timeouts(
            boot.planner_card.id.as_str(),
            boot.planner_card.track_id.as_str(),
        )
        .await
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, row.id);
    let latest = boot
        .repo
        .harness_latest_terminal(
            boot.planner_card.id.as_str(),
            boot.planner_card.track_id.as_str(),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(latest.turn_id, "later-failure-1024");
    assert!(latest.timeout().is_none());
    assert!(
        boot.repo
            .harness_pending_timeouts("foreign-card", boot.planner_card.track_id.as_str())
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        boot.repo
            .harness_pending_timeouts(boot.planner_card.id.as_str(), "foreign-track")
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        boot.repo
            .harness_latest_terminal(boot.planner_card.id.as_str(), "foreign-track")
            .await
            .unwrap()
            .is_none()
    );
    // First insert wins: a later provider/recovery event cannot rewrite trusted terminal evidence.
    let duplicate = boot
        .repo
        .harness_turn_outcome_put(
            &boot.worker_session_id,
            boot.planner_card.id.as_str(),
            boot.planner_card.track_id.as_str(),
            SEED_THREAD_ID,
            &turn,
            &json!({"id":turn,"status":"completed"}).to_string(),
        )
        .await
        .unwrap();
    assert_eq!(duplicate, row.id);
    assert_eq!(
        boot.repo
            .harness_pending_timeouts(
                boot.planner_card.id.as_str(),
                boot.planner_card.track_id.as_str()
            )
            .await
            .unwrap()[0]
            .params,
        row.params
    );
    boot.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn planner_timeout_claude_confirmed_stop_and_restart_notice() {
    use super::claude_planner_stack_fixture::{Root, Stack};
    let root = Root::new("hold");
    let stack = Stack::boot(&root).await;
    let (_, card) = stack.create_claude_track().await;
    let session = stack.runtime(&card).await;
    let (status, body) = stack
        .send(
            "PUT",
            "/api/settings",
            Some(json!({"settings":{"planner_max_turn_duration_minutes":"1"}})),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(stack.post_input(&card, "start").await.0, 200);
    stack.wait_phase(&session.id, "turn_running").await;
    stack
        .harness(&session.id)
        .rewind_turn_clock_for_test(Duration::from_secs(61))
        .await;
    stack.wait_phase(&session.id, "turn_completed").await;
    let rows = stack.wait_outcomes(&card, 1).await;
    let params = &rows[0];
    assert_eq!(
        params["harness_interruption_reason"]["version"], "v1",
        "{rows:?}"
    );
    let terminal = stack
        .repo()
        .harness_latest_terminal(
            &card,
            stack
                .repo()
                .card_get(&card)
                .await
                .unwrap()
                .unwrap()
                .track_id
                .as_str(),
        )
        .await
        .unwrap()
        .unwrap();
    let key = format!("planner_turn_timeout:{}", terminal.id);
    stack.shutdown().await;
    let stack = Stack::boot(&root).await;
    root.remove_fake("stdin");
    assert_eq!(stack.post_input(&card, "continue").await.0, 200);
    super::claude_planner_wiring::wait_file(&root, "stdin").await;
    let stdin = root.read_fake("stdin").unwrap();
    assert!(stdin.contains(&key), "{stdin}");
    stack.shutdown().await;
}

#[tokio::test]
async fn planner_timeout_claude_unconfirmed_stop_does_not_acquire_trusted_evidence() {
    use super::claude_planner_stack_fixture::{Root, Stack};
    use calm_server::claude_planner::stop::{
        clear_claude_planner_stop_failure_for_test, fail_claude_planner_stop_for_test,
    };
    let root = Root::new("hold");
    let stack = Stack::boot(&root).await;
    let (_, card) = stack.create_claude_track().await;
    let session = stack.runtime(&card).await;
    assert_eq!(
        stack
            .send(
                "PUT",
                "/api/settings",
                Some(json!({"settings":{"planner_max_turn_duration_minutes":"1"}}))
            )
            .await
            .0,
        200
    );
    assert_eq!(stack.post_input(&card, "start").await.0, 200);
    stack.wait_phase(&session.id, "turn_running").await;
    fail_claude_planner_stop_for_test(&session.id);
    stack
        .harness(&session.id)
        .rewind_turn_clock_for_test(Duration::from_secs(61))
        .await;
    stack.wait_phase(&session.id, "turn_completed").await;
    let rows = stack.wait_outcomes(&card, 1).await;
    let params = &rows[0];
    assert!(params.get("harness_timeout").is_none(), "{params}");
    assert!(
        params.get("harness_interruption_reason").is_none(),
        "{params}"
    );
    clear_claude_planner_stop_failure_for_test(&session.id);
    stack.shutdown().await;
}

#[tokio::test]
async fn planner_timeout_legacy_provider_payload_does_not_become_trusted_evidence() {
    use crate::support::planner_timeout_fixture::{finish, request, start};
    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
    let turn = request(&boot).await;
    let row = finish(&boot, &turn, "interrupted").await;
    // Use real producer facts, then retain them only in the old provider-owned namespace.
    // The released recorder protected the reason field, but not arbitrary other payload keys.
    let mut params: serde_json::Value = serde_json::from_str(&row.params).unwrap();
    let evidence = serde_json::to_value(row.timeout().unwrap()).unwrap();
    params["harness_interruption_reason"] = json!("max_turn_duration");
    params["harness_timeout"] = evidence;
    sqlx::query("UPDATE harness_items SET params=?1 WHERE id=?2")
        .bind(params.to_string())
        .bind(row.id)
        .execute(boot.repo.pool())
        .await
        .unwrap();
    assert!(
        boot.repo
            .harness_pending_timeouts(
                boot.planner_card.id.as_str(),
                boot.planner_card.track_id.as_str()
            )
            .await
            .unwrap()
            .is_empty(),
        "legacy provider data must not acquire kernel time authority"
    );
    start(&boot).await;
    let delivered = format!("{:?}", boot.daemon.started_turns_for_test().last().unwrap());
    assert!(
        !delivered.contains("Trusted previous turn timeout"),
        "{delivered}"
    );
    boot.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn planner_timeout_terminal_causality_survives_utc_rollback_and_ties() {
    use crate::support::planner_queue_fixture::SEED_THREAD_ID;
    use crate::support::planner_timeout_fixture::{finish, request};
    use calm_server::db::prelude::*;
    for later_utc in [90_000_i64, 100_000] {
        for status in ["failed", "interrupted", "completed"] {
            let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
            let turn = request(&boot).await;
            let timeout = finish(&boot, &turn, "interrupted").await;
            sqlx::query("UPDATE harness_items SET created_at_ms=100000 WHERE id=?1")
                .bind(timeout.id)
                .execute(boot.repo.pool())
                .await
                .unwrap();
            let id = boot
                .repo
                .harness_turn_outcome_put(
                    &boot.worker_session_id,
                    boot.planner_card.id.as_str(),
                    boot.planner_card.track_id.as_str(),
                    SEED_THREAD_ID,
                    "later",
                    &json!({"id":"later","status":status}).to_string(),
                )
                .await
                .unwrap();
            sqlx::query("UPDATE harness_items SET created_at_ms=?1 WHERE id=?2")
                .bind(later_utc)
                .bind(id)
                .execute(boot.repo.pool())
                .await
                .unwrap();
            let latest = boot
                .repo
                .harness_latest_terminal(
                    boot.planner_card.id.as_str(),
                    boot.planner_card.track_id.as_str(),
                )
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                (latest.id, latest.created_at_ms),
                (id, later_utc),
                "{status}"
            );
            let pending = boot
                .repo
                .harness_pending_timeouts(
                    boot.planner_card.id.as_str(),
                    boot.planner_card.track_id.as_str(),
                )
                .await
                .unwrap();
            assert_eq!(
                pending
                    .iter()
                    .map(|r| (r.id, r.created_at_ms))
                    .collect::<Vec<_>>(),
                if status == "completed" {
                    vec![]
                } else {
                    vec![(timeout.id, 100_000)]
                }
            );
            boot.harness.shutdown().await.unwrap();
        }
    }
}
