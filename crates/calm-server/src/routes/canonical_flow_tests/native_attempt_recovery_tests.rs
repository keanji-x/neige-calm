#![cfg(test)]
//! A fresh source cannot turn a real native attempt back into unattempted work.
use super::*;
use crate::db::sqlite::conversation::{
    StoredMessage, StoredMessageRevision, StoredSubmissionIdentity, StoredSubmissionPlan,
    message_tx, revision_tx, submission_identity_tx, submission_plan_tx,
};
use crate::shared_codex_appserver::native_owner_tests::Peer;
use axum::extract::FromRef;
use calm_types::conversation::{InputSubmissionId, MessageRevisionRef};

#[derive(Clone, Copy)]
enum NativeOutcome {
    Unknown,
    Accepted,
}

#[derive(Debug, PartialEq)]
struct OriginalFacts {
    message: StoredMessage,
    revision: StoredMessageRevision,
    submission: StoredSubmissionIdentity,
    plan: StoredSubmissionPlan,
    // The remaining immutable Turn fields complement submission's typed scope.
    turn: (String, String, String, i64),
}

async fn original_facts(
    fixture: &Fixture,
    message: &MessageRevisionRef,
    submission: &InputSubmissionId,
) -> OriginalFacts {
    let mut tx = fixture.repo.pool().begin().await.unwrap();
    let identity = submission_identity_tx(&mut tx, submission).await.unwrap();
    let facts = OriginalFacts {
        message: message_tx(&mut tx, &message.message_id).await.unwrap(),
        revision: revision_tx(&mut tx, message).await.unwrap(),
        plan: submission_plan_tx(&mut tx, submission).await.unwrap(),
        turn: sqlx::query_as(
            "SELECT initiating_runtime_id,initial_submission_id,execution_kind,created_at_ms
             FROM conversation_turns WHERE id=?1 AND card_id=?2",
        )
        .bind(identity.logical_turn_id.as_str())
        .bind(&identity.card_id)
        .fetch_one(&mut *tx)
        .await
        .unwrap(),
        submission: identity,
    };
    tx.rollback().await.unwrap();
    facts
}

pub(super) async fn initial_request(peer: &mut Peer) -> Value {
    let (mut threads, mut turns) = (0, 0);
    let call = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let call = request(peer).await;
            let result = match call["method"].as_str().unwrap() {
                "config/read" => json!({"config":{}}),
                "thread/start" => {
                    threads += 1;
                    json!({"thread":{"id":THREAD}})
                }
                "turn/start" => {
                    turns += 1;
                    break call;
                }
                method => panic!("unexpected original native activation: {method}"),
            };
            send(peer, json!({"id":call["id"],"result":result})).await;
        }
    })
    .await
    .expect("the original HTTP input must reach the real native transport");
    assert_eq!((threads, turns), (1, 1));
    assert_eq!(call["params"]["threadId"], THREAD);
    assert!(call["params"]["input"].to_string().contains(FIRST_BODY));
    call
}

async fn without_new_execution<T>(
    peer: &mut Peer,
    completed: impl std::future::Future<Output = T>,
) -> T {
    tokio::pin!(completed);
    loop {
        tokio::select! {
            result = &mut completed => return result,
            call = request(peer) => {
                assert_eq!(
                    call["method"], "config/read",
                    "a held attempt must issue zero thread/start, thread/read, thread/resume or turn/start requests: {call}"
                );
                send(peer, json!({"id":call["id"],"result":{"config":{}}})).await;
            }
        }
    }
}

