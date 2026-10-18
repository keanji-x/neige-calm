//! Actual HTTP commands cross issuer, persistence and native-effect awaits.
use super::historical_control_admission_tests::{HistoricalFixture, hand_area_to_alice};
use super::*;
use crate::test_seams::{self, PausePoint};

async fn perform(
    app: axum::Router,
    cookie: String,
    method: &'static str,
    uri: String,
    body: Value,
) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header(header::COOKIE, cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

fn arm(fx: &HistoricalFixture, point: &str) -> PausePoint {
    let pause = PausePoint::default();
    test_seams::install_pause_for_test(point, &fx.card, pause.clone());
    pause
}

async fn entered(pause: &PausePoint) {
    tokio::time::timeout(Duration::from_secs(5), pause.entered.notified())
        .await
        .expect("the production control reaches its actual protected point");
}

async fn logout(fx: &HistoricalFixture) {
    let (status, body) = perform(
        fx.app.clone(),
        fx.cookie.clone(),
        "POST",
        "/api/auth/logout".into(),
        json!({}),
    )
    .await;
    assert_eq!((status, body), (StatusCode::OK, json!({"ok":true})));
}

async fn queue(fx: &HistoricalFixture) -> String {
    fx.harness.pause_issuance_for_dev();
    let queued = post(
        &fx.app,
        &fx.cookie,
        &format!("/api/cards/{}/planner/input", fx.card),
        "race-queue",
        json!({"text":"Keep the original pending message"}),
    )
    .await;
    queued["entry_id"].as_str().unwrap().to_owned()
}

async fn before_commit_refusal(delete: bool) {
    let fx = HistoricalFixture::new(false).await;
    let entry = queue(&fx).await;
    let before = fx.harness.snapshot().await.unwrap();
    let debounce = fx.harness.debounce_timestamps_set_for_test().await;
    let hard_fire = fx.harness.debounce_hard_fire_for_test().await;
    let pause = arm(&fx, test_seams::HISTORICAL_CONTROL_BEFORE_COMMIT);
    let method = if delete { "DELETE" } else { "PATCH" };
    let body = if delete {
        json!({"if_entry_rev":0})
    } else {
        json!({"if_entry_rev":0,"text":"Must not replace"})
    };
    let pending = tokio::spawn(perform(
        fx.app.clone(),
        fx.cookie.clone(),
        method,
        format!("/api/cards/{}/planner/input/{entry}", fx.card),
        body,
    ));
    entered(&pause).await;
    if delete {
        logout(&fx).await;
    } else {
        hand_area_to_alice(&fx.fixture, fx.area["id"].as_str().unwrap()).await;
    }
    pause.release.notify_one();
    let (status, body) = tokio::time::timeout(Duration::from_secs(5), pending)
        .await
        .expect("the released original control answers")
        .unwrap();
    let after = fx.harness.snapshot().await.unwrap();
    let after_debounce = fx.harness.debounce_timestamps_set_for_test().await;
    let after_hard_fire = fx.harness.debounce_hard_fire_for_test().await;
    fx.shutdown().await;
    assert_eq!(
        status,
        if delete {
            StatusCode::UNAUTHORIZED
        } else {
            StatusCode::FORBIDDEN
        },
        "{body}"
    );
    assert_eq!(after, before);
    assert_eq!(after_debounce, debounce);
    assert_eq!(after_hard_fire, hard_fire);
}

#[tokio::test]
async fn queued_delete_rechecks_logout_at_commit_and_restores_debounce() {
    before_commit_refusal(true).await;
}
#[tokio::test]
async fn queued_edit_rechecks_membership_at_commit_and_restores_revision() {
    before_commit_refusal(false).await;
}

#[tokio::test]
async fn queued_steer_rechecks_logout_after_native_gate_without_unknown_delivery() {
    let fx = HistoricalFixture::new(true).await;
    let entry = queue(&fx).await;
    let before = fx.harness.snapshot().await.unwrap();
    let debounce = fx.harness.debounce_timestamps_set_for_test().await;
    let native =
        serde_json::to_value(fx.state.shared_codex_appserver.steered_turns_for_test()).unwrap();
    let pause = arm(&fx, test_seams::HISTORICAL_STEER_NATIVE_ADMISSION);
    let pending = tokio::spawn(perform(
        fx.app.clone(),
        fx.cookie.clone(),
        "POST",
        format!("/api/cards/{}/planner/input/{entry}/steer", fx.card),
        json!({"if_entry_rev":0}),
    ));
    entered(&pause).await;
    logout(&fx).await;
    pause.release.notify_one();
    let (status, body) = tokio::time::timeout(Duration::from_secs(5), pending)
        .await
        .expect("the released original control answers")
        .unwrap();
    let after = fx.harness.snapshot().await.unwrap();
    let after_debounce = fx.harness.debounce_timestamps_set_for_test().await;
    let after_native =
        serde_json::to_value(fx.state.shared_codex_appserver.steered_turns_for_test()).unwrap();
    fx.shutdown().await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "source refusal is known unsent: {body}"
    );
    assert_eq!(after, before);
    assert_eq!(after_debounce, debounce);
    assert_eq!(after_native, native);
}

