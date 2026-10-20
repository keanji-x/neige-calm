//! A default full Planner must retain existing Dispatcher work after Human END.
use super::*;
use crate::event::{EditAuthor, Event};
use crate::harness::profile::{HarnessProfile, PlannerBinding};
use crate::ids::{ActorId, CardId};
use crate::shared_codex_appserver::native_owner_tests::Peer;
use calm_types::conversation::PendingInput;

const REPORT_BODY: &str = "Preserve this actual User report update in the Planner workflow";
const FOLLOW_UP_TURN: &str = "dispatcher-report-follow-up-turn";

async fn read_report(app: &axum::Router, cookie: &str, track: &str) -> Value {
    let response = app
        .clone()
        .oneshot(
            Request::get(format!("/api/tracks/{track}/report"))
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let report: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(status, StatusCode::OK, "{report}");
    report
}

async fn shutdown(state: &AppState, worker: &str, peer: &mut Peer) -> bool {
    let handle = state.harness.remove(worker).unwrap();
    let mut stopping = tokio::spawn(async move { handle.shutdown().await });
    let mut delivered = false;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            tokio::select! {
                result = &mut stopping => break result.unwrap().unwrap(),
                call = request(peer) => {
                    match call["method"].as_str().unwrap() {
                        "config/read" => send(peer, json!({"id":call["id"],"result":{"config":{}}})).await,
                        "turn/start" => {
                            // Accepted report work may already own a native call
                            // when normal shutdown begins. Finish its real reply.
                            assert_eq!(call["params"]["threadId"], THREAD);
                            assert!(call["params"]["input"].to_string().contains(REPORT_BODY));
                            assert!(!call["params"]["input"].to_string().contains(FIRST_BODY));
                            send(peer, json!({"id":call["id"],"result":{"turn":{"id":FOLLOW_UP_TURN}}})).await;
                            notify_thread(&state.shared_codex_appserver, peer, THREAD,
                                "turn/started", json!({"id":FOLLOW_UP_TURN,"status":"inProgress"})).await;
                            notify_thread(&state.shared_codex_appserver, peer, THREAD,
                                "turn/completed", json!({"id":FOLLOW_UP_TURN,"status":"completed"})).await;
                            delivered = true;
                        }
                        "turn/interrupt" => {
                            assert_eq!(call["params"]["threadId"], THREAD);
                            send(peer, json!({"id":call["id"],"result":{}})).await;
                        }
                        method => panic!("unexpected native request during normal cleanup: {method}"),
                    }
                }
            }
        }
    })
    .await
    .expect("normal cleanup must finish before asserting the retained automatic work");
    delivered
}

