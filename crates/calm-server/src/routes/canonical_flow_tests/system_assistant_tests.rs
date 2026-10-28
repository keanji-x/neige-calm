//! Complete summary turns over production HTTP/boot and a native protocol peer.
use super::*;
use crate::routes::today_summary::TODAY_SUMMARY_BOOTSTRAP_TEXT as BOOTSTRAP;
use crate::shared_codex_appserver::native_owner_tests::Peer;

async fn activity(app: &axum::Router, cookie: &str) -> String {
    let track = assistant_canonical::track(app, cookie).await;
    edit(app, cookie, &track, 0).await;
    track
}
async fn edit(app: &axum::Router, cookie: &str, track: &str, revision: i64) {
    post(app, cookie, &format!("/api/tracks/{track}/report"), "report", json!({"ifDocRev":revision,"summary":"Real report edit","body":format!("# Activity {revision}\n")})).await;
}
async fn summary(app: &axum::Router, cookie: &str) -> Value {
    tokio::time::timeout(
        Duration::from_secs(5),
        post(
            app,
            cookie,
            "/api/today/summary",
            "same-http-key",
            json!(null),
        ),
    )
    .await
    .expect("acceptance does not wait on native effects")
}
fn body(edits: u32) -> String {
    // Independent activity oracle: the fixture has one user Track and only report edits.
    include_str!("../../../prompts/today-summary/write.md").replace("{counts}", &format!("- report edits: {edits}\n- tasks completed: 0\n- tasks failed: 0\n- distinct tracks touched: 1\n"))
}
async fn frame(fixture: &Fixture, card: &str) -> (String, CanonicalHarnessSnapshot) {
    assistant_canonical::carrier(fixture, card).await
}
async fn phase(
    fixture: &Fixture,
    card: &str,
    expected: HarnessPhaseTag,
) -> CanonicalHarnessSnapshot {
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let (_, frame) = frame(fixture, card).await;
            if frame.header.phase == expected {
                break frame;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    match result {
        Ok(frame) => frame,
        Err(error) => {
            let (worker, frame) = frame(fixture, card).await;
            panic!(
                "Assistant phase wait {expected:?}: {error}; worker={worker} header={:?} inputs={:?}",
                frame.header, frame.canonical_inputs
            );
        }
    }
}
async fn next(peer: &mut Peer, initial: bool) -> Value {
    let mut threads = 0;
    loop {
        let call = request(peer).await;
        let reply = match call["method"].as_str().unwrap() {
            "config/read" => json!({"config":{}}),
            "thread/start" if initial => {
                threads += 1;
                json!({"thread":{"id":THREAD}})
            }
            "turn/start" => {
                assert_eq!(threads, usize::from(initial));
                return call;
            }
            method => panic!("unexpected summary native request {method}"),
        };
        send(peer, json!({"id":call["id"],"result":reply})).await;
    }
}
struct Native<'a> {
    fixture: &'a Fixture,
    state: &'a AppState,
    card: &'a str,
}
impl Native<'_> {
    async fn ack(&self, peer: &mut Peer, call: Value, expected: &str, turn: &str) {
        let (fixture, state, card) = (self.fixture, self.state, self.card);
        assert_eq!(call["params"]["threadId"], THREAD);
        let texts = call["params"]["input"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|item| item["text"].as_str())
            .collect::<Vec<_>>();
        assert!(
            texts
                .iter()
                .any(|text| *text == expected || text.ends_with(&format!("\n\n{expected}"))),
            "complete frozen body must cross native boundary: {texts:?}"
        );
        assert_eq!(
            texts
                .iter()
                .map(|text| text.matches(expected).count())
                .sum::<usize>(),
            1
        );
        if expected != BOOTSTRAP {
            assert!(texts.iter().all(|text| !text.contains(BOOTSTRAP)));
        }
        let (worker, crossing) = frame(fixture, card).await;
        let original_attempt = crossing.canonical_inputs.attempts.last().unwrap();
        assert_eq!(
            original_attempt.delivery,
            InputDeliveryFact::CrossingUnknown
        );
        assert_eq!(original_attempt.correlation.thread_id, THREAD);
        assert_eq!(
            call["params"]["clientUserMessageId"],
            original_attempt.attempt.submission_id.as_str()
        );
        let claimed: (String, String) = sqlx::query_as(
        "SELECT json_extract(r.receipt_json,'$.originalWorker'),r.observation_id FROM conversation_submission_inputs i JOIN conversation_system_assistant_receipts r ON r.observation_id=i.observation_id AND r.event_id=i.envelope_id WHERE i.submission_id=?1 AND i.effective=1"
    ).bind(original_attempt.attempt.submission_id.as_str()).fetch_one(fixture.repo.pool()).await.unwrap();
        assert_eq!(claimed.0, worker);
        let mut tx = fixture.repo.pool().begin().await.unwrap();
        let plan = crate::db::sqlite::conversation::submission_plan_tx(
            &mut tx,
            &original_attempt.attempt.submission_id,
        )
        .await
        .unwrap();
        assert_eq!(
            plan.plan.input_refs().unwrap(),
            vec![calm_types::conversation::AcceptedInputRef::System {
                observation_id: calm_types::conversation::SystemObservationId::parse(&claimed.1)
                    .unwrap(),
            }]
        );
        let identity = crate::db::sqlite::conversation::submission_identity_tx(
            &mut tx,
            &original_attempt.attempt.submission_id,
        )
        .await
        .unwrap();
        assert_eq!(identity.card_id, card);
        assert!(matches!(
            identity.execution,
            calm_types::conversation::ConversationExecutionProvenance::LegacySingleUserDeployment {
                provider: crate::session_projection_repo::AgentProvider::Codex,
                profile: crate::harness::profile::HarnessProfile::Assistant,
                ..
            }
        ));
        tx.rollback().await.unwrap();

        assert!(
            matches!(crossing.canonical_inputs.active.as_ref().unwrap().submission_ids.as_slice(), [id] if id == &original_attempt.attempt.submission_id)
        );
        send(peer, json!({"id":call["id"],"result":{"turn":{"id":turn}}})).await;
        notify_thread(
            &state.shared_codex_appserver,
            peer,
            THREAD,
            "turn/started",
            json!({"id":turn,"status":"inProgress"}),
        )
        .await;
        let running = phase(fixture, card, HarnessPhaseTag::TurnRunning).await;
        let attempt = running.canonical_inputs.attempts.last().unwrap();
        assert!(
            matches!(&attempt.delivery, InputDeliveryFact::Accepted { evidence: calm_types::conversation::NativeInputAcceptance::NativeTurn { native_turn_id }, .. } if native_turn_id == turn)
        );
        assert_eq!(
            call["params"]["clientUserMessageId"],
            attempt.attempt.submission_id.as_str()
        );
    }

    async fn complete(&self, peer: &mut Peer, call: Value, expected: &str, turn: &str) {
        let state = self.state;
        self.ack(peer, call, expected, turn).await;
        notify_thread(
            &state.shared_codex_appserver,
            peer,
            THREAD,
            "turn/completed",
            json!({"id":turn,"status":"completed"}),
        )
        .await;
    }
}

