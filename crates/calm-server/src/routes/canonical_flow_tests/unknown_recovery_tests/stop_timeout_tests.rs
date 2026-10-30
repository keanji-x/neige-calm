//! Recover a real accepted Stop after restart loses its original END owner.
use super::*;

const QUEUED: &str = "Saved Human input after the original Stop deadline";
const FRESH: &str = "New authorized Human input after Stop recovery";

#[tokio::test]
async fn canonical_acp_stop_timeout_restart_has_a_supported_human_exit() {
    Box::pin(stop_timeout_recovery()).await;
}

async fn stop_timeout_recovery() {
    let mut fixture = Fixture::new().await;
    opencode_creation_tests::configure(&mut fixture);
    std::fs::write(fixture.root.path().join("scenario"), "hold").unwrap();
    let (state, app, cookie) = fixture.application().await;
    let area = post(
        &app,
        &cookie,
        "/api/areas",
        "area",
        json!({"name":"Stop restart recovery","color":"#123456"}),
    )
    .await;
    let created = post(
        &app,
        &cookie,
        "/api/tracks",
        "first",
        json!({
            "area_id":area["id"],"title":"","planner_provider":"opencode",
            "theme":{"fg":[255,255,255],"bg":[0,0,0]},"first_message":FIRST_BODY
        }),
    )
    .await;
    let track = created["id"].as_str().unwrap().to_owned();
    fixture
        .wait_phase(&track, HarnessPhaseTag::TurnRunning)
        .await;
    let (card, worker, original) = fixture.carrier(&track).await;
    // TurnRunning precedes the actual prompt write. Observe peer consumption
    // before losing the driver so the no-resend assertion has a real original.
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let log = std::fs::read_to_string(fixture.root.path().join("requests.jsonl")).unwrap();
            if log
                .lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .any(|call| {
                    call["method"] == "session/prompt"
                        && call["params"]["prompt"].to_string().contains(FIRST_BODY)
                })
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the native peer must consume the original prompt before driver loss");
    let original_ack = fixture.receipt(area["id"].as_str().unwrap()).await;
    let harness = state.harness.get(&worker).unwrap();
    let session = harness.acp_session_for_test().unwrap();
    let unsent = crate::acp_planner::test_seams::pause_before_interrupt_send(&worker);
    let stopping = tokio::spawn({
        let app = app.clone();
        let cookie = cookie.clone();
        let uri = format!("/api/cards/{card}/planner/interrupt");
        async move { status(&app, &cookie, &uri, "stop", json!({})).await }
    });
    tokio::time::timeout(Duration::from_secs(20), unsent.entered.notified())
        .await
        .unwrap();
    let accepted = fixture.carrier(&track).await.2;
    assert_eq!(accepted.header.phase, HarnessPhaseTag::IssuingInterrupt);
    assert!(accepted.header.interruption_intent.is_some());
    harness.expire_interrupt_deadline_for_test().await;
    fixture.wait_phase(&track, HarnessPhaseTag::Wedged).await;
    post(
        &app,
        &cookie,
        &format!("/api/cards/{card}/planner/input"),
        "queued",
        json!({"text":QUEUED}),
    )
    .await;
    let saved = fixture.carrier(&track).await.2;
    assert_eq!(
        saved.header.wedged_reason.as_deref(),
        Some(calm_types::harness::HARNESS_INTERRUPT_TIMEOUT_REASON)
    );
    assert_eq!(
        saved.canonical_inputs.active,
        original.canonical_inputs.active
    );
    assert_eq!(saved.canonical_inputs.pending.len(), 1);

    // Lose the real driver while its accepted cancel is still held, so it can
    // never supply native END or a genuine normal-join witness after restart.
    session.abort_driver_for_test().await;
    tokio::time::timeout(Duration::from_secs(20), async {
        while !session.driver_finished_for_test() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    drop(unsent);
    assert_eq!(stopping.await.unwrap(), StatusCode::CONFLICT);
    // The production shutdown/registered execution owner may prove cleanup;
    // an aborted join still cannot fabricate a native completion receipt.
    let _ = session.shutdown().await;
    let _ = state.harness.shutdown_and_remove(&worker).await;
    fixture.shutdown_process(&state).await;
    drop((session, harness, state, app));
    fixture.repo.pool().close().await;
    fixture.repo = Arc::new(SqlxRepo::open(&fixture.url).await.unwrap());
    let (state, app, cookie) = fixture.application().await;
    let reopened = fixture.carrier(&track).await.2;
    assert_eq!(reopened.header.phase, HarnessPhaseTag::Wedged);
    assert_eq!(
        reopened.header.interruption_intent,
        saved.header.interruption_intent
    );
    assert_eq!(reopened.canonical_inputs, saved.canonical_inputs);
    let witness_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM acp_normal_joins WHERE worker_session_id=?1")
            .bind(&worker)
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
    assert_eq!(witness_count, 0, "driver loss is not a genuine normal join");
    let receipt: (String, String, Option<String>) = sqlx::query_as(
        "SELECT input_json,state,outcome_json FROM acp_submissions WHERE worker_session_id=?1",
    )
    .bind(&worker)
    .fetch_one(fixture.repo.pool())
    .await
    .unwrap();
    assert_eq!(receipt.1, "unknown");
    assert!(receipt.2.is_none());
    let run = super::super::presentation_read_feedback_tests::read_run(&app, &cookie, &card).await;
    assert_eq!(
        run["recovery"]["action"], "abandon_session",
        "a restarted interrupt_timeout must have a supported Human exit"
    );
    let request = run["recovery"]["request"].clone();
    let uri = format!("/api/cards/{card}/conversation/abandon");
    // Each single-factor counterexample retains the real original Unknown and
    // calls the same public entry point. None may retire the original carrier.
    let baseline = serde_json::to_value(&reopened).unwrap();
    for path in [
        vec!["wedged_reason"],
        vec!["interruption_intent"],
        vec!["interruption_intent", "session_id"],
        vec!["interruption_intent", "card_id"],
        vec!["interruption_intent", "track_id"],
        vec!["interruption_intent", "thread_id"],
        vec!["interruption_intent", "turn_id"],
        vec!["last_thread_id"],
        vec!["last_turn_id"],
    ] {
        let mut changed = baseline.clone();
        let mut field = &mut changed;
        for key in &path {
            field = &mut field[*key];
        }
        *field = if path == ["interruption_intent"] {
            Value::Null
        } else {
            json!("foreign")
        };
        sqlx::query("UPDATE worker_sessions SET handle_state_json=?1 WHERE id=?2")
            .bind(changed.to_string())
            .bind(&worker)
            .execute(fixture.repo.pool())
            .await
            .unwrap();
        let held =
            super::super::presentation_read_feedback_tests::read_run(&app, &cookie, &card).await;
        assert_eq!(held["recovery"]["action"], "held", "{path:?}");
        assert_eq!(
            status(&app, &cookie, &uri, "invalid-stop", request.clone()).await,
            StatusCode::CONFLICT,
            "{path:?}"
        );
    }
    sqlx::query("UPDATE worker_sessions SET handle_state_json=?1 WHERE id=?2")
        .bind(baseline.to_string())
        .bind(&worker)
        .execute(fixture.repo.pool())
        .await
        .unwrap();
    let response = post(&app, &cookie, &uri, "recover-stop", request.clone()).await;
    let fresh = response["carrier_id"].as_str().unwrap().to_owned();
    assert_ne!(fresh, worker);
    assert_eq!(response["old_carrier_id"], worker);
    assert_eq!(
        post(&app, &cookie, &uri, "recover-stop", request).await,
        response
    );
    let replacement = fixture.carrier(&track).await.2;
    assert_eq!(
        replacement.canonical_inputs.pending,
        saved.canonical_inputs.pending
    );
    assert_eq!(
        replacement.header.push_watermark,
        saved.header.push_watermark
    );
    assert_eq!(
        fixture.receipt(area["id"].as_str().unwrap()).await,
        original_ack
    );
    let after: (String, String, Option<String>) = sqlx::query_as(
        "SELECT input_json,state,outcome_json FROM acp_submissions WHERE worker_session_id=?1",
    )
    .bind(&worker)
    .fetch_one(fixture.repo.pool())
    .await
    .unwrap();
    assert_eq!(
        after, receipt,
        "retirement preserves the original Unknown receipt"
    );
    let old: Value = sqlx::query_scalar::<_, String>(
        "SELECT handle_state_json FROM worker_sessions WHERE id=?1",
    )
    .bind(&worker)
    .fetch_one(fixture.repo.pool())
    .await
    .map(|raw| serde_json::from_str(&raw).unwrap())
    .unwrap();
    let old: CanonicalHarnessSnapshot = serde_json::from_value(old).unwrap();
    assert_eq!(old.canonical_inputs, saved.canonical_inputs);
    assert_eq!(
        old.header.interruption_intent,
        saved.header.interruption_intent
    );

    std::fs::write(fixture.root.path().join("scenario"), "reply").unwrap();
    post(
        &app,
        &cookie,
        &format!("/api/cards/{card}/planner/input"),
        "fresh",
        json!({"text":FRESH}),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let frame = fixture.carrier(&track).await.2;
            if frame.canonical_inputs.active.is_none()
                && frame.canonical_inputs.pending.is_empty()
                && !frame.canonical_inputs.attempts.is_empty()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let prompts: Vec<Value> = std::fs::read_to_string(fixture.root.path().join("requests.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|record| record["method"] == "session/prompt")
        .collect();
    let bodies: Vec<String> = prompts
        .iter()
        .map(|record| record["params"]["prompt"].to_string())
        .collect();
    for text in [FIRST_BODY, QUEUED, FRESH] {
        assert_eq!(
            bodies.iter().filter(|body| body.contains(text)).count(),
            1,
            "each accepted input reaches native once: {text}"
        );
    }
    assert_ne!(
        prompts[0]["params"]["sessionId"],
        prompts[1]["params"]["sessionId"]
    );
    let queued = bodies
        .iter()
        .position(|body| body.contains(QUEUED))
        .unwrap();
    let new = bodies.iter().position(|body| body.contains(FRESH)).unwrap();
    assert!(queued <= new);
    if queued == new {
        assert!(bodies[queued].find(QUEUED) < bodies[queued].find(FRESH));
    }
    state.harness.shutdown_and_remove(&fresh).await.unwrap();
    fixture.shutdown_process(&state).await;
}
