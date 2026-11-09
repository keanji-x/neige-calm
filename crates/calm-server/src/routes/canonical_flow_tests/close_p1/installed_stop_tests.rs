//! Installed shutdown must share native Stop facts without granting canonical END.
use super::*;

enum StopReply {
    Facts(Value),
    ReadError,
    Transport { daemon_down: bool },
}

fn facts(status: &str, turns: Value) -> StopReply {
    StopReply::Facts(json!({"thread":{
        "id":THREAD,"status":{"type":status,"activeFlags":[]},"turns":turns
    }}))
}

// Join the actual cleanup's card fence after its Stop request, answering only
// external RPCs. Returning from this fence observes the entire cleanup pass.
async fn close_pass(
    state: &AppState,
    app: &axum::Router,
    cookie: &str,
    ids: (&str, &str, &str),
    peer: &mut Peer,
    reply: StopReply,
) {
    let (track, card, worker) = ids;
    assert!(
        state.harness.get(&worker.to_owned()).is_some(),
        "exercise the registered owner"
    );
    assert_eq!(close_status(app, cookie, track).await, StatusCode::OK);
    let stop = request(peer).await;
    assert_eq!(stop["method"], "turn/interrupt");
    assert_eq!(stop["params"]["threadId"], THREAD);
    assert_eq!(stop["params"]["turnId"], TURN);
    let route = crate::state::RouteState::from_ref(state);
    if let StopReply::Transport { daemon_down } = reply {
        if daemon_down {
            // The existing provider readiness seam reports the daemon's loss;
            // closing its real transport makes the in-flight interrupt fail.
            state
                .shared_codex_appserver
                .publish_readiness_for_test(1, false);
        }
        peer.close(None).await.unwrap();
        drop(
            tokio::time::timeout(
                Duration::from_secs(5),
                crate::per_card_lock::lock_card(&route.planner_recovery_locks, card),
            )
            .await
            .expect("transport failure must finish a cleanup pass"),
        );
        return;
    }
    send(
        peer,
        json!({"id":stop["id"],"error":{"code":-32600,"message":"turn not found"}}),
    )
    .await;
    let settled = crate::per_card_lock::lock_card(&route.planner_recovery_locks, card);
    tokio::pin!(settled);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            tokio::select! {
                guard = &mut settled => { drop(guard); break; }
                call = request(peer) => {
                    let response = match call["method"].as_str().unwrap() {
                        "thread/read" => {
                            assert_eq!(call["params"]["threadId"], THREAD);
                            match &reply {
                                StopReply::Facts(result) => json!({"result":result}),
                                StopReply::ReadError => json!({"error":{"code":-32600,"message":"read unavailable"}}),
                                StopReply::Transport { .. } => unreachable!(),
                            }
                        }
                        "thread/loaded/list" => json!({"result":{"data":[]}}),
                        "thread/unsubscribe" => {
                            assert_eq!(call["params"]["threadId"], THREAD);
                            json!({"result":{"status":"notLoaded"}})
                        }
                        method => panic!("cleanup cannot issue or retarget work: {method}"),
                    };
                    let mut response = response;
                    response["id"] = call["id"].clone();
                    send(peer, response).await;
                }
            }
        }
    }).await.expect("native Stop observation must finish a cleanup pass");
}

async fn original_end(fixture: &Fixture, state: &AppState, track: &str, peer: &mut Peer) {
    notify_thread(
        &state.shared_codex_appserver,
        peer,
        THREAD,
        "turn/completed",
        json!({"id":TURN,"status":"completed"}),
    )
    .await;
    let ended = fixture
        .wait_phase(track, HarnessPhaseTag::TurnCompleted)
        .await;
    assert!(ended.canonical_inputs.active.is_none());
    assert_eq!(ended.canonical_inputs.attempts.len(), 1);
}

async fn stopped_original_reopens(reply: StopReply) {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let (_client, mut peer) = connect(&state.shared_codex_appserver).await;
    let (track, card, worker) = running(&fixture, &state, &app, &cookie, &mut peer).await;
    original_end(&fixture, &state, &track, &mut peer).await;
    let before = fixture.carrier(&track).await.2;
    let reconnect = matches!(reply, StopReply::Transport { .. });
    close_pass(
        &state,
        &app,
        &cookie,
        (&track, &card, &worker),
        &mut peer,
        reply,
    )
    .await;
    assert_eq!(
        pending(&fixture, &worker).await,
        0,
        "native Stop facts and the existing original END must settle installed cleanup"
    );
    assert!(state.harness.get(&worker).is_none());
    assert_eq!(
        fixture.carrier(&track).await.2.canonical_inputs,
        before.canonical_inputs
    );
    // Recovery and the new Human source still use the same original carrier.
    let (_reconnected, mut replacement);
    let peer = if reconnect {
        (_reconnected, replacement) = connect(&state.shared_codex_appserver).await;
        &mut replacement
    } else {
        &mut peer
    };
    reopen_human_input(
        &fixture,
        &state,
        &app,
        &cookie,
        (&track, &card, &worker),
        &before,
        peer,
    )
    .await;
    assert_eq!(fixture.counts(&card).await, (2, 2, 1, 2, 0));
    hibernation_tests::shutdown(&fixture, &state, &worker, peer).await;
}

