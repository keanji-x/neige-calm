//! Close regressions use authenticated HTTP and the actual native protocol peer.
use super::*;
use crate::shared_codex_appserver::native_owner_tests::Peer;
use calm_truth::db::RepoRead;

async fn running(
    fixture: &Fixture,
    state: &AppState,
    app: &axum::Router,
    cookie: &str,
    peer: &mut Peer,
) -> (String, String, String) {
    let created = hibernation_tests::create_track(app, cookie).await;
    let track = created["id"].as_str().unwrap().to_owned();
    let (card, worker, _) = fixture.carrier(&track).await;
    let call = native_attempt_recovery_tests::initial_request(peer).await;
    send(peer, json!({"id":call["id"],"result":{"turn":{"id":TURN}}})).await;
    notify_thread(
        &state.shared_codex_appserver,
        peer,
        THREAD,
        "turn/started",
        json!({"id":TURN,"status":"inProgress"}),
    )
    .await;
    fixture
        .wait_phase(&track, HarnessPhaseTag::TurnRunning)
        .await;
    (track, card, worker)
}

async fn pending(fixture: &Fixture, worker: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM track_close_cleanups WHERE worker_session_id=?1")
        .bind(worker)
        .fetch_one(fixture.repo.pool())
        .await
        .unwrap()
}

async fn finish_cleanup(fixture: &Fixture, state: &AppState, worker: &str, peer: &mut Peer) {
    state.dispatcher.poke_track_close_cleanups();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            tokio::select! {
                _ = async {
                    while pending(fixture, worker).await != 0 { tokio::task::yield_now().await; }
                } => break,
                call = request(peer) => {
                    assert!(matches!(call["method"].as_str(), Some("turn/interrupt" | "thread/unsubscribe")), "{call}");
                    assert_eq!(call["params"]["threadId"], THREAD);
                    send(peer, json!({"id":call["id"],"result":{}})).await;
                }
            }
        }
    }).await.expect("confirmed close cleanup must finish");
}

