//! Closed conversations use the original sweep and native resource lifecycle.
use super::*;
use crate::shared_codex_appserver::native_owner_tests::Peer;

async fn patch_closed(app: &axum::Router, cookie: &str, track: &str, closed: bool) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/tracks/{track}"))
                .header(header::COOKIE, cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({"closed":closed}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
}

async fn sweep(state: &AppState, peer: &mut Peer) -> usize {
    sweep_result(state, peer).await.unwrap()
}

async fn sweep_result(state: &AppState, peer: &mut Peer) -> crate::error::Result<usize> {
    let sweeping = crate::terminal_sweeper::sweep(state);
    tokio::pin!(sweeping);
    let mut releases = 0;
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            tokio::select! {
                result = &mut sweeping => break result,
                call = request(peer) => {
                    assert_eq!(call["method"], "thread/unsubscribe", "sleep cannot start or interrupt work");
                    assert_eq!(call["params"]["threadId"], THREAD);
                    releases += 1;
                    send(peer, json!({"id":call["id"],"result":{"status":"unsubscribed"}})).await;
                }
            }
        }
    })
    .await
    .expect("the original resource sweep must finish")?;
    Ok(releases)
}

async fn shutdown(fixture: &Fixture, state: &AppState, worker: &str, peer: &mut Peer) {
    if let Some(handle) = state.harness.remove(worker) {
        let stopping = handle.shutdown();
        tokio::pin!(stopping);
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                tokio::select! {
                    result = &mut stopping => break result.unwrap(),
                    call = request(peer) => {
                        assert_eq!(call["method"], "turn/interrupt");
                        assert_eq!(call["params"]["threadId"], THREAD);
                        send(peer, json!({"id":call["id"],"result":{}})).await;
                    }
                }
            }
        })
        .await
        .expect("normal original runtime shutdown");
    }
    fixture.shutdown_process(state).await;
}

