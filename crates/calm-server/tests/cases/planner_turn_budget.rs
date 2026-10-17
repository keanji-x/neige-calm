use crate::support::planner_queue_fixture::{
    Boot, Issuance, boot_with_issuance, idle_snapshot, post_input, send_json,
};
use calm_server::harness::{HarnessState, IssuingKind, PlannerHarness};
use serde_json::json;
use std::time::{Duration, Instant};

pub(super) async fn wait_for_state(
    harness: &PlannerHarness,
    matches: impl Fn(&HarnessState) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let state = harness.state_for_test().await;
        if matches(&state) {
            return;
        }
        assert!(Instant::now() < deadline, "last state: {state:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

pub(super) async fn setting(boot: &Boot, value: serde_json::Value) {
    let (status, body) = send_json(
        boot.app.clone(),
        "PUT",
        "/api/settings".into(),
        "user",
        json!({"settings": {"planner_max_turn_duration_minutes": value}}),
    )
    .await;
    assert_eq!(status, 200, "{body}");
}
pub(super) async fn start(boot: &Boot) {
    let (status, body) = post_input(boot.app.clone(), boot.planner_card.id.as_str(), "start").await;
    assert_eq!(status, 200, "{body}");
    wait_for_state(&boot.harness, |s| {
        matches!(s, HarnessState::TurnRunning { .. })
    })
    .await;
}

#[tokio::test]
async fn planner_turn_budget_rest_existing_harness_warns_and_stops() {
    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
    let (status, _) = send_json(
        boot.app.clone(),
        "PUT",
        "/api/settings".into(),
        "user",
        json!({"settings": {"planner_max_turn_duration_minutes": "1"}}),
    )
    .await;
    assert_eq!(status, 200);
    let (status, _) = post_input(boot.app.clone(), boot.planner_card.id.as_str(), "start").await;
    assert_eq!(status, 200);
    wait_for_state(&boot.harness, |s| {
        matches!(s, HarnessState::TurnRunning { .. })
    })
    .await;
    let input = serde_json::to_string(&boot.daemon.started_turns_for_test()).unwrap();
    assert!(
        input.contains("budget"),
        "initial trusted budget missing: {input}"
    );
    setting(&boot, json!("240")).await; // The running one-minute budget stays frozen.
    boot.harness
        .rewind_turn_clock_for_test(Duration::from_secs(49))
        .await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(boot.daemon.steered_turns_for_test().len(), 1);
    boot.harness
        .rewind_turn_clock_for_test(Duration::from_secs(12))
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
    boot.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn planner_turn_budget_current_freezes_next_samples_and_snapshot_roundtrips() {
    use calm_server::codex_appserver::Notification;
    use calm_server::harness::snapshot::FrozenBudget;
    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
    setting(&boot, json!("1")).await;
    start(&boot).await;
    let snapshot = boot.harness.snapshot().await.unwrap();
    assert!(
        matches!(&snapshot.turn_budget, FrozenBudget::Planner {budget, ..} if budget.limit() == Duration::from_secs(60))
    );
    let restored = calm_server::harness::HarnessSnapshot::from_value_strict(
        serde_json::to_value(&snapshot).unwrap(),
    );
    assert_eq!(snapshot.turn_budget, restored.turn_budget);
    setting(&boot, json!("2")).await;
    assert_eq!(
        snapshot.turn_budget,
        boot.harness.snapshot().await.unwrap().turn_budget
    );
    let HarnessState::TurnRunning { turn_id, .. } = boot.harness.state_for_test().await else {
        panic!("running")
    };
    boot.daemon
        .emit_notification_for_test(Notification::TurnCompleted {
            thread_id: "thread-pending-queue".into(),
            turn: json!({"id": turn_id, "status":"completed"}),
        });
    wait_for_state(&boot.harness, |s| {
        matches!(s, HarnessState::TurnCompleted { .. })
    })
    .await;
    start(&boot).await;
    let snapshot = boot.harness.snapshot().await.unwrap();
    assert!(
        matches!(&snapshot.turn_budget, FrozenBudget::Planner {budget, ..} if budget.limit() == Duration::from_secs(120))
    );
    setting(&boot, json!("1")).await;
    boot.harness
        .rewind_turn_clock_for_test(Duration::from_secs(61))
        .await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(matches!(
        boot.harness.state_for_test().await,
        HarnessState::TurnRunning { .. }
    ));
    assert!(boot.daemon.steered_turns_for_test().is_empty());
    boot.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn planner_turn_budget_corrupt_storage_leaves_input_queued_until_reset() {
    use calm_server::db::prelude::*;
    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
    boot.repo
        .settings_upsert("planner_max_turn_duration_minutes", "broken")
        .await
        .unwrap();
    let (status, _) = post_input(boot.app.clone(), boot.planner_card.id.as_str(), "start").await;
    assert_eq!(status, 200);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(boot.daemon.started_turns_for_test().is_empty());
    assert_eq!(boot.harness.snapshot().await.unwrap().pending_entries().len(), 1);
    assert!(
        boot.harness
            .issuance_block()
            .await
            .unwrap()
            .contains("Planner duration in Settings")
    );
    setting(&boot, serde_json::Value::Null).await;
    // Reset does not invent a recovery policy: the existing refusal backoff retries it.
    tokio::time::timeout(Duration::from_secs(35), async {
        loop {
            if matches!(
                boot.harness.state_for_test().await,
                HarnessState::TurnRunning { .. }
            ) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let input = serde_json::to_string(&boot.daemon.started_turns_for_test()).unwrap();
    assert!(input.contains("30 minutes"));
    boot.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn planner_turn_budget_slow_warning_never_delays_hard_stop() {
    use calm_server::shared_codex_appserver::TurnStartReturnHook;
    use std::sync::Arc;
    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
    setting(&boot, json!("1")).await;
    start(&boot).await;
    let HarnessState::TurnRunning {
        turn_id: old_turn, ..
    } = boot.harness.state_for_test().await
    else {
        panic!("running")
    };
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    boot.daemon
        .install_turn_steer_return_hook_for_test(TurnStartReturnHook {
            entered: entered.clone(),
            release: release.clone(),
        });
    boot.harness
        .rewind_turn_clock_for_test(Duration::from_secs(49))
        .await;
    tokio::time::timeout(Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    boot.harness
        .rewind_turn_clock_for_test(Duration::from_secs(12))
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
    assert_eq!(boot.daemon.steered_turns_for_test().len(), 1);
    boot.daemon.emit_notification_for_test(
        calm_server::codex_appserver::Notification::TurnCompleted {
            thread_id: "thread-pending-queue".into(),
            turn: json!({"id":old_turn,"status":"interrupted"}),
        },
    );
    wait_for_state(&boot.harness, |s| {
        matches!(s, HarnessState::TurnCompleted { .. })
    })
    .await;
    start(&boot).await;
    release.notify_one();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(boot.daemon.started_turns_for_test().len(), 2);
    let (_, rows) = crate::support::planner_queue_fixture::get(
        boot.app.clone(),
        format!("/api/cards/{}/harness/items", boot.planner_card.id),
    )
    .await;
    assert!(
        !rows.to_string().contains("budget reminder"),
        "late response must not project into a new turn: {rows}"
    );
    boot.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn planner_turn_budget_hold_pauses_warning_and_hard_stop() {
    use calm_server::event::AskQuestion;
    use calm_server::harness::held_requests::{
        ConnectionId, HeldRequestMessage, HeldResponder, RequestKey,
    };
    struct Responder;
    impl HeldResponder for Responder {
        fn respond(self: Box<Self>, _: usize) {}
    }
    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
    setting(&boot, json!("1")).await;
    start(&boot).await;
    let sender = boot.harness.held_request_sender();
    sender
        .send(HeldRequestMessage::Open {
            request_key: RequestKey("budget-hold".into()),
            connection: ConnectionId("budget".into()),
            questions: vec![AskQuestion {
                title: "Continue?".into(),
                options: vec!["yes".into(), "no".into()],
            }],
            responder: Box::new(Responder),
        })
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !boot.harness.watchdog_saw_held_request_for_test().await {
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    boot.harness
        .rewind_turn_clock_for_test(Duration::from_secs(3600))
        .await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(boot.daemon.steered_turns_for_test().is_empty());
    assert!(matches!(
        boot.harness.state_for_test().await,
        HarnessState::TurnRunning { .. }
    ));
    sender
        .send(HeldRequestMessage::Gone {
            request_key: RequestKey("budget-hold".into()),
        })
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(matches!(
        boot.harness.state_for_test().await,
        HarnessState::TurnRunning { .. }
    ));
    boot.harness
        .rewind_turn_clock_for_test(Duration::from_secs(49))
        .await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(boot.daemon.steered_turns_for_test().len(), 1);
    boot.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn planner_turn_budget_only_real_planner_role_and_profile() {
    use calm_server::db::prelude::*;
    for (role, marker) in [
        ("assistant", "assistant"),
        ("worker", "plain_chat"),
        ("worker", ""),
    ] {
        let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
        sqlx::query("UPDATE cards SET role = ?1, payload = ?2 WHERE id = ?3")
            .bind(role)
            .bind(
                json!({"schemaVersion":1,"harness_profile":marker,"planner_provider":"codex","assistant_provider":"codex"})
                    .to_string(),
            )
            .bind(boot.planner_card.id.as_str())
            .execute(boot.repo.pool())
            .await
            .unwrap();
        if role == "assistant" {
            sqlx::query("UPDATE worker_sessions SET contract='assistant' WHERE id=?")
                .bind(&boot.worker_session_id)
                .execute(boot.repo.pool())
                .await
                .unwrap();
        }
        boot.repo
            .settings_upsert("planner_max_turn_duration_minutes", "broken")
            .await
            .unwrap();
        // Send through production harness ingress: the REST role-specific input endpoint
        // rightly refuses a role changed out from under this fixture's Planner registry.
        boot.harness
            .observe(calm_server::harness::Observation::TrackGoal {
                text: "start".into(),
            })
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while boot.harness.snapshot().await.unwrap().pending_entries().is_empty() {
            assert!(
                Instant::now() < deadline,
                "ingress must stage the observation"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        boot.harness
            .rewind_debounce_for_test(Duration::from_secs(61))
            .await;
        wait_for_state(&boot.harness, |s| {
            matches!(s, HarnessState::TurnRunning { .. })
        })
        .await;
        let input = serde_json::to_string(&boot.daemon.started_turns_for_test()).unwrap();
        assert!(!input.contains("turn budget"), "{role}: {input}");
        boot.harness
            .rewind_turn_clock_for_test(Duration::from_secs(61))
            .await;
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(matches!(
            boot.harness.state_for_test().await,
            HarnessState::TurnRunning { .. }
        ));
        assert!(boot.daemon.steered_turns_for_test().is_empty());
        boot.harness.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn planner_turn_budget_claude_initial_warning_only() {
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
    let (status, body) = stack.post_input(&card, "start").await;
    assert_eq!(status, 200, "{body}");
    super::claude_planner_wiring::wait_file(&root, "stdin").await;
    let stdin = root.read_fake("stdin").unwrap();
    assert!(stdin.contains("1 minutes"), "{stdin}");
    assert!(stdin.contains("12 seconds"), "{stdin}");
    stack.wait_phase(&session.id, "turn_running").await;
    let harness = stack.harness(&session.id);
    harness
        .rewind_turn_clock_for_test(Duration::from_secs(49))
        .await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(matches!(
        harness.state_for_test().await,
        HarnessState::TurnRunning { .. }
    ));
    assert_eq!(stdin, root.read_fake("stdin").unwrap());
    let frozen = harness.snapshot().await.unwrap().turn_budget;
    let (status, _) = stack
        .send(
            "PUT",
            "/api/settings",
            Some(json!({"settings":{"planner_max_turn_duration_minutes":"2"}})),
        )
        .await;
    assert_eq!(status, 200);
    stack.shutdown().await;
    let stack = Stack::boot(&root).await;
    assert_eq!(
        frozen,
        stack.harness(&session.id).snapshot().await.unwrap().turn_budget
    );
    stack.shutdown().await;
}

#[tokio::test]
async fn planner_turn_budget_compaction_keeps_its_existing_limit() {
    use calm_server::codex_appserver::Notification;
    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
    setting(&boot, json!("1")).await;
    start(&boot).await;
    let HarnessState::TurnRunning { turn_id, .. } = boot.harness.state_for_test().await else {
        panic!("running")
    };
    boot.daemon
        .emit_notification_for_test(Notification::TurnCompleted {
            thread_id: "thread-pending-queue".into(),
            turn: json!({"id":turn_id,"status":"completed"}),
        });
    wait_for_state(&boot.harness, |s| {
        matches!(s, HarnessState::TurnCompleted { .. })
    })
    .await;
    boot.daemon
        .clear_active_turn_for_test("thread-pending-queue");
    boot.harness.compact().await.unwrap();
    boot.daemon
        .emit_turn_started_for_test("thread-pending-queue", "maintenance-budget");
    wait_for_state(&boot.harness, |s| {
        matches!(s, HarnessState::CompactionRunning { .. })
    })
    .await;
    boot.harness
        .set_state_for_test(HarnessState::CompactionRunning {
            turn_id: "maintenance-budget".into(),
            started_at: Instant::now() - Duration::from_secs(61),
        })
        .await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(matches!(
        boot.harness.state_for_test().await,
        HarnessState::CompactionRunning { .. }
    ));
    assert!(boot.daemon.steered_turns_for_test().is_empty());
    boot.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn planner_turn_budget_failed_warning_is_once_and_still_stops() {
    use calm_server::planner_turn_budget::FrozenBudget;
    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
    setting(&boot, json!("1")).await;
    start(&boot).await;
    boot.daemon.fail_turn_steer_for_test(true);
    boot.harness
        .rewind_turn_clock_for_test(Duration::from_secs(49))
        .await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(matches!(
        boot.harness.snapshot().await.unwrap().turn_budget,
        FrozenBudget::Planner {
            warning_attempted: true,
            ..
        }
    ));
    boot.daemon.fail_turn_steer_for_test(false);
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        boot.daemon.steered_turns_for_test().len(),
        1,
        "the failed attempt is not retried"
    );
    boot.harness
        .rewind_turn_clock_for_test(Duration::from_secs(12))
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
    boot.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn planner_turn_budget_warning_echo_keeps_system_projection_once() {
    use calm_server::codex_appserver::Notification;
    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
    setting(&boot, json!("1")).await;
    start(&boot).await;
    boot.daemon.install_turn_steer_return_hook_for_test(
        calm_server::shared_codex_appserver::TurnStartReturnHook {
            entered: std::sync::Arc::new(tokio::sync::Notify::new()),
            release: std::sync::Arc::new(tokio::sync::Notify::new()),
        },
    );
    boot.harness
        .rewind_turn_clock_for_test(Duration::from_secs(49))
        .await;
    let deadline = Instant::now() + Duration::from_secs(5);
    while boot.daemon.steered_turns_for_test().is_empty() {
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let (thread, turn, _, client) = boot.daemon.steered_turns_for_test().remove(0);
    boot.daemon.emit_notification_for_test(Notification::parse("item/completed".into(), json!({
        "threadId":thread,"turnId":turn,"item":{"type":"userMessage","id":"budget-echo","clientId":client.unwrap(),"content":[{"type":"text","text":"echo"}]}
    })));
    tokio::time::sleep(Duration::from_millis(2200)).await; // RPC timeout cannot erase an actual echo.
    let (status, rows) = crate::support::planner_queue_fixture::get(
        boot.app.clone(),
        format!("/api/cards/{}/harness/items", boot.planner_card.id),
    )
    .await;
    assert_eq!(status, 200);
    let warnings: Vec<_> = rows
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row.to_string().contains("budget reminder"))
        .collect();
    assert_eq!(warnings.len(), 1, "{rows}");
    assert_eq!(warnings[0]["input_segments"][0]["presentation"], "system");
    boot.daemon
        .emit_notification_for_test(Notification::TurnCompleted {
            thread_id: thread,
            turn: json!({"id":turn,"status":"completed"}),
        });
    wait_for_state(&boot.harness, |s| {
        matches!(s, HarnessState::TurnCompleted { .. })
    })
    .await;
    let (_, rows) = crate::support::planner_queue_fixture::get(
        boot.app.clone(),
        format!("/api/cards/{}/harness/items", boot.planner_card.id),
    )
    .await;
    assert_eq!(
        rows.as_array()
            .unwrap()
            .iter()
            .filter(|row| row.to_string().contains("budget reminder"))
            .count(),
        1,
        "an actual echo survives completion"
    );
    boot.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn planner_turn_budget_failed_start_replays_its_frozen_attempt() {
    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
    boot.daemon.fail_turn_start_for_test();
    setting(&boot, json!("1")).await;
    let (status, _) = post_input(boot.app.clone(), boot.planner_card.id.as_str(), "start").await;
    assert_eq!(status, 200);
    let deadline = Instant::now() + Duration::from_secs(5);
    while boot.harness.refused_issuances_for_test() == 0 {
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let frozen = boot.harness.snapshot().await.unwrap().turn_budget;
    setting(&boot, json!("2")).await;
    boot.daemon.clear_turn_start_failure_for_test();
    wait_for_state(&boot.harness, |s| {
        matches!(s, HarnessState::TurnRunning { .. })
    })
    .await;
    assert_eq!(frozen, boot.harness.snapshot().await.unwrap().turn_budget);
    let input = serde_json::to_string(&boot.daemon.started_turns_for_test()).unwrap();
    assert!(
        input.contains("1 minutes"),
        "the durable attempt must not be resampled: {input}"
    );
    boot.harness.shutdown().await.unwrap();
}
