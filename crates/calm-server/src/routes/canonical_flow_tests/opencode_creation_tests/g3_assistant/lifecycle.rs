use super::*;

#[tokio::test]
async fn g3_grok_assistant_outage_replay_no_wake_then_fresh_input() {
    for auth_failure in [false, true] {
        outage(auth_failure).await;
    }
}

async fn outage(auth_failure: bool) {
    let mut fixture = Fixture::new().await;
    if auth_failure {
        configure(&mut fixture);
        // Signing in changes the external agent's auth state, not its registration digest.
        std::fs::write(
            peer_root(&fixture, &AgentProvider::Grok).join("scenario"),
            "grok-auth-error",
        )
        .unwrap();
    }
    let (state, app, cookie) = fixture.application().await;
    let track = track(&app, &cookie).await;
    let uri = format!("/api/tracks/{track}/conversations");
    let card = crate::conversation_keys::derive_track_conversation_keys(&track, "outage").card_id;
    let seam = crate::test_seams::PausePoint::default();
    crate::test_seams::install_pause_for_test("canonical-human-wake-finished", &card, seam.clone());
    let body = json!({"provider":"grok","text":FIRST_BODY});
    let created = post(&app, &cookie, &uri, "outage", body.clone()).await;
    assert_eq!(facts(&fixture, &card).await, (1, 1, 1, 1, 1, 1));
    tokio::time::timeout(Duration::from_secs(5), seam.entered.notified())
        .await
        .unwrap();
    seam.release.notify_one();
    // Wake completion means the harness was scheduled, not that its asynchronous
    // authentication preflight has settled. Freeze only the durable failure fact.
    if auth_failure {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let (_, frame) = carrier(&fixture, &card).await;
                if frame.header.phase == HarnessPhaseTag::Idle
                    && frame.canonical_inputs.active.is_none()
                    && frame
                        .canonical_inputs
                        .attempts
                        .last()
                        .is_some_and(|attempt| {
                            matches!(attempt.delivery, InputDeliveryFact::NotDispatched { .. })
                        })
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("authentication refusal must settle without a native prompt");
    }
    let (worker, frame) = carrier(&fixture, &card).await;
    assert_eq!(frame.canonical_inputs.pending.len(), 1);
    assert_eq!(
        frame.canonical_inputs.attempts.len(),
        usize::from(auth_failure)
    );
    assert_eq!(prompts(&fixture, &AgentProvider::Grok), 0);
    state.harness.shutdown_and_remove(&worker).await.unwrap();
    fixture.shutdown_process(&state).await;
    drop((app, state));
    fixture.repo = Arc::new(SqlxRepo::open(&fixture.url).await.unwrap());
    configure(&mut fixture);
    let (state, app, cookie) = fixture.application().await;
    let replay = no_wake(&app, &cookie, &uri, "outage", body, &card).await;
    assert_eq!(replay["initialInput"], created["initialInput"]);
    assert_eq!(prompts(&fixture, &AgentProvider::Grok), 0);
    assert_eq!(
        carrier(&fixture, &card).await.1.canonical_inputs,
        frame.canonical_inputs
    );
    post(
        &app,
        &cookie,
        &format!("/api/cards/{card}/planner/input"),
        "wake",
        json!({"text":"fresh Human activation"}),
    )
    .await;
    idle(&fixture, &card, frame.canonical_inputs.attempts.len() + 2).await;
    assert_eq!(carrier(&fixture, &card).await.0, worker);
    assert_eq!(prompts(&fixture, &AgentProvider::Grok), 2);
    assert_eq!(
        calls(&fixture)
            .iter()
            .filter(|c| c["method"] == "session/prompt"
                && c["params"]["prompt"].to_string().contains(FIRST_BODY))
            .count(),
        1
    );
    fixture.shutdown_process(&state).await;
}