#[tokio::test]
async fn production_http_closed_canonical_planner_sleeps_and_wakes_original_record() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let area = post(
        &app,
        &cookie,
        "/api/areas",
        "area",
        json!({"name":"Canonical sleep","color":"#123456"}),
    )
    .await;
    let (_client, mut peer) = connect(&state.shared_codex_appserver).await;
    let created = post(
        &app,
        &cookie,
        "/api/tracks",
        "first",
        json!({
            "area_id":area["id"], "title":"", "planner_provider":"codex",
            "theme":{"fg":[255,255,255],"bg":[0,0,0]}, "first_message":FIRST_BODY
        }),
    )
    .await;
    let track = created["id"].as_str().unwrap();
    let (card, worker, _) = fixture.carrier(track).await;
    let receipt = fixture.receipt(area["id"].as_str().unwrap()).await;
    let call = native_attempt_recovery_tests::initial_request(&mut peer).await;
    send(
        &mut peer,
        json!({"id":call["id"],"result":{"turn":{"id":TURN}}}),
    )
    .await;
    notify_thread(
        &state.shared_codex_appserver,
        &mut peer,
        THREAD,
        "turn/started",
        json!({"id":TURN,"status":"inProgress"}),
    )
    .await;
    fixture
        .wait_phase(track, HarnessPhaseTag::TurnRunning)
        .await;
    notify_thread(
        &state.shared_codex_appserver,
        &mut peer,
        THREAD,
        "turn/completed",
        json!({"id":TURN,"status":"completed"}),
    )
    .await;
    let completed = fixture
        .wait_phase(track, HarnessPhaseTag::TurnCompleted)
        .await;
    assert!(completed.canonical_inputs.pending.is_empty());
    assert!(completed.canonical_inputs.active.is_none());
    assert_eq!(completed.canonical_inputs.attempts.len(), 1);
    assert!(matches!(
        completed.canonical_inputs.attempts[0].delivery,
        InputDeliveryFact::Accepted { .. }
    ));
    patch_closed(&app, &cookie, track, true).await;
    let releases = sweep(&state, &mut peer).await;
    let remains = state.harness.get(&worker).is_some();
    let cached = state.shared_codex_appserver.cached_card_for_thread(THREAD);
    let after = fixture.carrier(track).await;
    if remains {
        shutdown(&fixture, &state, &worker, &mut peer).await;
    }
    assert!(
        !remains,
        "closed canonical Planner must leave the live registry through the original sweep"
    );
    assert_eq!(
        releases, 1,
        "the real provider subscription must be released exactly once"
    );
    assert!(cached.is_none());
    assert_eq!(
        (after.0.as_str(), after.1.as_str()),
        (card.as_str(), worker.as_str())
    );
    assert_eq!(
        after.2, completed,
        "sleep cannot rewrite original execution facts"
    );
    assert_eq!(fixture.receipt(area["id"].as_str().unwrap()).await, receipt);
    assert_eq!(fixture.counts(&card).await, (1, 1, 1, 1, 0));
    assert_eq!(
        sweep(&state, &mut peer).await,
        0,
        "already sleeping is idempotent"
    );
    patch_closed(&app, &cookie, track, false).await;
    let next = post(
        &app,
        &cookie,
        &format!("/api/cards/{card}/planner/input"),
        "after-sleep",
        json!({"text":"Continue after sleeping the original record"}),
    )
    .await;
    assert_eq!(next["worker_session_id"], worker);
    let (mut reads, mut resumes) = (0, 0);
    let call = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let call = request(&mut peer).await;
            let result = match call["method"].as_str().unwrap() {
                "thread/read" => {
                    reads += 1;
                    json!({"thread":{"id":THREAD,"status":{"type":"notLoaded"},"turns":[]}})
                }
                "thread/resume" => {
                    resumes += 1;
                    json!({"thread":{"id":THREAD,"status":{"type":"idle"},"turns":[]}})
                }
                "config/read" => json!({"config":{}}),
                "turn/start" => break call,
                method => panic!("sleep recovery cannot create a replacement execution: {method}"),
            };
            send(&mut peer, json!({"id":call["id"],"result":result})).await;
        }
    })
    .await
    .expect("one legitimate Human input must wake the original sleeping record");
    assert_eq!((reads, resumes), (1, 1));
    assert_eq!(call["params"]["threadId"], THREAD);
    let input = call["params"]["input"].to_string();
    assert!(input.contains("Continue after sleeping the original record"));
    assert!(!input.contains(FIRST_BODY));
    send(
        &mut peer,
        json!({"id":call["id"],"result":{"turn":{"id":"after-sleep-turn"}}}),
    )
    .await;
    notify_thread(
        &state.shared_codex_appserver,
        &mut peer,
        THREAD,
        "turn/started",
        json!({"id":"after-sleep-turn","status":"inProgress"}),
    )
    .await;
    fixture
        .wait_phase(track, HarnessPhaseTag::TurnRunning)
        .await;
    notify_thread(
        &state.shared_codex_appserver,
        &mut peer,
        THREAD,
        "turn/completed",
        json!({"id":"after-sleep-turn","status":"completed"}),
    )
    .await;
    let done = fixture
        .wait_phase(track, HarnessPhaseTag::TurnCompleted)
        .await;
    assert_eq!(done.canonical_inputs.attempts.len(), 2);
    assert_eq!(
        done.canonical_inputs.attempts[0],
        completed.canonical_inputs.attempts[0]
    );
    assert!(done.canonical_inputs.active.is_none());
    assert_eq!(fixture.counts(&card).await, (2, 2, 1, 2, 0));
    assert_eq!(fixture.receipt(area["id"].as_str().unwrap()).await, receipt);
    shutdown(&fixture, &state, &worker, &mut peer).await;
}

async fn create_track(app: &axum::Router, cookie: &str) -> Value {
    let area = post(
        app,
        cookie,
        "/api/areas",
        "area",
        json!({"name":"Canonical sleep protection","color":"#123456"}),
    )
    .await;
    post(
        app,
        cookie,
        "/api/tracks",
        "first",
        json!({
            "area_id":area["id"], "title":"", "planner_provider":"codex",
            "theme":{"fg":[255,255,255],"bg":[0,0,0]}, "first_message":FIRST_BODY
        }),
    )
    .await
}