#[tokio::test]
async fn stop_membership_rejection_at_commit_preserves_all_control_facts() {
    let fx = HistoricalFixture::new(true).await;
    let before = fx.harness.snapshot().await.unwrap();
    let controls = fx.harness.historical_control_facts_for_test().await;
    let native = fx.state.shared_codex_appserver.interrupted_turns_for_test();
    let pause = arm(&fx, test_seams::HISTORICAL_CONTROL_BEFORE_COMMIT);
    let pending = tokio::spawn(perform(
        fx.app.clone(),
        fx.cookie.clone(),
        "POST",
        format!("/api/cards/{}/planner/interrupt", fx.card),
        json!({}),
    ));
    entered(&pause).await;
    hand_area_to_alice(&fx.fixture, fx.area["id"].as_str().unwrap()).await;
    pause.release.notify_one();
    let (status, body) = tokio::time::timeout(Duration::from_secs(5), pending)
        .await
        .expect("the released original control answers")
        .unwrap();
    let after = fx.harness.snapshot().await.unwrap();
    let after_controls = fx.harness.historical_control_facts_for_test().await;
    let after_native = fx.state.shared_codex_appserver.interrupted_turns_for_test();
    fx.shutdown().await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(body.get("stopped").is_none());
    assert_eq!(after, before);
    assert_eq!(after_controls, controls);
    assert_eq!(after_native, native);
}

#[tokio::test]
async fn stop_storage_rejection_preserves_all_control_facts_and_dispatches_nothing() {
    let fx = HistoricalFixture::new(true).await;
    let before = fx.harness.snapshot().await.unwrap();
    let controls = fx.harness.historical_control_facts_for_test().await;
    let native = fx.state.shared_codex_appserver.interrupted_turns_for_test();
    sqlx::query(
        "CREATE TRIGGER refuse_stop BEFORE UPDATE OF handle_state_json ON worker_sessions
        WHEN NEW.id=OLD.id AND json_extract(NEW.handle_state_json,'$.phase')='issuing_interrupt'
        BEGIN SELECT RAISE(ABORT,'fixture refuses Stop commit'); END",
    )
    .execute(fx.fixture.repo.pool())
    .await
    .unwrap();
    let (status, body) = perform(
        fx.app.clone(),
        fx.cookie.clone(),
        "POST",
        format!("/api/cards/{}/planner/interrupt", fx.card),
        json!({}),
    )
    .await;
    let after = fx.harness.snapshot().await.unwrap();
    let after_controls = fx.harness.historical_control_facts_for_test().await;
    let after_native = fx.state.shared_codex_appserver.interrupted_turns_for_test();
    sqlx::query("DROP TRIGGER refuse_stop")
        .execute(fx.fixture.repo.pool())
        .await
        .unwrap();
    fx.shutdown().await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert!(body.get("stopped").is_none());
    assert_eq!(after, before);
    assert_eq!(after_controls, controls);
    assert_eq!(after_native, native);
}