async fn close_status(app: &axum::Router, cookie: &str, track: &str) -> StatusCode {
    app.clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/tracks/{track}"))
                .header(header::COOKIE, cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({"closed":true}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn close_p1_root_reopen_human_input_reaches_native_without_replaying_original() {
    root_reopen(false).await;
}

#[tokio::test]
async fn close_p1_active_root_waits_for_original_end_and_reopens_without_resend() {
    root_reopen(true).await;
}

async fn root_reopen(active: bool) {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let (_client, mut peer) = connect(&state.shared_codex_appserver).await;
    let (track, card, worker) = running(&fixture, &state, &app, &cookie, &mut peer).await;
    if !active {
        notify_thread(
            &state.shared_codex_appserver,
            &mut peer,
            THREAD,
            "turn/completed",
            json!({"id":TURN,"status":"completed"}),
        )
        .await;
        fixture
            .wait_phase(&track, HarnessPhaseTag::TurnCompleted)
            .await;
    }
    let before = fixture.carrier(&track).await.2;
    hibernation_tests::patch_closed(&app, &cookie, &track, true).await;
    if active {
        let stop = request(&mut peer).await;
        assert_eq!(stop["method"], "turn/interrupt");
        send(&mut peer, json!({"id":stop["id"],"result":{}})).await;
        // Join the same card fence after this pass. ACK alone cannot release
        // the obligation or the original accepted claim.
        let route = crate::state::RouteState::from_ref(&state);
        drop(crate::per_card_lock::lock_card(&route.planner_recovery_locks, &card).await);
        assert_eq!(pending(&fixture, &worker).await, 1);
        let held = fixture.carrier(&track).await.2;
        assert_eq!(held.canonical_inputs.active, before.canonical_inputs.active);
        assert_eq!(
            held.canonical_inputs.attempts,
            before.canonical_inputs.attempts
        );
        assert!(
            held.canonical_inputs
                .pending
                .starts_with(&before.canonical_inputs.pending)
        );
        notify_thread(
            &state.shared_codex_appserver,
            &mut peer,
            THREAD,
            "turn/completed",
            json!({"id":TURN,"status":"interrupted"}),
        )
        .await;
    }
    finish_cleanup(&fixture, &state, &worker, &mut peer).await;
    assert!(state.harness.get(&worker).is_none());
    if !active {
        let events: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM events WHERE kind='track.updated' AND scope_track=?1",
        )
        .bind(&track)
        .fetch_one(fixture.repo.pool())
        .await
        .unwrap();
        hibernation_tests::patch_closed(&app, &cookie, &track, true).await;
        let after: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM events WHERE kind='track.updated' AND scope_track=?1",
        )
        .bind(&track)
        .fetch_one(fixture.repo.pool())
        .await
        .unwrap();
        assert_eq!(
            after, events,
            "a repeated Close emits no duplicate Track update"
        );
        finish_cleanup(&fixture, &state, &worker, &mut peer).await;
    }
    reopen_human_input(
        &fixture,
        &state,
        &app,
        &cookie,
        (&track, &card, &worker),
        &before,
        &mut peer,
    )
    .await;
    hibernation_tests::shutdown(&fixture, &state, &worker, &mut peer).await;
}

async fn reopen_human_input(
    fixture: &Fixture,
    state: &AppState,
    app: &axum::Router,
    cookie: &str,
    ids: (&str, &str, &str),
    before: &CanonicalHarnessSnapshot,
    peer: &mut Peer,
) {
    let (track, card, _worker) = ids;
    hibernation_tests::patch_closed(app, cookie, track, false).await;
    post(
        app,
        cookie,
        &format!("/api/cards/{card}/planner/input"),
        "after-close",
        json!({"text":"Fresh work after confirmed close"}),
    )
    .await;
    let call = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let call = request(peer).await;
            let result = match call["method"].as_str().unwrap() {
                "thread/read" | "thread/resume" => {
                    json!({"thread":{"id":THREAD,"status":{"type":"idle"},"turns":[]}})
                }
                "config/read" => json!({"config":{}}),
                "turn/start" => break call,
                method => panic!("reopen must retain the original identity: {method}"),
            };
            send(peer, json!({"id":call["id"],"result":result})).await;
        }
    })
    .await
    .expect("new Human input must actually issue a native turn after Close and Reopen");
    assert_eq!(call["params"]["threadId"], THREAD);
    let input = call["params"]["input"].to_string();
    assert!(input.contains("Fresh work after confirmed close"));
    assert!(!input.contains(FIRST_BODY));
    send(
        peer,
        json!({"id":call["id"],"result":{"turn":{"id":"fresh-turn"}}}),
    )
    .await;
    notify_thread(
        &state.shared_codex_appserver,
        peer,
        THREAD,
        "turn/started",
        json!({"id":"fresh-turn","status":"inProgress"}),
    )
    .await;
    fixture
        .wait_phase(track, HarnessPhaseTag::TurnRunning)
        .await;
    // A later cleanup sweep must not control the successor's actual native turn.
    let active = fixture.carrier(track).await.2;
    state.dispatcher.sweep_track_close_cleanups().await;
    crate::shared_codex_appserver::native_owner_tests::assert_no_request(peer).await;
    assert_eq!(
        fixture.carrier(track).await.2.canonical_inputs,
        active.canonical_inputs
    );
    assert_eq!(
        state
            .shared_codex_appserver
            .active_turn_id_for_thread(THREAD)
            .as_deref(),
        Some("fresh-turn")
    );
    notify_thread(
        &state.shared_codex_appserver,
        peer,
        THREAD,
        "turn/completed",
        json!({"id":"fresh-turn","status":"completed"}),
    )
    .await;
    let after = fixture
        .wait_phase(track, HarnessPhaseTag::TurnCompleted)
        .await;
    assert_eq!(after.canonical_inputs.attempts.len(), 2);
    assert_eq!(
        after.canonical_inputs.attempts[0],
        before.canonical_inputs.attempts[0]
    );
}

#[tokio::test]
async fn close_p1_interrupt_timeout_failed_planner_still_owes_exact_stop() {
    interrupt_timeout_close(false).await;
}

#[tokio::test]
async fn close_p1_interrupt_timeout_without_stop_identity_refuses_atomically() {
    interrupt_timeout_close(true).await;
}