#[tokio::test]
async fn production_http_closed_canonical_pending_input_cannot_sleep() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let created = create_track(&app, &cookie).await;
    let track = created["id"].as_str().unwrap();
    let (card, worker, before) = fixture.carrier(track).await;
    assert_eq!(before.canonical_inputs.pending.len(), 1);
    assert!(before.canonical_inputs.active.is_none());
    assert!(before.canonical_inputs.attempts.is_empty());
    patch_closed(&app, &cookie, track, true).await;
    let eligible = crate::planner_hibernation::eligible(fixture.repo.pool(), &worker)
        .await
        .unwrap();
    crate::terminal_sweeper::sweep(&state).await.unwrap();
    let after = fixture.carrier(track).await.2;
    let counts = fixture.counts(&card).await;
    fixture.shutdown_process(&state).await;
    assert!(
        !eligible,
        "a closed original record with pending input must remain recoverable"
    );
    assert_eq!(after, before);
    assert_eq!(counts, (1, 1, 1, 0, 0));
}

async fn active_input_cannot_sleep(unknown: bool) {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let (_client, mut peer) = connect(&state.shared_codex_appserver).await;
    let created = create_track(&app, &cookie).await;
    let track = created["id"].as_str().unwrap();
    let (card, worker, _) = fixture.carrier(track).await;
    let call = native_attempt_recovery_tests::initial_request(&mut peer).await;
    if unknown {
        send(
            &mut peer,
            json!({"id":call["id"],"error":{"code":-32000,"message":"native result is unknown"}}),
        )
        .await;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let current = fixture.carrier(track).await.2;
                if matches!(
                    current.canonical_inputs.attempts[0].delivery,
                    InputDeliveryFact::Unknown
                ) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    } else {
        send(
            &mut peer,
            json!({"id":call["id"],"result":{"turn":{"id":TURN}}}),
        )
        .await;
        notify_thread(
            &state.shared_codex_appserver,
            &mut peer,
            THREAD,
            "turn/started",
            json!({"id":TURN,"status":"inProgress"}),
        )
        .await;
        fixture
            .wait_phase(track, HarnessPhaseTag::TurnRunning)
            .await;
    }
    let before = fixture.carrier(track).await.2;
    assert!(before.canonical_inputs.active.is_some());
    assert_eq!(before.canonical_inputs.attempts.len(), 1);
    patch_closed(&app, &cookie, track, true).await;
    assert!(
        !crate::planner_hibernation::eligible(fixture.repo.pool(), &worker)
            .await
            .unwrap()
    );
    let handle = state.harness.get(&worker).unwrap();
    assert!(!handle.hibernate_if_idle().await.unwrap());
    assert_eq!(sweep(&state, &mut peer).await, 0);
    assert!(state.harness.get(&worker).is_some());
    assert_eq!(
        state.shared_codex_appserver.cached_card_for_thread(THREAD),
        Some(card.clone())
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), request(&mut peer))
            .await
            .is_err(),
        "a failed sleep must not replay the original attempt"
    );
    assert_eq!(fixture.carrier(track).await.2, before);
    assert_eq!(fixture.counts(&card).await, (1, 1, 1, 1, 0));
    shutdown(&fixture, &state, &worker, &mut peer).await;
}

#[tokio::test]
async fn production_http_closed_canonical_running_input_cannot_sleep() {
    active_input_cannot_sleep(false).await;
}

#[tokio::test]
async fn production_http_closed_canonical_unknown_input_cannot_sleep_or_resend() {
    active_input_cannot_sleep(true).await;
}