async fn complete(fx: &HistoricalFixture, turn: &str) {
    let thread = fx.harness.snapshot().await.unwrap().last_thread_id.unwrap();
    fx.state.shared_codex_appserver.emit_notification_for_test(
        crate::codex_appserver::Notification::TurnCompleted {
            thread_id: thread,
            turn: json!({"id":turn,"status":"interrupted"}),
        },
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while fx.harness.runtime_header().await.unwrap().phase != HarnessPhaseTag::TurnCompleted {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the real reducer records the original native end");
}

#[tokio::test]
async fn accepted_stop_drains_original_target_after_logout_and_ack_is_not_end() {
    let fx = HistoricalFixture::new(true).await;
    let original = fx.harness.snapshot().await.unwrap();
    let (entered, release) = fx
        .state
        .shared_codex_appserver
        .hold_turn_interrupt_for_test();
    let pending = tokio::spawn(perform(
        fx.app.clone(),
        fx.cookie.clone(),
        "POST",
        format!("/api/cards/{}/planner/interrupt", fx.card),
        json!({}),
    ));
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    let accepted = fx.harness.snapshot().await.unwrap();
    logout(&fx).await;
    release.notify_one();
    let (status, body) = tokio::time::timeout(Duration::from_secs(5), pending)
        .await
        .expect("the released original control answers")
        .unwrap();
    let acknowledged = fx.harness.snapshot().await.unwrap();
    let native = fx.state.shared_codex_appserver.interrupted_turns_for_test();
    complete(&fx, "fake-turn-0001").await;
    // The original legacy shutdown separately asks the last-known turn. ACK
    // that cleanup RPC as well; it is not the earlier business Stop or END.
    release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), fx.shutdown())
        .await
        .expect("the original cleanup RPC receives its own ACK");
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["stopped"], true);
    assert_eq!(accepted.phase, HarnessPhaseTag::IssuingInterrupt);
    assert!(matches!(
        accepted.interruption_intent,
        Some(calm_types::harness::HarnessInterruptionIntent::Control(_))
    ));
    assert_eq!(
        acknowledged.phase,
        HarnessPhaseTag::IssuingInterrupt,
        "ACK does not prove END"
    );
    assert_eq!(
        native,
        vec![(
            original.last_thread_id.unwrap(),
            original.last_turn_id.unwrap()
        )]
    );
}

