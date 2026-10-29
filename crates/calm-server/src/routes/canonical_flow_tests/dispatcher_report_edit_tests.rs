//! A default full Planner must retain existing Dispatcher work after Human END.
use super::*;
use crate::event::{EditAuthor, Event};
use crate::harness::profile::{HarnessProfile, PlannerBinding};
use crate::ids::{ActorId, CardId};
use crate::shared_codex_appserver::native_owner_tests::Peer;
use calm_truth::db::RepoRead;
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
    let context = state.mcp_context();
    let report_payload: String = sqlx::query_scalar("SELECT r.payload FROM cards r JOIN cards p ON p.track_id=r.track_id WHERE p.session_id=? AND r.kind='track-report'")
        .bind(worker).fetch_one(context.sqlite_db.as_ref().unwrap().read_pool()).await.unwrap();
    let report: crate::track_report::TrackReportPayload =
        serde_json::from_str(&report_payload).unwrap();
    let block_id = report
        .blocks
        .unwrap()
        .into_iter()
        .find(|b| {
            b.payload["markdown"]
                .as_str()
                .is_some_and(|text| text.contains(REPORT_BODY))
        })
        .unwrap()
        .id;
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
                            assert!(call["params"]["input"].to_string().contains(&block_id));
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
    assert_eq!(serde_json::to_value(&initial).unwrap()["schema_version"], 5);
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
                        assert!(call["params"]["input"].to_string().contains(write["id"].as_str().unwrap()));
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