#[tokio::test]
async fn production_http_default_planner_accepts_real_user_report_edit_from_dispatcher() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let area = post(
        &app,
        &cookie,
        "/api/areas",
        "area",
        json!({"name":"Default Planner workflow","color":"#123456"}),
    )
    .await;
    let (client, mut peer) = connect(&state.shared_codex_appserver).await;
    // No storage-contract override, template, cwd or role is supplied. This is
    // the ordinary blank UserArea first-message path a person creates in the UI.
    let created = post(
        &app,
        &cookie,
        "/api/tracks",
        "first",
        json!({
            "area_id":area["id"],"title":"","planner_provider":"codex",
            "theme":{"fg":[255,255,255],"bg":[0,0,0]},"first_message":FIRST_BODY
        }),
    )
    .await;
    let track = created["id"].as_str().unwrap();
    let (card, worker, initial) = fixture.carrier(track).await;
    assert_eq!(serde_json::to_value(&initial).unwrap()["schema_version"], 3);
    let stored_card = fixture.repo.card_get(&card).await.unwrap().unwrap();
    let role = fixture.repo.card_role_get(&card).await.unwrap().unwrap();
    assert_eq!(role, crate::model::CardRole::Planner);
    assert_eq!(
        PlannerBinding::from_card(&stored_card, role)
            .unwrap()
            .profile,
        HarnessProfile::Planner
    );
    let first = super::native_attempt_recovery_tests::initial_request(&mut peer).await;
    send(
        &mut peer,
        json!({"id":first["id"],"result":{"turn":{"id":TURN}}}),
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
    assert!(completed.canonical_inputs.active.is_none());
    assert!(completed.canonical_inputs.pending.is_empty());
    assert_eq!(completed.canonical_inputs.attempts.len(), 1);
    let original_receipt = fixture.receipt(area["id"].as_str().unwrap()).await;
    let card_id = CardId::from(card.clone());
    let cursor_before = state.dispatcher.push_cursor_for_test(&card_id);
    let report = read_report(&app, &cookie, track).await;
    let write = post(
        &app,
        &cookie,
        &format!("/api/tracks/{track}/report/blocks"),
        "report-edit",
        json!({"kind":"prose","markdown":REPORT_BODY,"payload":null,
            "ifDocRev":report["docRev"].as_u64().unwrap(),"position":null}),
    )
    .await;
    assert!(write["docRev"].as_u64().unwrap() > report["docRev"].as_u64().unwrap());
    let events = crate::db::RepoEventWrite::events_for_track(
        fixture.repo.as_ref(),
        track,
        &["track.report_edited"],
        None,
    )
    .await
    .unwrap();
    let event = events
        .into_iter()
        .find(|entry| {
            matches!(&entry.event,
        Event::TrackReportEdited { author: EditAuthor::User, body_after, .. }
            if body_after.contains(REPORT_BODY))
        })
        .expect("the real User write commits its original report event");
    assert_eq!(event.actor, ActorId::User);
    assert_eq!(event.scope.track_id().unwrap().as_str(), track);
    assert!(event.id > cursor_before);
    assert!(event.id > completed.header.push_watermark);
    // This is the original production replay door, with the actual persisted
    // event/id. It shares the live listener's per-track push lock and dedup;
    // no test converts an event to a queue entry or directly observes a harness.
    state
        .dispatcher
        .catch_up_push(track.into(), event.event.clone(), event.id)
        .await;
    let accepted = tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            let (_, current_worker, frame) = fixture.carrier(track).await;
            assert_eq!(current_worker, worker);
            if frame.canonical_inputs.pending.iter().any(|entry| matches!(entry,
                PendingInput::System { observation, envelope_id: Some(id), .. }
                    if *id == event.id && matches!(observation.observation(),
                        crate::harness::Observation::ReportEdited { body, .. } if body.contains(REPORT_BODY))))
            {
                break None;
            }
            tokio::select! {
                call = request(&mut peer) => match call["method"].as_str().unwrap() {
                    "config/read" => send(&mut peer, json!({"id":call["id"],"result":{"config":{}}})).await,
                    "turn/start" => {
                        assert_eq!(call["params"]["threadId"], THREAD);
                        assert!(call["params"]["input"].to_string().contains(REPORT_BODY));
                        assert!(!call["params"]["input"].to_string().contains(FIRST_BODY));
                        break Some(call);
                    }
                    method => panic!("the same Planner must retain automatic work without restarting: {method}"),
                },
                _ = tokio::time::sleep(Duration::from_millis(10)) => {},
            }
        }
    }).await;
    let mut delivered = accepted.is_ok();
    if let Ok(Some(call)) = accepted {
        send(
            &mut peer,
            json!({"id":call["id"],"result":{"turn":{"id":FOLLOW_UP_TURN}}}),
        )
        .await;
        notify_thread(
            &state.shared_codex_appserver,
            &mut peer,
            THREAD,
            "turn/started",
            json!({"id":FOLLOW_UP_TURN,"status":"inProgress"}),
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
            json!({"id":FOLLOW_UP_TURN,"status":"completed"}),
        )
        .await;
        fixture
            .wait_phase(track, HarnessPhaseTag::TurnCompleted)
            .await;
    }
    let (after_card, after_worker, after) = fixture.carrier(track).await;
    let cursor_after = state.dispatcher.push_cursor_for_test(&card_id);
    let receipt_after = fixture.receipt(area["id"].as_str().unwrap()).await;
    let persisted_event_count = crate::db::RepoEventWrite::events_for_track(
        fixture.repo.as_ref(),
        track,
        &["track.report_edited"],
        None,
    )
    .await
    .unwrap()
    .iter()
    .filter(|entry| entry.id == event.id)
    .count();
    delivered |= shutdown(&state, &worker, &mut peer).await;
    peer.close(None).await.unwrap();
    drop((client, peer, state, app));
    fixture.repo.pool().close().await;
    assert_eq!((after_card, after_worker), (card, worker));
    assert_eq!(after.header.last_thread_id.as_deref(), Some(THREAD));
    assert_eq!(receipt_after, original_receipt);
    assert_eq!(
        persisted_event_count, 1,
        "the original report event remains durable"
    );
    if !delivered {
        assert!(
            cursor_after < event.id,
            "an unaccepted report must remain replayable"
        );
        assert_eq!(after.canonical_inputs, completed.canonical_inputs);
    }
    assert!(
        delivered,
        concat!(
            "default Planner lost its real User report event {}: no pending input or next native turn; ",
            "Dispatcher cursor {}->{}, durable watermark {}, original worker unchanged"
        ),
        event.id, cursor_before, cursor_after, after.header.push_watermark
    );
    assert!(cursor_after >= event.id);
}