#[tokio::test]
async fn accepted_assistant_stop_drains_after_real_end_archive_and_logout() {
    let mut fx = HistoricalFixture::new(false).await;
    let planner = fx.worker.clone();
    let track = fx
        .fixture
        .repo
        .card_get(&fx.card)
        .await
        .unwrap()
        .unwrap()
        .track_id;
    let created = post(
        &fx.app,
        &fx.cookie,
        &format!("/api/tracks/{track}/conversations"),
        "stop-before-archive",
        json!({"provider":"codex","text":"Finish the original Assistant reply"}),
    )
    .await;
    fx.card = created["id"].as_str().unwrap().to_owned();
    fx.worker = fx
        .fixture
        .repo
        .session_projection_active_for_card(&fx.card)
        .await
        .unwrap()
        .unwrap()
        .id;
    fx.harness = fx.state.harness.get(&fx.worker).unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let memory = fx.harness.runtime_header().await.unwrap().phase;
            let runtime = fx
                .fixture
                .repo
                .session_projection_by_id(&fx.worker)
                .await
                .unwrap()
                .unwrap();
            if memory == HarnessPhaseTag::TurnRunning && runtime.active_turn_id.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the public Assistant creation starts its original input");
    let original = fx.harness.snapshot().await.unwrap();
    let thread = original.last_thread_id.clone().unwrap();
    let turn = original.last_turn_id.clone().unwrap();
    let (entered, release) = fx
        .state
        .shared_codex_appserver
        .hold_turn_interrupt_for_test();
    let pending = tokio::spawn(perform(
        fx.app.clone(),
        fx.cookie.clone(),
        "POST",
        format!("/api/cards/{}/planner/interrupt", fx.card),
        json!({}),
    ));
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .expect("the original Stop reaches its native RPC after acceptance");
    let accepted = fx.harness.snapshot().await.unwrap();

    // The external fake reports the same real END to both native activity and
    // the original reducer, matching main's archive fixture. No phase is set.
    fx.state
        .shared_codex_appserver
        .clear_active_turn_for_test(&thread);
    complete(&fx, &turn).await;
    let archive = fx
        .app
        .clone()
        .oneshot(
            Request::patch(format!(
                "/api/tracks/{track}/conversations/{}/archive",
                fx.card
            ))
            .header(header::COOKIE, &fx.cookie)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(json!({"archived":true}).to_string()))
            .unwrap(),
        )
        .await
        .unwrap();
    let archive_status = archive.status();
    let input = fx
        .app
        .clone()
        .oneshot(
            Request::post(format!("/api/cards/{}/planner/input", fx.card))
                .header(header::COOKIE, &fx.cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .header("Idempotency-Key", "refuse-work-after-archive")
                .body(Body::from(
                    json!({"text":"Must not create work in an archived conversation"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let input_status = input.status();
    let input_body: Value =
        serde_json::from_slice(&input.into_body().collect().await.unwrap().to_bytes()).unwrap();
    logout(&fx).await;
    release.notify_one();
    let (status, body) = tokio::time::timeout(Duration::from_secs(5), pending)
        .await
        .expect("archive and logout cannot revoke the accepted original drain")
        .unwrap();
    let after = fx.harness.snapshot().await.unwrap();
    let stored = fx
        .fixture
        .repo
        .session_projection_active_for_card(&fx.card)
        .await
        .unwrap()
        .unwrap();
    let archived =
        crate::db::RepoRead::conversation_archived_at(fx.fixture.repo.as_ref(), &fx.card)
            .await
            .unwrap();
    let native = fx.state.shared_codex_appserver.interrupted_turns_for_test();
    let starts = fx.state.shared_codex_appserver.turn_start_count_for_test();
    release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), fx.shutdown())
        .await
        .expect("the separate original cleanup RPC receives its own ACK");
    fx.state
        .harness
        .remove(&planner)
        .unwrap()
        .shutdown()
        .await
        .unwrap();

    assert_eq!(archive_status, StatusCode::NO_CONTENT);
    assert!(archived.is_some());
    assert_eq!(input_status, StatusCode::CONFLICT, "{input_body}");
    assert!(input_body["error"].as_str().unwrap().contains("archived"));
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["stopped"], true);
    assert_eq!(accepted.phase, HarnessPhaseTag::IssuingInterrupt);
    let Some(calm_types::harness::HarnessInterruptionIntent::Control(
        calm_types::harness::HarnessControlIntent::UserStopV1(request),
    )) = accepted.interruption_intent
    else {
        panic!("the original writer must commit its exact UserStopV1 acceptance");
    };
    assert_eq!(request.session_id, fx.worker);
    assert_eq!(request.card_id, fx.card);
    assert_eq!(request.thread_id, thread);
    assert_eq!(request.turn_id, turn);
    assert_eq!(after.phase, HarnessPhaseTag::TurnCompleted);
    assert_eq!(after.last_thread_id.as_deref(), Some(thread.as_str()));
    assert_eq!(after.last_turn_id.as_deref(), Some(turn.as_str()));
    assert_eq!(stored.id, fx.worker);
    assert_eq!(stored.thread_id.as_deref(), Some(thread.as_str()));
    assert!(stored.active_turn_id.is_none());
    assert_eq!(native, vec![(thread, turn)]);
    assert_eq!(starts, 1, "archive and Stop cannot start later work");
}

#[tokio::test]
async fn selected_stop_cannot_retarget_a_later_turn_while_waiting_for_its_owner() {
    let fx = HistoricalFixture::new(true).await;
    let pause = arm(&fx, test_seams::HISTORICAL_STOP_SELECTED);
    let pending = tokio::spawn(perform(
        fx.app.clone(),
        fx.cookie.clone(),
        "POST",
        format!("/api/cards/{}/planner/interrupt", fx.card),
        json!({}),
    ));
    entered(&pause).await;
    complete(&fx, "fake-turn-0001").await;
    post(
        &fx.app,
        &fx.cookie,
        &format!("/api/cards/{}/planner/input", fx.card),
        "new-turn",
        json!({"text":"A genuinely later turn"}),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let snapshot = fx.harness.snapshot().await.unwrap();
            if snapshot.phase == HarnessPhaseTag::TurnRunning
                && snapshot.last_turn_id.as_deref() == Some("fake-turn-0002")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    pause.release.notify_one();
    let (status, body) = tokio::time::timeout(Duration::from_secs(5), pending)
        .await
        .expect("the released original control answers")
        .unwrap();
    let after = fx.harness.snapshot().await.unwrap();
    let native = fx.state.shared_codex_appserver.interrupted_turns_for_test();
    fx.shutdown().await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body.get("stopped").is_none());
    assert_eq!(after.phase, HarnessPhaseTag::TurnRunning);
    assert_eq!(after.last_turn_id.as_deref(), Some("fake-turn-0002"));
    assert!(native.is_empty());
}

#[tokio::test]
async fn stop_phase_event_follows_committed_memory_publication() {
    let fx = HistoricalFixture::new(true).await;
    let pause = arm(&fx, test_seams::HISTORICAL_STOP_COMMITTED);
    let mut events = fx.state.events.subscribe();
    let pending = tokio::spawn(perform(
        fx.app.clone(),
        fx.cookie.clone(),
        "POST",
        format!("/api/cards/{}/planner/interrupt", fx.card),
        json!({}),
    ));
    entered(&pause).await;
    let mut early = false;
    while let Ok(event) = events.try_recv() {
        if matches!(
            event.event,
            crate::event::Event::HarnessPhaseChanged {
                new_phase: HarnessPhaseTag::IssuingInterrupt,
                ..
            }
        ) {
            early = true;
        }
    }
    pause.release.notify_one();
    let (status, body) = tokio::time::timeout(Duration::from_secs(5), pending)
        .await
        .expect("the released original control answers")
        .unwrap();
    let (_, run) = perform(
        fx.app.clone(),
        fx.cookie.clone(),
        "GET",
        format!(
            "/api/cards/{}/planner/run?contract=conversation_v1",
            fx.card
        ),
        Value::Null,
    )
    .await;
    complete(&fx, "fake-turn-0001").await;
    fx.shutdown().await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        !early,
        "an event must not invite a GET while the committed phase is still unpublished"
    );
    assert_eq!(run["phase"], "issuing_interrupt");
}

#[tokio::test]
async fn historical_stop_refuses_unconfirmed_issuance_without_claiming_acceptance() {
    let fx = HistoricalFixture::new(false).await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    fx.state
        .shared_codex_appserver
        .install_turn_start_return_hook_for_test(
            crate::shared_codex_appserver::TurnStartReturnHook {
                entered: entered.clone(),
                release: release.clone(),
            },
        );
    post(
        &fx.app,
        &fx.cookie,
        &format!("/api/cards/{}/planner/input", fx.card),
        "unknown-stop-target",
        json!({"text":"Wait for this exact native turn to be confirmed"}),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .expect("the original native start has not yet answered");
    let before = fx.harness.snapshot().await.unwrap();
    let (status, body) = perform(
        fx.app.clone(),
        fx.cookie.clone(),
        "POST",
        format!("/api/cards/{}/planner/interrupt", fx.card),
        json!({}),
    )
    .await;
    let after = fx.harness.snapshot().await.unwrap();
    let native = fx.state.shared_codex_appserver.interrupted_turns_for_test();
    release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), async {
        while fx.harness.runtime_header().await.unwrap().phase != HarnessPhaseTag::TurnRunning {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the original start can still complete after refused Stop");
    fx.shutdown().await;
    assert_eq!(before.phase, HarnessPhaseTag::IssuingTurn);
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body.get("stopped").is_none());
    assert_eq!(after, before);
    assert!(native.is_empty());
}

async fn repeated_human_stop(selected_before_acceptance: bool) {
    let fx = HistoricalFixture::new(true).await;
    let uri = format!("/api/cards/{}/planner/interrupt", fx.card);
    let pending = if selected_before_acceptance {
        let pause = arm(&fx, test_seams::HISTORICAL_STOP_SELECTED);
        let pending = tokio::spawn(perform(
            fx.app.clone(),
            fx.cookie.clone(),
            "POST",
            uri.clone(),
            json!({}),
        ));
        entered(&pause).await;
        Some((pending, pause))
    } else {
        None
    };
    let (first_status, first_body) = perform(
        fx.app.clone(),
        fx.cookie.clone(),
        "POST",
        uri.clone(),
        json!({}),
    )
    .await;
    let accepted = fx.harness.snapshot().await.unwrap();
    let facts = fx.harness.historical_control_facts_for_test().await;
    let first_native = fx.state.shared_codex_appserver.interrupted_turns_for_test();
    let (repeat_status, repeat_body) = if let Some((pending, pause)) = pending {
        pause.release.notify_one();
        tokio::time::timeout(Duration::from_secs(5), pending)
            .await
            .expect("the already-selected request must settle")
            .unwrap()
    } else {
        perform(fx.app.clone(), fx.cookie.clone(), "POST", uri, json!({})).await
    };
    let after = fx.harness.snapshot().await.unwrap();
    let after_facts = fx.harness.historical_control_facts_for_test().await;
    let native = fx.state.shared_codex_appserver.interrupted_turns_for_test();
    fx.shutdown().await;
    assert_eq!(first_status, StatusCode::OK, "{first_body}");
    assert_eq!(first_body["stopped"], true);
    assert_eq!(accepted.phase, HarnessPhaseTag::IssuingInterrupt);
    assert_eq!(first_native.len(), 1);
    assert_eq!(repeat_status, StatusCode::OK, "{repeat_body}");
    assert_eq!(repeat_body["stopped"], false);
    assert_eq!(
        after, accepted,
        "the first accepted intent remains authoritative"
    );
    assert_eq!(after_facts, facts, "the original deadline is not extended");
    assert_eq!(native, first_native, "ACK without END is not a new Stop");
}

#[tokio::test]
async fn repeated_human_stop_preserves_the_accepted_intent_after_ack_without_end() {
    repeated_human_stop(false).await;
}

#[tokio::test]
async fn selected_human_stop_cannot_replace_an_intent_accepted_while_it_waited() {
    repeated_human_stop(true).await;
}

#[tokio::test]
async fn human_stop_rechecks_accepted_interrupt_in_the_original_snapshot_transaction() {
    let fx = HistoricalFixture::new(true).await;
    let pause = arm(&fx, test_seams::HISTORICAL_CONTROL_BEFORE_COMMIT);
    let pending = tokio::spawn(perform(
        fx.app.clone(),
        fx.cookie.clone(),
        "POST",
        format!("/api/cards/{}/planner/interrupt", fx.card),
        json!({}),
    ));
    entered(&pause).await;
    // The original trusted cleanup owner really commits an interruption after
    // Human preflight. It does not run through the paused public command.
    fx.harness.interrupt("kernel_cleanup".into()).await.unwrap();
    let accepted = fx.harness.snapshot().await.unwrap();
    let facts = fx.harness.historical_control_facts_for_test().await;
    let accepted_native = fx.state.shared_codex_appserver.interrupted_turns_for_test();
    pause.release.notify_one();
    let (status, body) = tokio::time::timeout(Duration::from_secs(5), pending)
        .await
        .expect("the original snapshot writer settles its selected request")
        .unwrap();
    let after = fx.harness.snapshot().await.unwrap();
    let after_facts = fx.harness.historical_control_facts_for_test().await;
    let native = fx.state.shared_codex_appserver.interrupted_turns_for_test();
    fx.shutdown().await;
    assert_eq!(accepted.phase, HarnessPhaseTag::IssuingInterrupt);
    assert_eq!(accepted_native.len(), 1);
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["stopped"], false);
    assert_eq!(after, accepted);
    assert_eq!(after_facts, facts, "the accepted deadline stays unchanged");
    assert_eq!(native, accepted_native);
}