#[tokio::test]
async fn summary_post_runs_bootstrap_end_body_and_later_post() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let track = activity(&app, &cookie).await;
    let accepted = summary(&app, &cookie).await;
    let card = accepted["card_id"].as_str().unwrap();
    let (worker, before) = frame(&fixture, card).await;
    assert_eq!(before.canonical_inputs.pending.len(), 2);
    assert_eq!(fixture.counts(card).await.0, 0);
    let (_client, mut peer) = connect(&state.shared_codex_appserver).await;
    state.dispatcher.reconcile_tick_for_test().await;
    let native = Native {
        fixture: &fixture,
        state: &state,
        card,
    };
    let first = next(&mut peer, true).await;
    // Change activity after POST but before bootstrap END: summary1 must stay frozen.
    edit(&app, &cookie, &track, 1).await;
    native
        .complete(&mut peer, first, BOOTSTRAP, "bootstrap")
        .await;
    let first_body = next(&mut peer, false).await;
    native
        .complete(&mut peer, first_body, &body(1), "summary1")
        .await;
    phase(&fixture, card, HarnessPhaseTag::TurnCompleted).await;
    let later = summary(&app, &cookie).await;
    assert_eq!(later, accepted);
    let later_body = next(&mut peer, false).await;
    native
        .complete(&mut peer, later_body, &body(2), "summary2")
        .await;
    let final_frame = phase(&fixture, card, HarnessPhaseTag::TurnCompleted).await;
    assert_eq!(frame(&fixture, card).await.0, worker);
    assert!(final_frame.canonical_inputs.pending.is_empty());
    assert_eq!(final_frame.canonical_inputs.attempts.len(), 3);
    shutdown(&fixture, &state).await;
}