async fn post_and_settle_wake(
    state: &AppState,
    app: &axum::Router,
    cookie: &str,
    peer: &mut Peer,
    card: &str,
    input: (&str, &str, Value),
) -> Value {
    let route = crate::state::RouteState::from_ref(state);
    // Hold the actual recovery drive so this HTTP wake owns its card fence
    // before we wait for that fence's release. No lifecycle state is injected.
    let drive = route.operation_runtime.lock_for_track_delete().await;
    let accepted = post(app, cookie, input.0, input.1, input.2).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let fence = crate::per_card_lock::lock_card(&route.planner_recovery_locks, card);
            tokio::pin!(fence);
            if futures_util::poll!(&mut fence).is_pending() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the actual HTTP wake must hold its original card fence");
    drop(drive);
    let released = crate::per_card_lock::lock_card(&route.planner_recovery_locks, card);
    let guard = tokio::time::timeout(
        Duration::from_secs(5),
        without_new_execution(peer, released),
    )
    .await
    .expect("the real cold wake must finish without a new native execution");
    drop(guard);
    // Also observe the installed scheduler, if any, across its normal tick.
    without_new_execution(peer, tokio::time::sleep(Duration::from_millis(150))).await;
    accepted
}

async fn shutdown(state: &AppState, worker: &str, peer: &mut Peer) {
    let Some(handle) = state.harness.remove(worker) else {
        return;
    };
    let mut stopping = tokio::spawn(async move { handle.shutdown().await });
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            tokio::select! {
                result = &mut stopping => break result.unwrap().unwrap(),
                call = request(peer) => {
                    assert_eq!(call["method"], "turn/interrupt");
                    assert_eq!(call["params"]["threadId"], THREAD);
                    assert_eq!(call["params"]["turnId"], TURN);
                    send(peer, json!({"id":call["id"],"result":{}})).await;
                }
            }
        }
    })
    .await
    .expect("normal shutdown must complete through its real native control");
}