#[tokio::test]
async fn installed_not_loaded_original_end_reopens_once() {
    stopped_original_reopens(facts("notLoaded", json!([]))).await;
}

#[tokio::test]
async fn installed_ended_original_end_reopens_once() {
    for status in ["completed", "interrupted", "failed"] {
        stopped_original_reopens(facts("idle", json!([{"id":TURN,"status":status}]))).await;
    }
}

#[tokio::test]
async fn installed_daemon_down_original_end_reopens_once() {
    stopped_original_reopens(StopReply::Transport { daemon_down: true }).await;
}

#[tokio::test]
async fn installed_unconfirmed_stop_with_original_end_retains_cleanup() {
    for reply in [
        facts("active", json!([{"id":TURN,"status":"inProgress"}])),
        facts("idle", json!([{"id":TURN,"status":"unknown"}])),
        facts("idle", json!([])),
        StopReply::ReadError,
        StopReply::Transport { daemon_down: false },
    ] {
        let fixture = Fixture::new().await;
        let (state, app, cookie) = fixture.application().await;
        let (_client, mut peer) = connect(&state.shared_codex_appserver).await;
        let (track, card, worker) = running(&fixture, &state, &app, &cookie, &mut peer).await;
        original_end(&fixture, &state, &track, &mut peer).await;
        let before = fixture.carrier(&track).await.2;
        close_pass(
            &state,
            &app,
            &cookie,
            (&track, &card, &worker),
            &mut peer,
            reply,
        )
        .await;
        assert_eq!(
            pending(&fixture, &worker).await,
            1,
            "refusal or Unknown is not a physical Stop fact"
        );
        assert!(state.harness.get(&worker).is_some());
        assert_eq!(
            fixture.carrier(&track).await.2.canonical_inputs,
            before.canonical_inputs
        );
        fixture.shutdown_process(&state).await;
    }
}

#[tokio::test]
async fn installed_stop_facts_without_original_end_retain_claim() {
    for reply in [
        facts("active", json!([{"id":TURN,"status":"inProgress"}])),
        facts("idle", json!([{"id":TURN,"status":"unknown"}])),
        StopReply::ReadError,
        facts("notLoaded", json!([])),
        facts("idle", json!([{"id":"another-turn","status":"completed"}])),
        StopReply::Transport { daemon_down: true },
    ] {
        let fixture = Fixture::new().await;
        let (state, app, cookie) = fixture.application().await;
        let (_client, mut peer) = connect(&state.shared_codex_appserver).await;
        let (track, card, worker) = running(&fixture, &state, &app, &cookie, &mut peer).await;
        let connected = !matches!(reply, StopReply::Transport { .. });
        let before = fixture.carrier(&track).await.2;
        assert!(before.canonical_inputs.active.is_some());
        close_pass(
            &state,
            &app,
            &cookie,
            (&track, &card, &worker),
            &mut peer,
            reply,
        )
        .await;
        assert_eq!(
            pending(&fixture, &worker).await,
            1,
            "physical Stop facts cannot supply the original immutable END"
        );
        assert!(state.harness.get(&worker).is_some());
        let after = fixture.carrier(&track).await.2;
        assert_eq!(
            after.canonical_inputs.active,
            before.canonical_inputs.active
        );
        assert_eq!(
            after.canonical_inputs.attempts,
            before.canonical_inputs.attempts
        );
        assert!(
            after
                .canonical_inputs
                .pending
                .starts_with(&before.canonical_inputs.pending)
        );
        hibernation_tests::patch_closed(&app, &cookie, &track, false).await;
        let wake = crate::test_seams::PausePoint::default();
        crate::test_seams::install_pause_for_test(
            "canonical-human-wake-finished",
            &card,
            wake.clone(),
        );
        assert_eq!(
            status(
                &app,
                &cookie,
                &format!("/api/cards/{card}/planner/input"),
                "blocked-after-close",
                json!({"text":"Retain this new Human source"})
            )
            .await,
            StatusCode::OK
        );
        tokio::time::timeout(Duration::from_secs(5), wake.entered.notified())
            .await
            .expect("the actual Human wake must finish before checking the execution fence");
        wake.release.notify_one();
        let queued = fixture.carrier(&track).await.2;
        assert_eq!(
            queued.canonical_inputs.active,
            after.canonical_inputs.active
        );
        assert_eq!(
            queued.canonical_inputs.attempts,
            after.canonical_inputs.attempts
        );
        assert!(
            queued
                .canonical_inputs
                .pending
                .starts_with(&after.canonical_inputs.pending)
        );
        assert_eq!(
            queued.canonical_inputs.pending.len(),
            after.canonical_inputs.pending.len() + 1
        );
        assert_eq!(pending(&fixture, &worker).await, 1);
        if connected {
            crate::shared_codex_appserver::native_owner_tests::assert_no_request(&mut peer).await;
        }
        fixture.shutdown_process(&state).await;
    }
}
