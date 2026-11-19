//! Close completion observes the original peer independently of registry residency.
use super::*;
use crate::shared_codex_appserver::transport_recovery_tests as transport;

async fn accepted(
    fixture: &Fixture,
    app: &axum::Router,
    cookie: &str,
    peer: &transport::Fixture,
    area: &str,
    thread: &str,
) -> (String, String, String) {
    peer.answers.lock().unwrap().insert(
        "thread/start".into(),
        json!({"result":{"thread":{"id":thread}}}),
    );
    peer.answers
        .lock()
        .unwrap()
        .insert("turn/start".into(), json!({"result":{"turn":{"id":TURN}}}));
    let created = post(
        app,
        cookie,
        "/api/tracks",
        thread,
        json!({"area_id":area,"title":"","planner_provider":"codex",
            "theme":{"fg":[255,255,255],"bg":[0,0,0]},"first_message":FIRST_BODY}),
    )
    .await;
    let track = created["id"].as_str().unwrap().to_owned();
    fixture
        .wait_phase(&track, HarnessPhaseTag::TurnRunning)
        .await;
    let (card, worker, frame) = fixture.carrier(&track).await;
    assert!(matches!(
        frame.canonical_inputs.attempts[0].delivery,
        InputDeliveryFact::Accepted { .. }
    ));
    (track, card, worker)
}

fn history(peer: &transport::Fixture, thread: &str, terminal: Value) {
    peer.answers.lock().unwrap().insert(
        "thread/read".into(),
        json!({"result":{"thread":{"id":thread,"status":{"type":"idle"},"turns":[terminal]}}}),
    );
}

fn terminal() -> Value {
    json!({"id":TURN,"status":"completed","itemsView":"full","items":[]})
}

async fn joined(state: &AppState, worker: &str) {
    stop_owner_tests::joined_cleanup(state, worker).await;
}

#[tokio::test]
async fn installed_lost_end_reconnects_then_close_consumes_history_and_new_human_once() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let peer = transport::peer_for(
        state.shared_codex_appserver.clone(),
        fixture.repo.clone(),
        tempfile::tempdir().unwrap(),
    )
    .await;
    peer.keep_peer_process_alive().await;
    let area = post(
        &app,
        &cookie,
        "/api/areas",
        "area",
        json!({"name":"Completion","color":"#123456"}),
    )
    .await;
    let (track, card, worker) = accepted(
        &fixture,
        &app,
        &cookie,
        &peer,
        area["id"].as_str().unwrap(),
        THREAD,
    )
    .await;
    let before = fixture.carrier(&track).await.2;
    peer.disconnect().await;
    peer.reconnect().await;
    // Legitimate explicit recovery to the original endpoint emits no compensating END.
    assert!(state.harness.get(&worker).is_some());
    history(&peer, THREAD, terminal());
    peer.requests.lock().unwrap().clear();
    assert_eq!(close_status(&app, &cookie, &track).await, StatusCode::OK);
    joined(&state, &worker).await;
    let first_left = pending(&fixture, &worker).await;
    // A durable retry must use the same original obligation and installed owner.
    assert_eq!(close_status(&app, &cookie, &track).await, StatusCode::OK);
    joined(&state, &worker).await;
    let left = pending(&fixture, &worker).await;
    let after = fixture.carrier(&track).await.2;
    println!(
        "OBSERVATION {}",
        json!({"first_pending":first_left,"retry_pending":left,"active":after.canonical_inputs.active,"requests":peer.requests.lock().unwrap().clone()})
    );
    if left != 0 {
        fixture.shutdown_process(&state).await;
    }
    assert_eq!(
        left, 0,
        "installed source-loss history must discharge the original END"
    );
    assert!(after.canonical_inputs.active.is_none());
    assert_eq!(
        after.canonical_inputs.attempts,
        before.canonical_inputs.attempts
    );
    assert!(state.harness.get(&worker).is_none());
    peer.answers.lock().unwrap().insert(
        "thread/resume".into(),
        json!({"result":{"thread":{"id":THREAD,"status":{"type":"idle"}}}}),
    );
    hibernation_tests::patch_closed(&app, &cookie, &track, false).await;
    peer.answers.lock().unwrap().insert(
        "turn/start".into(),
        json!({"result":{"turn":{"id":"fresh-turn"}}}),
    );
    post(
        &app,
        &cookie,
        &format!("/api/cards/{card}/planner/input"),
        "fresh",
        json!({"text":"New Human after recovered END"}),
    )
    .await;
    fixture
        .wait_phase(&track, HarnessPhaseTag::TurnRunning)
        .await;
    state.dispatcher.sweep_track_close_cleanups().await;
    let requests = peer.requests.lock().unwrap().clone();
    let starts: Vec<_> = requests
        .iter()
        .filter(|r| r["method"] == "turn/start")
        .collect();
    assert_eq!(starts.len(), 1);
    assert_eq!(starts[0]["params"]["threadId"], THREAD);
    assert!(!requests.iter().any(|r| r["method"] == "thread/start"));
    assert!(
        starts[0]["params"]["input"]
            .to_string()
            .contains("New Human after recovered END")
    );
    assert!(
        !starts[0]["params"]["input"]
            .to_string()
            .contains(FIRST_BODY)
    );
    assert_eq!(
        fixture
            .carrier(&track)
            .await
            .2
            .canonical_inputs
            .attempts
            .len(),
        2
    );
    if let Some(handle) = state.harness.remove(&worker) {
        handle.shutdown().await.unwrap();
    }
    fixture.shutdown_process(&state).await;
}