async fn cold_attempt_is_not_readmitted(outcome: NativeOutcome) {
    let mut fixture = Fixture::new().await;
    let (state, app, old_cookie) = fixture.application().await;
    let area = post(
        &app,
        &old_cookie,
        "/api/areas",
        "area",
        json!({"name":"Original native attempt","color":"#123456"}),
    )
    .await;
    let (client, mut peer) = connect(&state.shared_codex_appserver).await;
    let create = json!({"area_id":area["id"],"title":"","planner_provider":"codex",
        "theme":{"fg":[255,255,255],"bg":[0,0,0]},"first_message":FIRST_BODY});
    let created = post(&app, &old_cookie, "/api/tracks", "first", create.clone()).await;
    let track = created["id"].as_str().unwrap();
    let call = initial_request(&mut peer).await;
    let (card, worker, crossing) = fixture.carrier(track).await;
    assert_eq!(crossing.canonical_inputs.attempts.len(), 1);
    assert!(matches!(
        crossing.canonical_inputs.attempts[0].delivery,
        InputDeliveryFact::CrossingUnknown
    ));
    assert!(crossing.canonical_inputs.pending.is_empty());
    let active = crossing.canonical_inputs.active.clone().unwrap();
    assert_eq!(active.submission_ids.len(), 1);
    let receipt = fixture.receipt(area["id"].as_str().unwrap()).await;
    let crate::db::sqlite::conversation::HumanTrackCreationAck::V1 { first_message, .. } =
        &receipt.acknowledgement;
    let original = original_facts(&fixture, first_message, &active.submission_ids[0]).await;
    assert_eq!(original.submission.logical_turn_id, active.logical_turn_id);
    assert_eq!(original.submission.card_id, card);
    match outcome {
        NativeOutcome::Unknown => {
            send(&mut peer, json!({"id":call["id"],"result":{"turn":{}}})).await;
        }
        NativeOutcome::Accepted => {
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
        }
    }
    let observed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let (_, _, frame) = fixture.carrier(track).await;
            let delivery = &frame.canonical_inputs.attempts[0].delivery;
            if match outcome {
                NativeOutcome::Unknown => matches!(delivery, InputDeliveryFact::Unknown),
                NativeOutcome::Accepted => {
                    matches!(delivery, InputDeliveryFact::Accepted { .. })
                        && frame.header.phase == HarnessPhaseTag::TurnRunning
                }
            } {
                break frame;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the real native reply must settle its original crossing marker");
    assert_eq!(observed.canonical_inputs.active.as_ref(), Some(&active));
    assert_eq!(
        observed.canonical_inputs.attempts[0].attempt,
        crossing.canonical_inputs.attempts[0].attempt
    );
    assert_eq!(fixture.counts(&card).await, (1, 1, 1, 1, 0));
    shutdown(&state, &worker, &mut peer).await;
    // Shutdown may legitimately record control intent or change phase. It
    // cannot finish the original work: no native END was delivered.
    assert_eq!(
        fixture.carrier(track).await.2.canonical_inputs,
        observed.canonical_inputs
    );
    peer.close(None).await.unwrap();
    fixture.shutdown_process(&state).await;
    drop((client, peer, state, app));
    fixture.repo.pool().close().await;
    fixture.repo = Arc::new(SqlxRepo::open(&fixture.url).await.unwrap());
    let (state, app, boot_cookie) = fixture.application().await;
    let (client, mut peer) = connect(&state.shared_codex_appserver).await;
    let boot = state.recover_harnesses_on_boot(BootRows::All);
    assert_eq!(
        tokio::time::timeout(
            Duration::from_secs(5),
            without_new_execution(&mut peer, boot)
        )
        .await
        .unwrap()
        .unwrap(),
        0
    );
    let cookie = super::source_readmission_tests::login_again(&app).await;
    assert_ne!(cookie, old_cookie);
    assert_ne!(cookie, boot_cookie);
    let replay = post(&app, &cookie, "/api/tracks", "first", create).await;
    // This replay has no pending head. Its source lookup can refuse before
    // taking the recovery drive; the later appended wake is fenced below.
    without_new_execution(&mut peer, tokio::time::sleep(Duration::from_millis(150))).await;
    assert_eq!(replay["id"], created["id"]);
    assert_eq!(
        fixture.carrier(track).await.2.canonical_inputs,
        observed.canonical_inputs
    );
    let run = super::presentation_read_feedback_tests::read_run(&app, &cookie, &card).await;
    let delivery = &run["active_input"]["submissions"][0];
    assert_eq!(delivery["carrier_id"], worker);
    assert_eq!(delivery["logical_turn_id"], active.logical_turn_id.as_str());
    assert_eq!(delivery["submission_id"], active.submission_ids[0].as_str());
    assert_eq!(
        delivery["attempt"]["attempt_id"],
        observed.canonical_inputs.attempts[0]
            .attempt
            .attempt_id
            .as_str()
    );
    assert_eq!(
        delivery["attempt"]["revision"],
        observed.canonical_inputs.attempts[0].revision.to_string()
    );
    assert_eq!(delivery["attempt"]["target"]["thread_id"], THREAD);
    let keep = match outcome {
        NativeOutcome::Unknown => json!({"kind":"available"}),
        NativeOutcome::Accepted => json!({"kind":"unavailable","reason":"not_held"}),
    };
    assert_eq!(delivery["attempt"]["actions"]["keep"], keep);
    let retry_reason = match outcome {
        NativeOutcome::Unknown => "invocation_not_fully_frozen",
        NativeOutcome::Accepted => "not_held",
    };
    assert_eq!(
        delivery["attempt"]["actions"]["retry"],
        json!({"kind":"unavailable","reason":retry_reason})
    );
    assert!(delivery["attempt"].get("resolution").unwrap().is_null());
    let appended = post_and_settle_wake(
        &state,
        &app,
        &cookie,
        &mut peer,
        &card,
        (
            &format!("/api/cards/{card}/planner/input"),
            "new-input",
            json!({"text":"Keep this newer input pending behind the original attempt"}),
        ),
    )
    .await;
    assert_eq!(appended["worker_session_id"], worker);
    assert_ne!(appended["entry_id"], first_message.message_id.as_str());
    let (after_card, after_worker, after) = fixture.carrier(track).await;
    assert_eq!((after_card, after_worker), (card.clone(), worker.clone()));
    assert_eq!(after.header.last_thread_id.as_deref(), Some(THREAD));
    assert_eq!(after.header.last_turn_id, observed.header.last_turn_id);
    assert_eq!(after.canonical_inputs.active.as_ref(), Some(&active));
    assert_eq!(
        after.canonical_inputs.attempts,
        observed.canonical_inputs.attempts
    );
    assert_eq!(after.canonical_inputs.pending.len(), 1);
    assert_eq!(
        after.canonical_inputs.pending[0]
            .message_id()
            .unwrap()
            .as_str(),
        appended["entry_id"].as_str().unwrap()
    );
    assert_eq!(
        original_facts(&fixture, first_message, &active.submission_ids[0]).await,
        original
    );
    assert_eq!(fixture.receipt(area["id"].as_str().unwrap()).await, receipt);
    assert_eq!(fixture.counts(&card).await, (2, 2, 1, 1, 0));
    let turns: i64 = sqlx::query_scalar("SELECT count(*) FROM conversation_turns WHERE card_id=?1")
        .bind(&card)
        .fetch_one(fixture.repo.pool())
        .await
        .unwrap();
    assert_eq!(turns, 1);
    shutdown(&state, &worker, &mut peer).await;
    peer.close(None).await.unwrap();
    fixture.shutdown_process(&state).await;
    drop((client, peer, state, app));
    fixture.repo.pool().close().await;
}

#[tokio::test]
async fn production_http_cold_login_preserves_unknown_attempt_without_resending() {
    cold_attempt_is_not_readmitted(NativeOutcome::Unknown).await;
}

#[tokio::test]
async fn production_http_cold_login_preserves_accepted_active_without_resending() {
    cold_attempt_is_not_readmitted(NativeOutcome::Accepted).await;
}

#[tokio::test]
async fn h2_canonical_remote_error_holds_claim_but_releases_unstarted_native_slot() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let area = post(
        &app,
        &cookie,
        "/api/areas",
        "area",
        json!({"name":"Refused native input","color":"#123456"}),
    )
    .await;
    let (_client, mut peer) = connect(&state.shared_codex_appserver).await;
    let created = post(
        &app,
        &cookie,
        "/api/tracks",
        "first",
        json!({"area_id":area["id"],"title":"","planner_provider":"codex",
            "theme":{"fg":[255,255,255],"bg":[0,0,0]},"first_message":FIRST_BODY}),
    )
    .await;
    let track = created["id"].as_str().unwrap();
    let call = initial_request(&mut peer).await;
    send(
        &mut peer,
        json!({"id":call["id"],"error":{"code":-32600,"message":"unknown model"}}),
    )
    .await;
    let (_, _, frame) = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let carrier = fixture.carrier(track).await;
            if !matches!(
                carrier.2.canonical_inputs.attempts[0].delivery,
                InputDeliveryFact::CrossingUnknown
            ) {
                break carrier;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the native response must settle the original crossing");
    // Correlation proves an error response, not rollback of input acceptance (#2843, P1).
    assert!(matches!(
        &frame.canonical_inputs.attempts[0].delivery,
        InputDeliveryFact::Unknown
    ));
    assert!(frame.canonical_inputs.active.is_some());
    assert!(frame.canonical_inputs.pending.is_empty());
    let starting = state.shared_codex_appserver.clone();
    let mut retry = tokio::spawn(async move {
        starting
            .turn_start(
                THREAD,
                vec![provider::InputItem::text("native-slot-probe")],
                &provider::TurnModelSelection {
                    model: Some("corrected-model".into()),
                    effort: None,
                },
                provider::codex::TurnApprovals::Explicit(
                    crate::planner_permission_mode::PlannerPermissionMode::Never,
                ),
                Some("retry"),
            )
            .await
    });
    let call = tokio::select! {
        call = request(&mut peer) => call,
        result = &mut retry => panic!("unstarted native slot was not cleaned: {result:?}"),
    };
    assert_eq!(call["method"], "turn/start");
    assert_eq!(call["params"]["model"], "corrected-model");
    send(
        &mut peer,
        json!({"id":call["id"],"result":{"turn":{"id":TURN}}}),
    )
    .await;
    assert_eq!(retry.await.unwrap().unwrap(), TURN);
    fixture.shutdown_process(&state).await;
    peer.close(None).await.unwrap();
}