// Both creation shapes have Managed resources: attach_folder without cwd is
// deliberately distinct from borrowing an existing directory.
async fn g1_report_first(cold: bool, attach_folder: bool) {
    let mut fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let area = u8_canonical::area(&app, &cookie, "Report first").await;
    let mut body = u8_canonical::body(&area, "codex");
    body["title"] = json!("");
    body["attach_folder"] = json!(attach_folder);
    assert!(body.get("cwd").is_none());
    let created = post(&app, &cookie, "/api/tracks", "blank", body).await;
    let track = created["id"].as_str().unwrap();
    let (card, worker, empty) = fixture.carrier(track).await;
    let stored = fixture.repo.track_get(track).await.unwrap().unwrap();
    assert_eq!(
        stored.workspace.kind,
        crate::model::TrackWorkspaceKind::Managed
    );
    assert!(!std::path::Path::new(&stored.workspace.path).exists());
    let (client, mut peer) = connect(&state.shared_codex_appserver).await;
    state.dispatcher.reconcile_tick_for_test().await;
    no_execution(&mut peer).await;
    crate::shared_codex_appserver::native_owner_tests::assert_no_request(&mut peer).await;
    assert_eq!(
        fixture.carrier(track).await.2.canonical_inputs,
        empty.canonical_inputs
    );
    assert!(empty.canonical_inputs.pending.is_empty());
    assert!(empty.canonical_inputs.active.is_none());
    assert!(empty.canonical_inputs.attempts.is_empty());
    assert_eq!(fixture.counts(&card).await, (0, 0, 1, 0, 0));
    // Cold case accepts a real source with the backend unavailable, then reopens
    // the file DB before native activation. No synthetic pending/phase writes.
    if cold {
        peer.close(None).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while !client.is_closed() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
    let report = read_report(&app, &cookie, track).await;
    let write = post(
        &app,
        &cookie,
        &format!("/api/tracks/{track}/report/blocks"),
        "edit",
        json!({"kind":"prose","markdown":REPORT_BODY,"payload":null,
            "ifDocRev":report["docRev"],"position":null}),
    )
    .await;
    let event = crate::db::RepoEventWrite::events_for_track(
        fixture.repo.as_ref(),
        track,
        &["track.report_edited"],
        None,
    )
    .await
    .unwrap()
    .into_iter()
    .find(|entry| {
        matches!(&entry.event, Event::TrackReportEdited {author: EditAuthor::User, body_after, ..}
            if body_after.contains(REPORT_BODY))
    })
    .unwrap();
    assert_eq!(event.actor, ActorId::User);
    state
        .dispatcher
        .catch_up_push(track.into(), event.event.clone(), event.id)
        .await;
    let (state, app, cookie, mut peer) = if cold {
        let pending = fixture.carrier(track).await.2;
        assert!(matches!(pending.canonical_inputs.pending.as_slice(),
            [PendingInput::System { envelope_id: Some(id), observation, .. }]
                if *id == event.id && matches!(observation.observation(),
                    crate::harness::Observation::ReportEdited {body, ..} if body.contains(REPORT_BODY))));
        assert!(pending.canonical_inputs.active.is_none());
        assert!(pending.canonical_inputs.attempts.is_empty());
        fixture.shutdown_process(&state).await;
        state
            .retire_dispatch_and_stop_provider_sweep()
            .await
            .unwrap();
        crate::state::WorkerState::from_ref(&state)
            .repo
            .sqlite_db()
            .unwrap()
            .close()
            .await;
        drop((state, app, cookie));
        fixture.repo.db().close().await;
        fixture.repo = Arc::new(SqlxRepo::open(&fixture.url).await.unwrap());
        let (state, app, cookie) = fixture.application().await;
        assert_eq!(
            fixture.carrier(track).await.2.canonical_inputs,
            pending.canonical_inputs
        );
        let (_client, peer) = connect(&state.shared_codex_appserver).await;
        state.dispatcher.reconcile_tick_for_test().await;
        (state, app, cookie, peer)
    } else {
        (state, app, cookie, peer)
    };
    let first = next_report_turn(&mut peer, true).await;
    let (same_card, same_worker, crossing) = fixture.carrier(track).await;
    assert_eq!((&same_card, &same_worker), (&card, &worker));
    assert_eq!(crossing.canonical_inputs.attempts.len(), 1);
    let submission = &crossing.canonical_inputs.attempts[0].attempt.submission_id;
    assert_eq!(first["params"]["clientUserMessageId"], submission.as_str());
    let mut tx = fixture.repo.pool().begin().await.unwrap();
    let plan = crate::db::sqlite::conversation::submission_plan_tx(&mut tx, submission)
        .await
        .unwrap();
    let refs = plan.plan.input_refs().unwrap();
    assert!(matches!(
        refs.as_slice(),
        [calm_types::conversation::AcceptedInputRef::System { .. }]
    ));
    let original: (i64, String) = sqlx::query_as("SELECT envelope_id,observation_id FROM conversation_submission_inputs WHERE submission_id=? AND effective=1")
        .bind(submission.as_str()).fetch_one(&mut *tx).await.unwrap();
    assert_eq!(original.0, event.id);
    assert_eq!(
        refs[0],
        calm_types::conversation::AcceptedInputRef::System {
            observation_id: calm_types::conversation::SystemObservationId::parse(&original.1)
                .unwrap()
        }
    );
    tx.rollback().await.unwrap();
    assert_eq!(first["params"]["threadId"], THREAD);
    assert!(
        first["params"]["input"]
            .to_string()
            .contains(write["id"].as_str().unwrap())
    );
    assert!(!first["params"]["input"].to_string().contains(FIRST_BODY));
    assert!(first["params"]["input"].to_string().contains("report.md"));
    assert!(
        !first["params"]["input"]
            .to_string()
            .contains(crate::routes::today_summary::TODAY_SUMMARY_BOOTSTRAP_TEXT)
    );
    ack_turn(&fixture, &state, &mut peer, track, &first, "report-first").await;
    let uri = format!("/api/cards/{card}/planner/input");
    let human = json!({"text":FIRST_BODY});
    let ack = post(&app, &cookie, &uri, "human", human.clone()).await;
    assert_eq!(ack["worker_session_id"], worker);
    for _ in 0..2 {
        state
            .dispatcher
            .catch_up_push(track.into(), event.event.clone(), event.id)
            .await;
        state.dispatcher.reconcile_tick_for_test().await;
        assert_eq!(post(&app, &cookie, &uri, "human", human.clone()).await, ack);
    }
    let queued = fixture.carrier(track).await.2;
    assert_eq!(queued.canonical_inputs.attempts.len(), 1);
    assert!(matches!(queued.canonical_inputs.pending.as_slice(),
        [PendingInput::Human { message, .. }] if message.message_id.as_str() == ack["entry_id"].as_str().unwrap()));
    no_execution(&mut peer).await;
    crate::shared_codex_appserver::native_owner_tests::assert_no_request(&mut peer).await;
    notify_thread(
        &state.shared_codex_appserver,
        &mut peer,
        THREAD,
        "turn/completed",
        json!({"id":"report-first","status":"completed"}),
    )
    .await;
    let second = next_report_turn(&mut peer, false).await;
    assert!(second["params"]["input"].to_string().contains(FIRST_BODY));
    // Report suggestions remain unread context until an actual read; the
    // precise Human-only plan below proves the event itself is not resubmitted.
    assert!(
        !second["params"]["input"]
            .to_string()
            .contains(crate::routes::today_summary::TODAY_SUMMARY_BOOTSTRAP_TEXT)
    );
    ack_turn(&fixture, &state, &mut peer, track, &second, "human-next").await;
    let running = fixture.carrier(track).await.2;
    let mut tx = fixture.repo.pool().begin().await.unwrap();
    let human_plan = crate::db::sqlite::conversation::submission_plan_tx(
        &mut tx,
        &running.canonical_inputs.attempts[1].attempt.submission_id,
    )
    .await
    .unwrap();
    assert_eq!(
        human_plan.plan.input_refs().unwrap(),
        vec![
            queued.canonical_inputs.pending[0]
                .accepted_input_ref()
                .unwrap()
        ]
    );
    tx.rollback().await.unwrap();
    notify_thread(
        &state.shared_codex_appserver,
        &mut peer,
        THREAD,
        "turn/completed",
        json!({"id":"human-next","status":"completed"}),
    )
    .await;
    let done = fixture
        .wait_phase(track, HarnessPhaseTag::TurnCompleted)
        .await;
    assert!(done.canonical_inputs.pending.is_empty());
    assert!(done.canonical_inputs.active.is_none());
    assert_eq!(done.canonical_inputs.attempts.len(), 2);
    assert_eq!(
        done.canonical_inputs.attempts[0].attempt.submission_id,
        *submission
    );
    assert_ne!(second["params"]["clientUserMessageId"], submission.as_str());
    for _ in 0..2 {
        state
            .dispatcher
            .catch_up_push(track.into(), event.event.clone(), event.id)
            .await;
        state.dispatcher.reconcile_tick_for_test().await;
    }
    no_execution(&mut peer).await;
    crate::shared_codex_appserver::native_owner_tests::assert_no_request(&mut peer).await;
    assert_eq!(
        fixture.carrier(track).await,
        (card.clone(), worker.clone(), done)
    );
    assert_eq!(fixture.counts(&card).await, (1, 1, 1, 2, 0));
    assert!(!shutdown(&state, &worker, &mut peer).await);
    fixture.shutdown_process(&state).await;
}

async fn no_execution(peer: &mut Peer) {
    let observed = tokio::time::timeout(Duration::from_millis(150), async {
        loop {
            let call = request(peer).await;
            assert_eq!(
                call["method"], "config/read",
                "unexpected native request: {call}"
            );
            send(peer, json!({"id":call["id"],"result":{"config":{}}})).await;
        }
    })
    .await;
    assert!(observed.is_err());
}

async fn ack_turn(
    fixture: &Fixture,
    state: &AppState,
    peer: &mut Peer,
    track: &str,
    call: &Value,
    turn: &str,
) {
    assert_eq!(call["params"]["threadId"], THREAD);
    send(peer, json!({"id":call["id"],"result":{"turn":{"id":turn}}})).await;
    notify_thread(
        &state.shared_codex_appserver,
        peer,
        THREAD,
        "turn/started",
        json!({"id":turn,"status":"inProgress"}),
    )
    .await;
    let running = fixture
        .wait_phase(track, HarnessPhaseTag::TurnRunning)
        .await;
    let attempt = running.canonical_inputs.attempts.last().unwrap();
    assert_eq!(
        call["params"]["clientUserMessageId"],
        attempt.attempt.submission_id.as_str()
    );
    assert!(matches!(&attempt.delivery, InputDeliveryFact::Accepted {
        evidence: calm_types::conversation::NativeInputAcceptance::NativeTurn {native_turn_id}, ..}
        if native_turn_id == turn));
}

#[tokio::test]
async fn g1_http_first_system_report_then_human_live() {
    for attach_folder in [false, true] {
        g1_report_first(false, attach_folder).await;
    }
}

#[tokio::test]
async fn g1_http_first_system_report_then_human_cold() {
    for attach_folder in [false, true] {
        g1_report_first(true, attach_folder).await;
    }
}

async fn next_report_turn(peer: &mut Peer, initial: bool) -> Value {
    let mut threads = 0;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let call = request(peer).await;
            match call["method"].as_str().unwrap() {
                "config/read" => send(peer, json!({"id":call["id"],"result":{"config":{}}})).await,
                "thread/start" if initial => {
                    threads += 1;
                    assert_eq!(threads, 1);
                    send(
                        peer,
                        json!({"id":call["id"],"result":{"thread":{"id":THREAD}}}),
                    )
                    .await;
                }
                "turn/start" => {
                    assert_eq!(threads, usize::from(initial));
                    return call;
                }
                method => panic!("unexpected report continuation request: {method}"),
            }
        }
    })
    .await
    .unwrap()
}