#[tokio::test]
async fn installed_history_wrong_unknown_or_incomplete_end_keeps_original_obligation() {
    for end in [
        json!({"id":"wrong-turn","status":"completed","items":[]}),
        json!({"id":TURN,"status":"unknown","items":[]}),
        json!({"id":TURN,"status":"completed"}),
        json!({"id":TURN,"status":"completed","itemsView":"partial","items":[]}),
    ] {
        let fixture = Fixture::new().await;
        let (state, app, cookie) = fixture.application().await;
        let peer = transport::peer_for(
            state.shared_codex_appserver.clone(),
            fixture.repo.clone(),
            tempfile::tempdir().unwrap(),
        )
        .await;
        let area = post(
            &app,
            &cookie,
            "/api/areas",
            "area",
            json!({"name":"Rejected END","color":"#123456"}),
        )
        .await;
        let (track, _, worker) = accepted(
            &fixture,
            &app,
            &cookie,
            &peer,
            area["id"].as_str().unwrap(),
            THREAD,
        )
        .await;
        let before = fixture.carrier(&track).await.2;
        peer.disconnect().await;
        peer.reconnect().await;
        history(&peer, THREAD, end.clone());
        assert_eq!(close_status(&app, &cookie, &track).await, StatusCode::OK);
        joined(&state, &worker).await;
        assert_eq!(pending(&fixture, &worker).await, 1, "{end}");
        let after = fixture.carrier(&track).await.2;
        assert_eq!(
            after.canonical_inputs.active,
            before.canonical_inputs.active
        );
        assert_eq!(
            after.canonical_inputs.attempts,
            before.canonical_inputs.attempts
        );
        assert!(state.harness.get(&worker).is_some());
        fixture.shutdown_process(&state).await;
    }
}

#[tokio::test]
async fn close_history_source_replacement_rejects_old_response() {
    changed_read(true).await;
}

#[tokio::test]
async fn close_history_activity_replacement_rejects_old_response() {
    changed_read(false).await;
}

async fn changed_read(source: bool) {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let peer = transport::peer_for(
        state.shared_codex_appserver.clone(),
        fixture.repo.clone(),
        tempfile::tempdir().unwrap(),
    )
    .await;
    let area = post(
        &app,
        &cookie,
        "/api/areas",
        "area",
        json!({"name":"Captured END fence","color":"#123456"}),
    )
    .await;
    let (track, _, worker) = accepted(
        &fixture,
        &app,
        &cookie,
        &peer,
        area["id"].as_str().unwrap(),
        THREAD,
    )
    .await;
    let before = fixture.carrier(&track).await.2;
    peer.disconnect().await;
    peer.reconnect().await;
    history(&peer, THREAD, terminal());
    let (entered, release) = peer.hold_read(THREAD);
    assert_eq!(close_status(&app, &cookie, &track).await, StatusCode::OK);
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    let changed = if source {
        peer.connect_new_owner(&state.shared_codex_appserver).await;
        true
    } else {
        // Only external native protocol changes activity; no host FSM/claim is injected.
        peer.notify(json!({"method":"turn/started","params":{"threadId":THREAD,"turn":{"id":TURN,"status":"inProgress"}}}));
        tokio::time::timeout(Duration::from_secs(2), async {
            while state
                .shared_codex_appserver
                .active_turn_id_for_thread(THREAD)
                .as_deref()
                != Some(TURN)
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .is_ok()
    };
    release.notify_one();
    // Join the already-running pass, without scheduling a new read on the replacement.
    let context = state.mcp_context();
    let _fence = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(fence) = context.tool_calls.fence(&worker) {
                break fence;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let left = pending(&fixture, &worker).await;
    let after = fixture.carrier(&track).await.2;
    fixture.shutdown_process(&state).await;
    println!(
        "OBSERVATION {}",
        json!({"source_change":source,"changed_while_read_pending":changed,"pending":left,"active":after.canonical_inputs.active})
    );
    assert!(
        changed,
        "native source or activity replacement must be observed"
    );
    assert_eq!(left, 1, "stale original END cannot clear the obligation");
    assert_eq!(
        after.canonical_inputs.active,
        before.canonical_inputs.active
    );
    assert_eq!(
        after.canonical_inputs.attempts,
        before.canonical_inputs.attempts
    );
}