#[tokio::test]
async fn summary_post_accepts_original_system_bootstrap_and_frozen_body() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    activity(&app, &cookie).await;
    let summary = tokio::time::timeout(
        Duration::from_secs(5),
        post(
            &app,
            &cookie,
            "/api/today/summary",
            "same-http-key",
            json!(null),
        ),
    )
    .await
    .expect("acceptance does not wait on a provider");
    let card = summary["card_id"].as_str().unwrap();
    let receipts: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM conversation_system_assistant_receipts WHERE card_id=?1",
    )
    .bind(card)
    .fetch_one(fixture.repo.pool())
    .await
    .unwrap();
    assert_eq!(
        receipts, 2,
        "bootstrap and frozen summary use canonical System acceptance"
    );
    shutdown(&fixture, &state).await;
}

mod prefix;
mod recovery;

mod authority;

#[tokio::test]
async fn concurrent_http_posts_on_independent_connections_execute_one_bootstrap_and_both_bodies() {
    let mut fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    activity(&app, &cookie).await;
    post(
        &app,
        &cookie,
        "/api/today/launchpad/ensure",
        "ensure",
        json!(null),
    )
    .await;
    fixture.data_dir = Some(fixture.root.path().join("peer-data"));
    let (other_state, other_app, other_cookie) = fixture.application().await;
    let (a, b) = tokio::join!(summary(&app, &cookie), summary(&other_app, &other_cookie));
    assert_eq!(a, b);
    let card = a["card_id"].as_str().unwrap();
    let (worker, before) = frame(&fixture, card).await;
    assert_eq!(before.canonical_inputs.pending.len(), 3);
    let receipts = recovery::receipts(&fixture, card).await;
    assert_eq!(receipts.len(), 3);
    assert_ne!(receipts[1].subject, receipts[2].subject);
    assert!(
        receipts
            .iter()
            .all(|receipt| receipt.original_worker == worker)
    );
    // Retire the second producer; the first reconstructs sources from the same SQL facts.
    other_state.dispatcher.retire();
    let (_client, mut peer) = connect(&state.shared_codex_appserver).await;
    state.dispatcher.reconcile_tick_for_test().await;
    let native = Native {
        fixture: &fixture,
        state: &state,
        card,
    };
    let bootstrap = next(&mut peer, true).await;
    native
        .complete(&mut peer, bootstrap, BOOTSTRAP, "concurrent-bootstrap")
        .await;
    for turn in ["concurrent-one", "concurrent-two"] {
        let call = next(&mut peer, false).await;
        native.complete(&mut peer, call, &body(1), turn).await;
    }
    let after = phase(&fixture, card, HarnessPhaseTag::TurnCompleted).await;
    assert!(after.canonical_inputs.pending.is_empty());
    assert_eq!(after.canonical_inputs.attempts.len(), 3);
    assert_eq!(frame(&fixture, card).await.0, worker);
    assert_eq!(recovery::receipts(&fixture, card).await, receipts);
    assert_eq!(fixture.counts(card).await.2, 1);
    shutdown(&fixture, &state).await;
    shutdown(&fixture, &other_state).await;
}

#[tokio::test]
async fn end_preserves_summary_accepted_while_original_body_is_running() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    activity(&app, &cookie).await;
    let accepted = summary(&app, &cookie).await;
    let card = accepted["card_id"].as_str().unwrap();
    let (_client, mut peer) = connect(&state.shared_codex_appserver).await;
    state.dispatcher.reconcile_tick_for_test().await;
    let native = Native {
        fixture: &fixture,
        state: &state,
        card,
    };
    let bootstrap = next(&mut peer, true).await;
    native
        .complete(&mut peer, bootstrap, BOOTSTRAP, "merge-bootstrap")
        .await;
    let first_body = next(&mut peer, false).await;
    native
        .ack(&mut peer, first_body, &body(1), "merge-first")
        .await;
    assert_eq!(summary(&app, &cookie).await, accepted);
    let receipts = recovery::receipts(&fixture, card).await;
    assert_eq!(receipts.len(), 3);
    notify_thread(
        &state.shared_codex_appserver,
        &mut peer,
        THREAD,
        "turn/completed",
        json!({"id":"merge-first","status":"completed"}),
    )
    .await;
    let second_body = next(&mut peer, false).await;
    native
        .complete(&mut peer, second_body, &body(1), "merge-second")
        .await;
    let after = phase(&fixture, card, HarnessPhaseTag::TurnCompleted).await;
    assert!(after.canonical_inputs.pending.is_empty());
    assert_eq!(after.canonical_inputs.attempts.len(), 3);
    assert_eq!(
        after.header.push_watermark,
        receipts.last().unwrap().event_id
    );
    assert_eq!(recovery::receipts(&fixture, card).await, receipts);
    shutdown(&fixture, &state).await;
}