#[tokio::test]
async fn production_http_invalid_closed_snapshot_cannot_abort_other_planner_hibernation() {
    use axum::extract::FromRef;
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    // This is an actual accepted record while native is unavailable. Its real
    // source-bearing wake finishes before the persisted corruption is injected.
    let bad = create_track(&app, &cookie).await;
    let bad_track = bad["id"].as_str().unwrap();
    let (bad_card, bad_worker, original) = fixture.carrier(bad_track).await;
    let source_index = state
        .harness
        .canonical_sources(&bad_worker, &bad_track.to_owned().into());
    let input = original.canonical_inputs.pending[0].input_ref();
    tokio::time::timeout(Duration::from_secs(5), async {
        while source_index.source(&input).is_none() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the actual failed wake registered its accepted input");
    let route = crate::state::RouteState::from_ref(&state);
    drop(crate::per_card_lock::lock_card(&route.planner_recovery_locks, &bad_card).await);
    patch_closed(&app, &cookie, bad_track, true).await;
    let mut unsupported = serde_json::to_value(&original).unwrap();
    unsupported["schema_version"] = json!(99);
    sqlx::query("UPDATE worker_sessions SET handle_state_json=?1 WHERE id=?2")
        .bind(unsupported.to_string())
        .bind(&bad_worker)
        .execute(fixture.repo.pool())
        .await
        .unwrap();
    assert!(
        crate::planner_hibernation::eligible(fixture.repo.pool(), &bad_worker)
            .await
            .is_err(),
        "unsupported input contracts must still be refused"
    );
    let (_client, mut peer) = connect(&state.shared_codex_appserver).await;
    let area = post(
        &app,
        &cookie,
        "/api/areas",
        "healthy-area",
        json!({"name":"Healthy closed planner","color":"#123456"}),
    )
    .await;
    let healthy = post(
        &app,
        &cookie,
        "/api/tracks",
        "healthy-first",
        json!({
            "area_id":area["id"], "title":"", "planner_provider":"codex",
            "theme":{"fg":[255,255,255],"bg":[0,0,0]}, "first_message":FIRST_BODY
        }),
    )
    .await;
    let track = healthy["id"].as_str().unwrap();
    let (card, worker, _) = fixture.carrier(track).await;
    let call = native_attempt_recovery_tests::initial_request(&mut peer).await;
    send(
        &mut peer,
        json!({"id":call["id"],"result":{"turn":{"id":TURN}}}),
    )
    .await;
    notify_thread(
        &state.shared_codex_appserver,
        &mut peer,
        THREAD,
        "turn/started",
        json!({"id":TURN,"status":"inProgress"}),
    )
    .await;
    fixture
        .wait_phase(track, HarnessPhaseTag::TurnRunning)
        .await;
    notify_thread(
        &state.shared_codex_appserver,
        &mut peer,
        THREAD,
        "turn/completed",
        json!({"id":TURN,"status":"completed"}),
    )
    .await;
    let completed = fixture
        .wait_phase(track, HarnessPhaseTag::TurnCompleted)
        .await;
    patch_closed(&app, &cookie, track, true).await;
    let outcome = sweep_result(&state, &mut peer).await;
    let remains = state.harness.get(&worker).is_some();
    let after = fixture.carrier(track).await.2;
    let cached = state.shared_codex_appserver.cached_card_for_thread(THREAD);
    // Restore the injected fault and clean the real listener even when the
    // regression is red; the corrupted record is never treated as eligible.
    sqlx::query("UPDATE worker_sessions SET handle_state_json=?1 WHERE id=?2")
        .bind(serde_json::to_string(&original).unwrap())
        .bind(&bad_worker)
        .execute(fixture.repo.pool())
        .await
        .unwrap();
    shutdown(&fixture, &state, &worker, &mut peer).await;
    assert!(
        outcome.is_ok(),
        "one invalid snapshot must stay within its existing per-record failure boundary: {outcome:?}"
    );
    assert_eq!(outcome.unwrap(), 1);
    assert!(!remains);
    assert!(cached.is_none());
    assert_eq!(after, completed);
    assert_eq!(fixture.counts(&card).await, (1, 1, 1, 1, 0));
    assert_eq!(fixture.carrier(bad_track).await.2, original);
}