async fn interrupt_timeout_close(missing_stop_identity: bool) {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let (_client, mut peer) = connect(&state.shared_codex_appserver).await;
    let (track, card, worker) = running(&fixture, &state, &app, &cookie, &mut peer).await;
    let stopping = tokio::spawn({
        let (app, cookie, card) = (app.clone(), cookie.clone(), card.clone());
        async move {
            status(
                &app,
                &cookie,
                &format!("/api/cards/{card}/planner/interrupt"),
                "stop",
                json!({}),
            )
            .await
        }
    });
    let call = request(&mut peer).await;
    assert_eq!(call["method"], "turn/interrupt");
    let handle = state.harness.get(&worker).unwrap();
    handle.expire_interrupt_deadline_for_test().await;
    let timed_out = fixture.wait_phase(&track, HarnessPhaseTag::Wedged).await;
    assert_eq!(
        timed_out.header.wedged_reason.as_deref(),
        Some(calm_types::harness::HARNESS_INTERRUPT_TIMEOUT_REASON)
    );
    assert!(timed_out.canonical_inputs.active.is_some());
    // An ACK still supplies no END. The real watchdog already wrote Failed.
    send(&mut peer, json!({"id":call["id"],"result":{}})).await;
    assert_eq!(stopping.await.unwrap(), StatusCode::OK);
    let row: String = sqlx::query_scalar("SELECT state FROM worker_sessions WHERE id=?1")
        .bind(&worker)
        .fetch_one(fixture.repo.pool())
        .await
        .unwrap();
    assert_eq!(row, "failed");
    if missing_stop_identity {
        // Keep the damaged live projection in its own negative case. Restoring
        // only SQL cannot synchronously restore the original owner's cache.
        // This genuinely timed-out carrier must refuse an unbindable Stop
        // atomically. Do not synthesize a Failed row or bypass the HTTP close owner.
        let mut damaged = serde_json::to_value(&timed_out).unwrap();
        damaged["last_thread_id"] = Value::Null;
        fixture
            .repo
            .db()
            .writer()
            .execute(
                sqlx::query(
                    "UPDATE worker_sessions SET thread_id=NULL,handle_state_json=?1 WHERE id=?2",
                )
                .bind(damaged.to_string())
                .bind(&worker),
            )
            .await
            .unwrap();
        assert_eq!(
            close_status(&app, &cookie, &track).await,
            StatusCode::CONFLICT
        );
        assert!(
            fixture
                .repo
                .track_get(&track)
                .await
                .unwrap()
                .unwrap()
                .is_open()
        );
        assert_eq!(pending(&fixture, &worker).await, 0);
        let held = fixture.carrier(&track).await.2;
        assert_eq!(
            held.canonical_inputs.active,
            timed_out.canonical_inputs.active
        );
        assert_eq!(
            held.canonical_inputs.attempts,
            timed_out.canonical_inputs.attempts
        );
        assert!(
            held.canonical_inputs
                .pending
                .starts_with(&timed_out.canonical_inputs.pending)
        );
        fixture.shutdown_process(&state).await;
        return;
    }
    // A released Close could archive this genuinely failed execution without
    // recording Stop. Closing that legacy row again must commit the missing
    // obligation even though TrackUpdated is an idempotent no-op.
    fixture
        .repo
        .track_update(
            &track,
            crate::model::TrackPatch {
                closed: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    hibernation_tests::patch_closed(&app, &cookie, &track, true).await;
    assert_eq!(
        pending(&fixture, &worker).await,
        1,
        "Failed is not a stop confirmation"
    );
    let retry = tokio::time::timeout(Duration::from_secs(5), request(&mut peer))
        .await
        .expect("Close must retry the original Stop");
    assert_eq!(retry["method"], "turn/interrupt");
    assert_eq!(retry["params"]["threadId"], THREAD);
    assert_eq!(retry["params"]["turnId"], TURN);
    send(&mut peer, json!({"id":retry["id"],"result":{}})).await;
    let route = crate::state::RouteState::from_ref(&state);
    drop(crate::per_card_lock::lock_card(&route.planner_recovery_locks, &card).await);
    assert_eq!(
        pending(&fixture, &worker).await,
        1,
        "an ACK cannot invent END"
    );
    let held = fixture.carrier(&track).await.2;
    assert_eq!(
        held.canonical_inputs.active,
        timed_out.canonical_inputs.active
    );
    assert_eq!(
        held.canonical_inputs.attempts,
        timed_out.canonical_inputs.attempts
    );
    assert!(
        held.canonical_inputs
            .pending
            .starts_with(&timed_out.canonical_inputs.pending)
    );
    notify_thread(
        &state.shared_codex_appserver,
        &mut peer,
        THREAD,
        "turn/completed",
        json!({"id":TURN,"status":"interrupted"}),
    )
    .await;
    finish_cleanup(&fixture, &state, &worker, &mut peer).await;
    let row: (String, Option<i64>) =
        sqlx::query_as("SELECT state,completed_at_ms FROM worker_sessions WHERE id=?1")
            .bind(&worker)
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
    assert_eq!(
        row,
        ("idle".into(), None),
        "only the original timeout END restores this carrier"
    );
    let after = fixture.carrier(&track).await.2;
    assert!(after.canonical_inputs.active.is_none());
    assert_eq!(
        after.canonical_inputs.attempts,
        timed_out.canonical_inputs.attempts
    );
    fixture.shutdown_process(&state).await;
}

#[tokio::test]
async fn close_p1_registry_miss_snapshot_only_stops_original_thread() {
    for (active, inline) in [
        (false, None),
        (false, Some("")),
        (true, None),
        (true, Some("")),
    ] {
        let fixture = Fixture::new().await;
        let (state, app, cookie) = fixture.application().await;
        let (_client, mut peer) = connect(&state.shared_codex_appserver).await;
        let (track, _, worker) = running(&fixture, &state, &app, &cookie, &mut peer).await;
        if active {
            // Lose the control owner through its real shutdown. Its ACK still
            // supplies no END; retain the actual original input for cleanup.
            native_attempt_recovery_tests::shutdown(&state, &worker, &mut peer).await;
            assert!(state.harness.get(&worker).is_none());
            assert!(
                fixture
                    .carrier(&track)
                    .await
                    .2
                    .canonical_inputs
                    .active
                    .is_some()
            );
        } else {
            notify_thread(
                &state.shared_codex_appserver,
                &mut peer,
                THREAD,
                "turn/completed",
                json!({"id":TURN,"status":"completed"}),
            )
            .await;
            fixture
                .wait_phase(&track, HarnessPhaseTag::TurnCompleted)
                .await;
            let handle = state.harness.get(&worker).unwrap();
            assert!(handle.hibernate_if_idle().await.unwrap());
            assert!(state.harness.retire_if_same(&worker, &handle));
        }
        let before = fixture.carrier(&track).await.2;
        fixture
            .repo
            .db()
            .writer()
            .execute(
                sqlx::query("UPDATE worker_sessions SET thread_id=?1 WHERE id=?2")
                    .bind(inline)
                    .bind(&worker),
            )
            .await
            .unwrap();
        hibernation_tests::patch_closed(&app, &cookie, &track, true).await;
        let stopped = tokio::time::timeout(Duration::from_secs(2), request(&mut peer)).await;
        assert!(
            stopped.is_ok(),
            "a valid snapshot thread must not be silently treated as stopped: {inline:?}"
        );
        let call = stopped.unwrap();
        assert_eq!(call["method"], "turn/interrupt");
        assert_eq!(call["params"]["threadId"], THREAD);
        assert_eq!(call["params"]["turnId"], TURN);
        send(
            &mut peer,
            json!({"id":call["id"],"error":{"code":-32000,"message":"Stop unconfirmed"}}),
        )
        .await;
        let observation = request(&mut peer).await;
        assert_eq!(observation["method"], "thread/read");
        send(&mut peer, json!({"id":observation["id"],"result":{"thread":{"id":THREAD,"status":{"type":"active"},"turns":[{"id":TURN,"status":"inProgress"}]}}})).await;
        let route = crate::state::RouteState::from_ref(&state);
        let card = fixture.carrier(&track).await.0;
        drop(crate::per_card_lock::lock_card(&route.planner_recovery_locks, &card).await);
        assert_eq!(pending(&fixture, &worker).await, 1);
        fixture
            .repo
            .db()
            .writer()
            .execute(
                sqlx::query("UPDATE worker_sessions SET thread_id='foreign-thread' WHERE id=?1")
                    .bind(&worker),
            )
            .await
            .unwrap();
        tokio::select! {
            _ = state.dispatcher.sweep_track_close_cleanups() => {},
            call = request(&mut peer) => panic!("changed identity must retain the obligation without Stop: {call}"),
        }
        assert_eq!(pending(&fixture, &worker).await, 1);
        fixture
            .repo
            .db()
            .writer()
            .execute(
                sqlx::query("UPDATE worker_sessions SET thread_id=?1 WHERE id=?2")
                    .bind(inline)
                    .bind(&worker),
            )
            .await
            .unwrap();
        state.dispatcher.poke_track_close_cleanups();
        let retry = request(&mut peer).await;
        assert_eq!(retry["method"], "turn/interrupt");
        assert_eq!(retry["params"]["threadId"], THREAD);
        send(&mut peer, json!({"id":retry["id"],"result":{}})).await;
        if active {
            notify_thread(
                &state.shared_codex_appserver,
                &mut peer,
                THREAD,
                "turn/completed",
                json!({"id":TURN,"status":"interrupted"}),
            )
            .await;
        }
        finish_cleanup(&fixture, &state, &worker, &mut peer).await;
        let after = fixture.carrier(&track).await.2;
        assert!(after.canonical_inputs.active.is_none());
        assert_eq!(
            after.canonical_inputs.attempts,
            before.canonical_inputs.attempts
        );
        fixture.shutdown_process(&state).await;
    }
}

#[tokio::test]
async fn close_p1_missing_blank_or_foreign_thread_refuses_the_whole_close() {
    for damage in ["missing", "blank", "foreign", "snapshot-foreign"] {
        let fixture = Fixture::new().await;
        let (state, app, cookie) = fixture.application().await;
        let (_client, mut peer) = connect(&state.shared_codex_appserver).await;
        let (track, _, worker) = running(&fixture, &state, &app, &cookie, &mut peer).await;
        notify_thread(
            &state.shared_codex_appserver,
            &mut peer,
            THREAD,
            "turn/completed",
            json!({"id":TURN,"status":"completed"}),
        )
        .await;
        let before = fixture
            .wait_phase(&track, HarnessPhaseTag::TurnCompleted)
            .await;
        let handle = state.harness.get(&worker).unwrap();
        assert!(handle.hibernate_if_idle().await.unwrap());
        assert!(state.harness.retire_if_same(&worker, &handle));
        let mut snapshot = serde_json::to_value(&before).unwrap();
        let inline = match damage {
            "missing" => {
                snapshot["last_thread_id"] = Value::Null;
                None
            }
            "blank" => {
                snapshot["last_thread_id"] = json!(" ");
                Some("")
            }
            "foreign" => Some("foreign-thread"),
            "snapshot-foreign" => {
                snapshot["last_thread_id"] = json!("foreign-thread");
                None
            }
            _ => unreachable!(),
        };
        fixture
            .repo
            .db()
            .writer()
            .execute(
                sqlx::query(
                    "UPDATE worker_sessions SET thread_id=?1,handle_state_json=?2 WHERE id=?3",
                )
                .bind(inline)
                .bind(snapshot.to_string())
                .bind(&worker),
            )
            .await
            .unwrap();
        // The public route must refuse atomically, before a Stop can address a foreign thread.
        assert_eq!(
            close_status(&app, &cookie, &track).await,
            StatusCode::CONFLICT,
            "{damage}"
        );
        assert!(
            fixture
                .repo
                .track_get(&track)
                .await
                .unwrap()
                .unwrap()
                .is_open()
        );
        assert_eq!(pending(&fixture, &worker).await, 0);
        assert_eq!(
            fixture.carrier(&track).await.2.canonical_inputs,
            before.canonical_inputs
        );
        fixture.shutdown_process(&state).await;
    }
}

#[path = "close_p1/historical.rs"]
mod historical;

#[tokio::test]
async fn close_p1_unaddressed_native_execution_refuses_without_replaying_input() {
    unaddressed_native_execution(false).await;
}

#[tokio::test]
async fn close_p1_unaddressed_new_execution_cannot_stop_the_previous_turn() {
    unaddressed_native_execution(true).await;
}

async fn unaddressed_native_execution(after_completed: bool) {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let (_client, mut peer) = connect(&state.shared_codex_appserver).await;
    let created = hibernation_tests::create_track(&app, &cookie).await;
    let track = created["id"].as_str().unwrap();
    let (card, worker, _) = fixture.carrier(track).await;
    let mut call = native_attempt_recovery_tests::initial_request(&mut peer).await;
    if after_completed {
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
        fixture
            .wait_phase(track, HarnessPhaseTag::TurnCompleted)
            .await;
        post(
            &app,
            &cookie,
            &format!("/api/cards/{card}/planner/input"),
            "unaddressed-next",
            json!({"text":"A new uncertain execution"}),
        )
        .await;
        call = request(&mut peer).await;
        assert_eq!(call["method"], "turn/start");
    }
    send(&mut peer, json!({"id":call["id"],"result":{"turn":{}}})).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let frame = fixture.carrier(track).await.2;
            if matches!(
                frame.canonical_inputs.attempts.last().unwrap().delivery,
                InputDeliveryFact::Unknown
            ) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let before = fixture.carrier(track).await.2;
    assert!(before.canonical_inputs.active.is_some());
    assert_eq!(
        close_status(&app, &cookie, track).await,
        StatusCode::CONFLICT
    );
    assert!(
        fixture
            .repo
            .track_get(track)
            .await
            .unwrap()
            .unwrap()
            .is_open()
    );
    assert_eq!(pending(&fixture, &worker).await, 0);
    assert_eq!(
        fixture.carrier(track).await.2.canonical_inputs,
        before.canonical_inputs
    );
    fixture.shutdown_process(&state).await;
}

#[path = "close_p1/installed_stop_tests.rs"]
mod installed_stop_tests;