#[tokio::test]
async fn g3_grok_assistant_lost_reply_cold_boot_no_resend() {
    let mut fixture = Fixture::new().await;
    configure(&mut fixture);
    std::fs::write(
        peer_root(&fixture, &AgentProvider::Grok).join("scenario"),
        "lost",
    )
    .unwrap();
    let (state, app, cookie) = fixture.application().await;
    let track = track(&app, &cookie).await;
    let uri = format!("/api/tracks/{track}/conversations");
    let body = json!({"provider":"grok","text":FIRST_BODY});
    let created = post(&app, &cookie, &uri, "lost", body.clone()).await;
    let card = created["id"].as_str().unwrap();
    assert_eq!(facts(&fixture, card).await, (1, 1, 1, 1, 1, 1));
    let (worker, before) = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let current = carrier(&fixture, card).await;
            let receipt: Option<String> =
                sqlx::query_scalar("SELECT state FROM acp_submissions WHERE worker_session_id=?1")
                    .bind(&current.0)
                    .fetch_optional(fixture.repo.pool())
                    .await
                    .unwrap();
            if receipt.as_deref() == Some("unknown")
                && current
                    .1
                    .canonical_inputs
                    .attempts
                    .first()
                    .is_some_and(|a| matches!(a.delivery, InputDeliveryFact::Unknown))
            {
                break current;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(prompts(&fixture, &AgentProvider::Grok), 1);
    let original = native_receipts(&fixture, &worker).await;
    state.harness.shutdown_and_remove(&worker).await.unwrap();
    fixture.shutdown_process(&state).await;
    drop((app, state));
    fixture.repo = Arc::new(SqlxRepo::open(&fixture.url).await.unwrap());
    let (state, app, cookie) = fixture.application().await;
    let replay = no_wake(&app, &cookie, &uri, "lost", body, card).await;
    assert_eq!(replay["initialInput"], created["initialInput"]);
    assert_eq!(
        initial(&app, &cookie, &track, card, "lost").await["receipt"],
        created["initialInput"]["receipt"]
    );
    let seam = crate::test_seams::PausePoint::default();
    crate::test_seams::install_pause_for_test("canonical-human-wake-finished", card, seam.clone());
    post(
        &app,
        &cookie,
        &format!("/api/cards/{card}/planner/input"),
        "held",
        json!({"text":"held behind original Unknown"}),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(10), seam.entered.notified())
        .await
        .unwrap();
    seam.release.notify_one();
    let (current, after) = carrier(&fixture, card).await;
    assert_eq!(current, worker);
    assert_eq!(
        after.canonical_inputs.attempts,
        before.canonical_inputs.attempts
    );
    assert_eq!(native_receipts(&fixture, &worker).await, original);
    assert_eq!(prompts(&fixture, &AgentProvider::Grok), 1);
    fixture.shutdown_process(&state).await;
}

async fn native_receipts(
    fixture: &Fixture,
    worker: &str,
) -> Vec<(String, String, String, Option<String>)> {
    sqlx::query_as("SELECT turn_id,input_json,state,outcome_json FROM acp_submissions WHERE worker_session_id=?1 ORDER BY client_id")
        .bind(worker).fetch_all(fixture.repo.pool()).await.unwrap()
}

#[tokio::test]
async fn g3_grok_assistant_cold_boot_loads_original_worker() {
    continuation("reply").await;
}

#[tokio::test]
async fn g3_grok_assistant_load_refusal_and_registration_drift_no_replacement() {
    for scenario in ["unsupported-load", "load-error", "registration-drift"] {
        continuation(scenario).await;
    }
}

async fn continuation(scenario: &str) {
    let mut fixture = Fixture::new().await;
    configure(&mut fixture);
    let (state, app, cookie) = fixture.application().await;
    let track = track(&app, &cookie).await;
    let uri = format!("/api/tracks/{track}/conversations");
    let created = post(
        &app,
        &cookie,
        &uri,
        "cold",
        json!({"provider":"grok","text":FIRST_BODY}),
    )
    .await;
    let card = created["id"].as_str().unwrap();
    assert_eq!(facts(&fixture, card).await, (1, 1, 1, 1, 1, 1));
    idle(&fixture, card, 1).await;
    let (worker, _) = carrier(&fixture, card).await;
    let identity: (String, String) =
        sqlx::query_as("SELECT thread_id,agent_session_id FROM worker_sessions WHERE id=?1")
            .bind(&worker)
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
    let original = native_receipts(&fixture, &worker).await;
    state.harness.shutdown_and_remove(&worker).await.unwrap();
    fixture.shutdown_process(&state).await;
    drop((app, state));
    std::fs::write(
        peer_root(&fixture, &AgentProvider::Grok).join("scenario"),
        scenario,
    )
    .unwrap();
    fixture.repo = Arc::new(SqlxRepo::open(&fixture.url).await.unwrap());
    if scenario == "registration-drift" {
        sqlx::query("UPDATE acp_managed_sessions SET registration_digest='different' WHERE worker_session_id=?1")
            .bind(&worker).execute(fixture.repo.pool()).await.unwrap();
    }
    let (state, app, cookie) = fixture.application().await;
    assert_eq!(
        prompts(&fixture, &AgentProvider::Grok),
        1,
        "boot never prompts"
    );
    let seam = crate::test_seams::PausePoint::default();
    crate::test_seams::install_pause_for_test("canonical-human-wake-finished", card, seam.clone());
    post(
        &app,
        &cookie,
        &format!("/api/cards/{card}/planner/input"),
        "successor",
        json!({"text":"fresh successor"}),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(15), seam.entered.notified())
        .await
        .unwrap();
    seam.release.notify_one();
    if scenario == "reply" {
        idle(&fixture, card, 2).await;
        assert_eq!(prompts(&fixture, &AgentProvider::Grok), 2);
        let requests = calls(&fixture);
        let sent: Vec<_> = requests
            .iter()
            .filter(|c| c["method"] == "session/prompt")
            .collect();
        let loaded: Vec<_> = requests
            .iter()
            .filter(|c| c["method"] == "session/load")
            .collect();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0]["params"]["sessionId"], identity.1);
        for prompt in sent {
            let methods: Vec<_> = requests
                .iter()
                .filter(|c| c["_pid"] == prompt["_pid"])
                .filter_map(|c| c["method"].as_str())
                .collect();
            assert_eq!(&methods[..2], &["initialize", "authenticate"]);
            let authentication = requests
                .iter()
                .find(|c| c["_pid"] == prompt["_pid"] && c["method"] == "authenticate")
                .unwrap();
            assert_eq!(authentication["params"]["methodId"], "cached_token");
        }
        assert_eq!(facts(&fixture, card).await, (1, 1, 2, 2, 2, 1));
        let ends = fixture
            .transcript_count(card, Some(&worker), Some("turn/completed"), None)
            .await;
        assert_eq!(ends, 2, "session/load history replay cannot invent END");
    } else {
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let (_, frame) = carrier(&fixture, card).await;
                if frame.canonical_inputs.attempts.len() >= 2
                    && frame
                        .canonical_inputs
                        .attempts
                        .last()
                        .is_some_and(|attempt| {
                            matches!(attempt.delivery, InputDeliveryFact::NotDispatched { .. })
                        })
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the original owner must persist the continuation refusal");
        assert_eq!(prompts(&fixture, &AgentProvider::Grok), 1);
        assert_eq!(native_receipts(&fixture, &worker).await, original);
        assert!(
            !carrier(&fixture, card)
                .await
                .1
                .canonical_inputs
                .pending
                .is_empty()
        );
        if scenario == "load-error" {
            assert!(
                calls(&fixture)
                    .iter()
                    .any(|c| c["method"] == "session/load")
            );
        }
    }
    assert_eq!(
        calls(&fixture)
            .iter()
            .filter(|c| c["method"] == "session/new")
            .count(),
        1
    );
    assert_eq!(carrier(&fixture, card).await.0, worker);
    let after: (String, String) =
        sqlx::query_as("SELECT thread_id,agent_session_id FROM worker_sessions WHERE id=?1")
            .bind(&worker)
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
    assert_eq!(after, identity);
    assert_eq!(
        initial(&app, &cookie, &track, card, "cold").await["receipt"],
        created["initialInput"]["receipt"]
    );
    fixture.shutdown_process(&state).await;
}
