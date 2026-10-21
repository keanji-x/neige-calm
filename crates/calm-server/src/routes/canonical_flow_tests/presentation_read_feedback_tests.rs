//! Display actual owning execution feedback; no reader acquires execution authority.
use super::*;

pub(super) async fn read_run(app: &axum::Router, cookie: &str, card: &str) -> Value {
    let response = app
        .clone()
        .oneshot(
            Request::get(format!(
                "/api/cards/{card}/planner/run?contract=conversation_v1"
            ))
            .header(header::COOKIE, cookie)
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

async fn save_frame(fixture: &Fixture, worker: &str, frame: &CanonicalHarnessSnapshot) {
    let worker = worker.to_owned();
    let mut tx = crate::db::sqlite::begin_immediate_tx(fixture.repo.db())
        .await
        .unwrap();
    assert!(
        crate::db::sqlite::session_set_handle_state_tx(
            &mut tx,
            &worker,
            Some(serde_json::to_value(frame).unwrap())
        )
        .await
        .unwrap()
    );
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn canonical_run_reads_the_actual_live_source_refusal_for_the_original_unattempted_claim() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let area = post(
        &app,
        &cookie,
        "/api/areas",
        "source-feedback-area",
        json!({"name":"Source feedback","color":"#123456"}),
    )
    .await;
    let (_client, mut peer) = connect(&state.shared_codex_appserver).await;
    let created = post(
        &app,
        &cookie,
        "/api/tracks",
        "source-feedback",
        json!({"area_id":area["id"],"planner_provider":"codex","title":"",
            "theme":{"fg":[255,255,255],"bg":[0,0,0]},"first_message":FIRST_BODY}),
    )
    .await;
    let track = created["id"].as_str().unwrap();
    let (card, worker, _) = fixture.carrier(track).await;
    // The real creation and native thread setup commit the claim. Fault only
    // its first crossing marker; the fixture never manufactures a claim.
    sqlx::query(&format!("CREATE TRIGGER hold_read_feedback_marker BEFORE UPDATE OF handle_state_json ON worker_sessions
        WHEN NEW.id='{worker}' AND json_array_length(NEW.handle_state_json,'$.canonical_inputs.attempts') > 0
        BEGIN SELECT RAISE(ABORT,'fixture holds the original crossing marker'); END"))
        .execute(fixture.repo.pool()).await.unwrap();
    let original =
        super::source_readmission_tests::wait_for_claim(&fixture, track, &mut peer).await;
    post(
        &app,
        &cookie,
        "/api/auth/logout",
        "source-feedback-logout",
        json!({}),
    )
    .await;
    let cookie = super::source_readmission_tests::login_again(&app).await;
    sqlx::query("DROP TRIGGER hold_read_feedback_marker")
        .execute(fixture.repo.pool())
        .await
        .unwrap();
    let handle = state
        .harness
        .get(&worker)
        .expect("actual original live handle");
    let reason = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(reason) = handle.issuance_block().await
                && reason.contains("session")
            {
                break reason;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the actual source-refusal owner must publish safe feedback");
    let before = fixture.carrier(track).await.2;
    assert_eq!(before.canonical_inputs, original.canonical_inputs);
    assert!(before.canonical_inputs.attempts.is_empty());
    // A new verified login only reads. GET never supplies execution authority.
    let run = read_run(&app, &cookie, &card).await;
    assert_eq!(run["blocked_reason"], reason, "{run}");
    assert!(run["active_input"]["submissions"][0]["attempt"].is_null());
    assert_eq!(fixture.carrier(track).await.2, before);
    handle.shutdown().await.unwrap();
    peer.close(None).await.unwrap();
}

#[tokio::test]
async fn canonical_run_reads_only_the_exact_current_terminal_timeout_and_still_saves() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let area = post(
        &app,
        &cookie,
        "/api/areas",
        "timeout-read-area",
        json!({"name":"Timeout feedback","color":"#123456"}),
    )
    .await;
    let track = post(
        &app,
        &cookie,
        "/api/tracks",
        "timeout-read",
        json!({"area_id":area["id"],"planner_provider":"codex","title":"",
            "theme":{"fg":[255,255,255],"bg":[0,0,0]},"first_message":FIRST_BODY}),
    )
    .await;
    let track_id = track["id"].as_str().unwrap();
    let (card, worker, mut frame) = fixture.carrier(track_id).await;
    // A persisted read fixture, not a claim of real native END. Mint protected
    // evidence through the existing terminal recorder and preserve original input.
    let request = calm_types::harness::HarnessTimeoutRequest {
        session_id: worker.clone(),
        card_id: card.clone(),
        track_id: track_id.into(),
        thread_id: THREAD.into(),
        turn_id: TURN.into(),
        reason: calm_types::harness::HarnessInterruptionReason::MaxTurnDuration,
        requested_at_ms: 1,
        effective_elapsed_ms: 61000,
        budget_ms: 60000,
    };
    frame.header.phase = HarnessPhaseTag::TurnCompleted;
    frame.header.last_thread_id = Some(THREAD.into());
    frame.header.last_turn_id = Some(TURN.into());
    frame.header.interruption_intent =
        Some(calm_types::harness::HarnessInterruptionIntent::Versioned(
            calm_types::harness::HarnessTimeoutIntent::V1(request.clone()),
        ));
    save_frame(&fixture, &worker, &frame).await;
    crate::harness::turn_outcome::record(
        fixture.repo.as_ref(),
        &worker,
        &card,
        track_id,
        THREAD,
        TURN,
        &json!({"id":TURN,"status":"interrupted"}),
        crate::harness::turn_outcome::Confirmation::ProviderTerminal,
    )
    .await
    .unwrap();
    frame.header.interruption_intent = None;
    save_frame(&fixture, &worker, &frame).await;
    let run = read_run(&app, &cookie, &card).await;
    assert_eq!(run["runtime_state"], "idle");
    assert_eq!(run["phase"], "turn_completed");
    assert_eq!(
        run["terminal_timeout"]["request"],
        serde_json::to_value(&request).unwrap()
    );
    assert_eq!(run["history_replacement_supported"], false);
    // Deliberately malformed historical attribution uses the raw history writer.
    for changed in ["session", "card", "track", "thread", "turn"] {
        let mut other = request.clone();
        match changed {
            "session" => other.session_id = "old-session".into(),
            "card" => other.card_id = "other-card".into(),
            "track" => other.track_id = "other-track".into(),
            "thread" => other.thread_id = "old-thread".into(),
            "turn" => other.turn_id = "old-turn".into(),
            _ => unreachable!(),
        }
        let evidence = calm_types::harness::HarnessTimeoutEvidence::V1 {
            request: other.clone(),
            ended_at_ms: 2,
        };
        fixture.repo.fixture_transcript_insert(&other.session_id, &card, track_id, &other.thread_id, Some(&other.turn_id), None, None, "turn/completed",
            &json!({"id":other.turn_id,"status":"interrupted","harness_interruption_reason":evidence}).to_string(), None)
            .await.unwrap();
        assert_eq!(
            read_run(&app, &cookie, &card).await["terminal_timeout"],
            Value::Null,
            "{changed}"
        );
    }
    let saved = post(
        &app,
        &cookie,
        &format!("/api/cards/{card}/planner/input"),
        "after-timeout",
        json!({"text":"A confirmed timeout does not refuse future content"}),
    )
    .await;
    assert_eq!(saved["worker_session_id"], worker);
    assert_eq!(fixture.counts(&card).await.0, 2);
    assert_eq!(
        fixture
            .carrier(track_id)
            .await
            .2
            .canonical_inputs
            .pending
            .len(),
        2
    );
    assert!(
        state.harness.get(&worker).is_none(),
        "read never manufactures a live runtime"
    );
}
